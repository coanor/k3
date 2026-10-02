[CmdletBinding()]
param(
    [ValidateSet("gpu", "cpu")]
    [string]$Backend = "gpu",
    [string]$VenvPath,
    [string]$PythonVersion = "3.11",
    [string]$ConfigPath
)

$ErrorActionPreference = "Stop"
Set-StrictMode -Version Latest

if ([string]::IsNullOrWhiteSpace($VenvPath)) {
    $VenvPath = Join-Path $PSScriptRoot ".venv-separator"
}
if ([string]::IsNullOrWhiteSpace($ConfigPath)) {
    $ConfigPath = Join-Path $PSScriptRoot "config.json"
}

$runtimeCache = Join-Path $PSScriptRoot ".cache"
$tempDir = Join-Path $runtimeCache "temp"
New-Item -ItemType Directory -Force -Path $tempDir | Out-Null
$env:TEMP = $tempDir
$env:TMP = $tempDir
if ([string]::IsNullOrWhiteSpace($env:PIP_CACHE_DIR)) {
    $env:PIP_CACHE_DIR = Join-Path $runtimeCache "pip"
}

function Invoke-Checked {
    param(
        [Parameter(Mandatory = $true)][string]$Executable,
        [Parameter(Mandatory = $true)][string[]]$Arguments
    )

    & $Executable @Arguments
    if ($LASTEXITCODE -ne 0) {
        throw "命令执行失败（退出码 $LASTEXITCODE）：$Executable $($Arguments -join ' ')"
    }
}

function Set-JsonProperty {
    param(
        [Parameter(Mandatory = $true)][object]$Object,
        [Parameter(Mandatory = $true)][string]$Name,
        [AllowNull()][object]$Value
    )

    if ($Object.PSObject.Properties.Name -contains $Name) {
        $Object.$Name = $Value
    } else {
        $Object | Add-Member -MemberType NoteProperty -Name $Name -Value $Value
    }
}

$separatorRoot = Join-Path $PSScriptRoot "python\separator"
$requirements = Join-Path $separatorRoot "requirements-runtime.txt"
if (-not (Test-Path -LiteralPath (Join-Path $separatorRoot "pyproject.toml") -PathType Leaf)) {
    throw "找不到 Windows 分离包源码：$separatorRoot"
}

$pythonLauncher = Get-Command "py.exe" -ErrorAction SilentlyContinue
if ($null -eq $pythonLauncher) {
    throw "找不到 Python Launcher。请先安装 64 位 Python $PythonVersion。"
}

$VenvPath = [IO.Path]::GetFullPath($VenvPath)
$pythonSelector = "-$PythonVersion"
Write-Host "正在创建 Windows 分离环境：$VenvPath"
Invoke-Checked -Executable $pythonLauncher.Source -Arguments @(
    $pythonSelector, "-m", "venv", $VenvPath
)

$venvPython = Join-Path $VenvPath "Scripts\python.exe"
$worker = Join-Path $VenvPath "Scripts\k3-separator.exe"
if (-not (Test-Path -LiteralPath $venvPython -PathType Leaf)) {
    throw "虚拟环境创建后未找到 Python：$venvPython"
}

Invoke-Checked -Executable $venvPython -Arguments @(
    "-m", "pip", "install", "--upgrade", "pip", "setuptools<82", "wheel"
)

$torchIndex = if ($Backend -eq "gpu") {
    "https://download.pytorch.org/whl/cu128"
} else {
    "https://download.pytorch.org/whl/cpu"
}
Invoke-Checked -Executable $venvPython -Arguments @(
    "-m", "pip", "install",
    "torch==2.11.0", "torchvision==0.26.0", "torchaudio==2.11.0",
    "--index-url", $torchIndex
)

$separatorExtra = "audio-separator[$Backend]>=0.44.5,<0.45"
Invoke-Checked -Executable $venvPython -Arguments @(
    "-m", "pip", "install", $separatorExtra, "--no-deps"
)
$onnxRuntime = if ($Backend -eq "gpu") { "onnxruntime-gpu>=1.17" } else { "onnxruntime>=1.17" }
Invoke-Checked -Executable $venvPython -Arguments @(
    "-m", "pip", "install", "-r", $requirements, $onnxRuntime, "diffq-fixed>=0.2"
)
Invoke-Checked -Executable $venvPython -Arguments @(
    "-m", "pip", "install", "--force-reinstall", "--no-deps", $separatorRoot
)

$runtimeCheck = @'
import json
import onnxruntime as ort
import torch
from audio_separator.separator import Separator

result = {
    "torch": torch.__version__,
    "cuda_available": bool(torch.cuda.is_available()),
    "device": torch.cuda.get_device_name(0) if torch.cuda.is_available() else None,
    "onnx_providers": ort.get_available_providers(),
}
print(json.dumps(result, ensure_ascii=False))
'@
$runtime = $runtimeCheck | & $venvPython -
if ($LASTEXITCODE -ne 0) {
    throw "Windows 分离 runtime 健康检查失败。"
}
$runtimeStatus = $runtime | Select-Object -Last 1 | ConvertFrom-Json
if ($Backend -eq "gpu" -and -not $runtimeStatus.cuda_available) {
    throw "已安装 GPU worker，但 PyTorch 无法使用 CUDA。"
}

$modelDir = if (-not [string]::IsNullOrWhiteSpace($env:K3_MODEL_DIR)) {
    [IO.Path]::GetFullPath($env:K3_MODEL_DIR)
} else {
    Join-Path $PSScriptRoot "models"
}
$logDir = if (-not [string]::IsNullOrWhiteSpace($env:K3_LOG_DIR)) {
    [IO.Path]::GetFullPath($env:K3_LOG_DIR)
} else {
    Join-Path $PSScriptRoot "logs"
}
New-Item -ItemType Directory -Force -Path $modelDir, $logDir | Out-Null
$env:TORCH_HOME = Join-Path $modelDir "torch"
$env:HF_HOME = Join-Path $modelDir "huggingface"
$healthText = '{"id":"health","method":"health"}' |
    & $worker --model-dir $modelDir
if ($LASTEXITCODE -ne 0) {
    throw "k3-separator.exe 健康检查执行失败。"
}
$health = $healthText | ConvertFrom-Json
if (-not $health.ok -or -not $health.result.runtime.audio_separator_installed) {
    throw "k3-separator.exe 健康检查未通过：$healthText"
}

if (Test-Path -LiteralPath $ConfigPath -PathType Leaf) {
    $config = Get-Content -LiteralPath $ConfigPath -Raw -Encoding UTF8 | ConvertFrom-Json
    if (-not ($config.PSObject.Properties.Name -contains "separation")) {
        $config | Add-Member -MemberType NoteProperty -Name "separation" -Value ([PSCustomObject]@{})
    }
    Set-JsonProperty -Object $config.separation -Name "worker" -Value $worker
    Set-JsonProperty -Object $config.separation -Name "model_dir" -Value $modelDir
    Set-JsonProperty -Object $config.separation -Name "log_dir" -Value $logDir
    $json = $config | ConvertTo-Json -Depth 10
    [IO.File]::WriteAllText(
        [IO.Path]::GetFullPath($ConfigPath),
        $json + [Environment]::NewLine,
        [Text.UTF8Encoding]::new($false)
    )
    Write-Host "已更新配置：$ConfigPath"
}

Write-Host "Windows 分离 worker 已就绪：$worker"
Write-Host "CUDA 可用：$($runtimeStatus.cuda_available)"
Write-Host "模型目录：$modelDir"
