[CmdletBinding()]
param(
    [ValidateSet("auto", "gpu", "cpu")]
    [string]$Backend = "auto",
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
        throw "Command failed (exit code $LASTEXITCODE): $Executable $($Arguments -join ' ')"
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
    throw "Windows separator sources not found: $separatorRoot"
}

$pythonLauncher = Get-Command "py.exe" -ErrorAction SilentlyContinue
if ($null -eq $pythonLauncher) {
    throw "Python Launcher not found. Install 64-bit Python $PythonVersion first."
}

$VenvPath = [IO.Path]::GetFullPath($VenvPath)
$pythonSelector = "-$PythonVersion"
Write-Host "Creating Windows separation runtime: $VenvPath"
Invoke-Checked -Executable $pythonLauncher.Source -Arguments @(
    $pythonSelector, "-m", "venv", $VenvPath
)

$venvPython = Join-Path $VenvPath "Scripts\python.exe"
$worker = Join-Path $VenvPath "Scripts\k3-separator.exe"
if (-not (Test-Path -LiteralPath $venvPython -PathType Leaf)) {
    throw "Python not found after creating the virtual environment: $venvPython"
}

Invoke-Checked -Executable $venvPython -Arguments @(
    "-m", "pip", "install", "--upgrade", "pip", "setuptools<82", "wheel"
)

Invoke-Checked -Executable $venvPython -Arguments @(
    (Join-Path $separatorRoot "scripts\install-runtime.py"),
    "--python", $venvPython, "--backend", $Backend
)

$separatorExtra = "audio-separator>=0.44.5,<0.45"
Invoke-Checked -Executable $venvPython -Arguments @(
    "-m", "pip", "install", $separatorExtra, "--no-deps"
)
$onnxRuntime = "onnxruntime==1.24.4"
Invoke-Checked -Executable $venvPython -Arguments @(
    "-m", "pip", "uninstall", "-y", "onnxruntime-gpu"
)
Invoke-Checked -Executable $venvPython -Arguments @(
    "-m", "pip", "install", "-r", $requirements, $onnxRuntime, "diffq-fixed>=0.2"
)
# CPU and GPU ONNX distributions share files; repair them after removing GPU ONNX.
Invoke-Checked -Executable $venvPython -Arguments @(
    "-m", "pip", "install", "--force-reinstall", "--no-deps", $onnxRuntime
)
Invoke-Checked -Executable $venvPython -Arguments @(
    "-m", "pip", "install", "--force-reinstall", "--no-deps", $separatorRoot
)

$runtimeCheck = @'
import json
from k3_separator.hardware import configure_device
configure_device()
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
    throw "Windows separation runtime health check failed."
}
$runtimeStatus = $runtime | Select-Object -Last 1 | ConvertFrom-Json

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
    throw "k3-separator.exe health check failed to run."
}
$health = $healthText | ConvertFrom-Json
if (-not $health.ok -or -not $health.result.runtime.audio_separator_installed) {
    throw "k3-separator.exe health check failed: $healthText"
}

$config = if (Test-Path -LiteralPath $ConfigPath -PathType Leaf) {
    Get-Content -LiteralPath $ConfigPath -Raw -Encoding UTF8 | ConvertFrom-Json
} else {
    [PSCustomObject]@{}
}
if (-not ($config.PSObject.Properties.Name -contains "separation")) {
    $config | Add-Member -MemberType NoteProperty -Name "separation" -Value ([PSCustomObject]@{})
}
Set-JsonProperty -Object $config.separation -Name "worker" -Value $worker
Set-JsonProperty -Object $config.separation -Name "model_dir" -Value $modelDir
Set-JsonProperty -Object $config.separation -Name "log_dir" -Value $logDir
$hardware = Get-Content -LiteralPath (Join-Path $VenvPath "k3-hardware.json") -Raw -Encoding UTF8 | ConvertFrom-Json
if (-not ($config.separation.PSObject.Properties.Name -contains "profile")) {
    Set-JsonProperty -Object $config.separation -Name "profile" -Value $hardware.separation.profile
}
foreach ($property in $hardware.separation.PSObject.Properties) {
    if (-not ($config.separation.PSObject.Properties.Name -contains $property.Name)) {
        if ($config.separation.profile -ne $hardware.separation.profile) { continue }
        if (($config.separation.PSObject.Properties.Name -contains "model") -and
            $config.separation.model -ne $hardware.separation.model) { continue }
        Set-JsonProperty -Object $config.separation -Name $property.Name -Value $property.Value
    }
}
$json = $config | ConvertTo-Json -Depth 10
[IO.File]::WriteAllText(
    [IO.Path]::GetFullPath($ConfigPath),
    $json + [Environment]::NewLine,
    [Text.UTF8Encoding]::new($false)
)
Write-Host "Configuration updated: $ConfigPath"

Write-Host "Windows separation worker ready: $worker"
Write-Host "CUDA available: $($runtimeStatus.cuda_available)"
Write-Host "Model directory: $modelDir"
