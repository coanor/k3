[CmdletBinding()]
param(
    [Parameter(Mandatory = $true)]
    [Alias("f")]
    [string[]]$Files,
    [Alias("d")]
    [string]$Directory
)

$ErrorActionPreference = "Stop"
Set-StrictMode -Version Latest
# Windows PowerShell 5.1 默认使用本机代码页输出；GUI 通过管道按 UTF-8 记录诊断。
$OutputEncoding = New-Object System.Text.UTF8Encoding($false)
try { [Console]::OutputEncoding = $OutputEncoding } catch { }

function ConvertTo-Boolean {
    param([Parameter(Mandatory = $true)][string]$Value, [string]$Name)
    switch ($Value.ToLowerInvariant()) {
        { $_ -in @("1", "true", "yes", "on") } { return $true }
        { $_ -in @("0", "false", "no", "off") } { return $false }
        default { throw "$Name 必须是 true 或 false。" }
    }
}

function Get-PropertyValue {
    param(
        [AllowNull()][object]$Object,
        [Parameter(Mandatory = $true)][string]$Name,
        [AllowNull()][object]$Default
    )

    if ($null -ne $Object -and $Object.PSObject.Properties.Name -contains $Name) {
        return $Object.$Name
    }
    return $Default
}

function Invoke-K3 {
    param([Parameter(Mandatory = $true)][string[]]$Arguments)
    & $script:K3Bin @Arguments
    if ($LASTEXITCODE -ne 0) {
        throw "k3.exe 执行失败（退出码 $LASTEXITCODE）。"
    }
}

function Write-SeparationOutputs {
    param([Parameter(Mandatory = $true)][string]$Project)

    $document = Get-Content -LiteralPath (Join-Path $Project "project.json") -Raw -Encoding UTF8 | ConvertFrom-Json
    if ($document.separation.status -ne "ready") {
        throw "project 未保存 ready 状态的分离 manifest：$Project"
    }
    $details = $document.separation.details
    foreach ($output in @(
        @{ Label = "主唱"; Name = "vocals" },
        @{ Label = "和声"; Name = "backing_vocals" },
        @{ Label = "伴奏"; Name = "accompaniment" }
    )) {
        $relative = Get-PropertyValue -Object $details -Name $output.Name -Default $null
        if (-not [string]::IsNullOrWhiteSpace($relative)) {
            $nativeRelative = $relative -replace "/", [IO.Path]::DirectorySeparatorChar
            Write-Host "  $($output.Label)：$(Join-Path $Project $nativeRelative)"
        }
    }
}

$configPath = Join-Path $PSScriptRoot "config.json"
$config = if (Test-Path -LiteralPath $configPath -PathType Leaf) {
    Get-Content -LiteralPath $configPath -Raw -Encoding UTF8 | ConvertFrom-Json
} else {
    $null
}
$separationConfig = if ($null -ne $config) {
    Get-PropertyValue -Object $config -Name "separation" -Default $null
} else {
    $null
}

if ([string]::IsNullOrWhiteSpace($Directory)) {
    if (-not [string]::IsNullOrWhiteSpace($env:K3_OUTPUT_DIR)) {
        $Directory = $env:K3_OUTPUT_DIR
    } elseif ($null -ne $config) {
        $Directory = Get-PropertyValue -Object $config -Name "projects_root" -Default $null
    } else {
        throw "未指定 project 总目录；请使用 -d 或设置 K3_OUTPUT_DIR。"
    }
}
if ([string]::IsNullOrWhiteSpace($Directory)) {
    throw "未指定 project 总目录；请使用 -d、设置 K3_OUTPUT_DIR 或配置 projects_root。"
}
New-Item -ItemType Directory -Force -Path $Directory | Out-Null
$projectsRoot = (Resolve-Path -LiteralPath $Directory).Path

$script:K3Bin = if (-not [string]::IsNullOrWhiteSpace($env:K3_BIN)) {
    $env:K3_BIN
} else {
    Join-Path $PSScriptRoot "k3.exe"
}
if (-not (Test-Path -LiteralPath $script:K3Bin -PathType Leaf)) {
    throw "找不到 k3.exe：$script:K3Bin"
}

$worker = if (-not [string]::IsNullOrWhiteSpace($env:K3_WORKER)) {
    $env:K3_WORKER
} elseif ($null -ne $config) {
    Get-PropertyValue -Object $separationConfig -Name "worker" -Default (Join-Path $PSScriptRoot ".venv-separator\Scripts\k3-separator.exe")
} else {
    Join-Path $PSScriptRoot ".venv-separator\Scripts\k3-separator.exe"
}
if (-not [IO.Path]::IsPathRooted($worker)) {
    $worker = Join-Path $PSScriptRoot $worker
}
if (-not (Test-Path -LiteralPath $worker -PathType Leaf)) {
    throw "找不到 Windows 分离 worker：$worker；请先运行 .\install-separator.ps1。"
}

$configuredProfile = Get-PropertyValue -Object $separationConfig -Name "profile" -Default "quality"
$profile = if (-not [string]::IsNullOrWhiteSpace($env:K3_PROFILE)) {
    $env:K3_PROFILE
} else {
    $configuredProfile
}
$model = if (-not [string]::IsNullOrWhiteSpace($env:K3_MODEL)) {
    $env:K3_MODEL
} elseif ($profile -eq $configuredProfile) {
    Get-PropertyValue -Object $separationConfig -Name "model" -Default $null
} else {
    $null
}
$modelDir = if (-not [string]::IsNullOrWhiteSpace($env:K3_MODEL_DIR)) {
    $env:K3_MODEL_DIR
} elseif ($null -ne $config) {
    Get-PropertyValue -Object $separationConfig -Name "model_dir" -Default $null
} else {
    $null
}
$segmentSize = if (-not [string]::IsNullOrWhiteSpace($env:K3_SEGMENT_SIZE)) {
    $env:K3_SEGMENT_SIZE
} elseif ($profile -eq $configuredProfile) {
    Get-PropertyValue -Object $separationConfig -Name "segment_size" -Default $null
} else {
    $null
}
$autocast = if (-not [string]::IsNullOrWhiteSpace($env:K3_AUTOCAST)) {
    ConvertTo-Boolean -Value $env:K3_AUTOCAST -Name "K3_AUTOCAST"
} elseif ($null -ne $config) {
    [bool](Get-PropertyValue -Object $separationConfig -Name "autocast" -Default $true)
} else {
    $true
}
$preserveBackingVocals = if (-not [string]::IsNullOrWhiteSpace($env:K3_PRESERVE_BACKING_VOCALS)) {
    ConvertTo-Boolean -Value $env:K3_PRESERVE_BACKING_VOCALS -Name "K3_PRESERVE_BACKING_VOCALS"
} elseif ($null -ne $config) {
    [bool](Get-PropertyValue -Object $separationConfig -Name "preserve_backing_vocals" -Default $true)
} else {
    $true
}
$logDir = Get-PropertyValue -Object $separationConfig -Name "log_dir" -Default $null
if (-not [string]::IsNullOrWhiteSpace($logDir)) {
    $env:K3_LOG_DIR = $logDir
}

$inputs = @()
$projects = @()
$seen = [Collections.Generic.HashSet[string]]::new([StringComparer]::OrdinalIgnoreCase)
foreach ($file in $Files) {
    $resolved = (Resolve-Path -LiteralPath $file -ErrorAction Stop).Path
    if (-not (Test-Path -LiteralPath $resolved -PathType Leaf)) {
        throw "音频文件不存在：$file"
    }
    $name = [IO.Path]::GetFileNameWithoutExtension($resolved)
    if (-not $seen.Add($name)) {
        throw "多个输入会映射到同一个 project：$name"
    }
    $project = Join-Path $projectsRoot $name
    if ((Test-Path -LiteralPath $project) -and
        -not (Test-Path -LiteralPath (Join-Path $project "project.json") -PathType Leaf)) {
        throw "目标目录已存在但不是有效 project：$project"
    }
    $inputs += $resolved
    $projects += $project
}

for ($index = 0; $index -lt $inputs.Count; $index++) {
    $input = $inputs[$index]
    $project = $projects[$index]
    $title = [IO.Path]::GetFileNameWithoutExtension($input)
    $replacing = Test-Path -LiteralPath (Join-Path $project "project.json") -PathType Leaf
    if ($replacing) {
        if ($env:K3_NO_OVERWRITE -eq "1") {
            throw "project already exists; refusing to replace stems: $project"
        }
        Write-Host "正在按当前配置重新分离：$title"
    } else {
        Write-Host "正在创建 project：$title"
        Invoke-K3 -Arguments @("new", "--root", $project, "--song", $input, "--title", $title)
    }

    $arguments = @("separate", "--project", $project, "--profile", $profile, "--worker", $worker)
    if ($replacing) {
        $arguments += "--overwrite"
    }
    if (-not [string]::IsNullOrWhiteSpace($model)) {
        $arguments += @("--model", $model)
    }
    if (-not [string]::IsNullOrWhiteSpace($modelDir)) {
        $arguments += @("--model-dir", $modelDir)
    }
    if ($null -ne $segmentSize) {
        $arguments += @("--segment-size", [string]$segmentSize)
    }
    if (-not $autocast) {
        $arguments += "--no-autocast"
    }
    if (-not $preserveBackingVocals) {
        $arguments += "--no-preserve-backing-vocals"
    }
    Invoke-K3 -Arguments $arguments

    Write-Host "完成：$project"
    Write-SeparationOutputs -Project $project
}
