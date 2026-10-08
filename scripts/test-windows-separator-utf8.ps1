[CmdletBinding()]
param(
    [string]$ScriptPath,
    [string]$WorkRoot = $env:TEMP
)

$ErrorActionPreference = "Stop"
Set-StrictMode -Version Latest
if ([string]::IsNullOrWhiteSpace($ScriptPath)) {
    $ScriptPath = Join-Path $PSScriptRoot "..\separate.ps1"
}

# Run the real script with a stub CLI. A UTF-8 project title whose bytes break
# Windows PowerShell 5.1's ANSI fallback exercises the final manifest read.
$fixture = Join-Path $WorkRoot ("k3-utf8-" + [guid]::NewGuid().ToString("N"))
$projectRoot = Join-Path $fixture "projects"
$project = Join-Path $projectRoot "song"
$audio = Join-Path $fixture "song.flac"
$worker = Join-Path $fixture "worker.exe"
$stub = Join-Path $fixture "k3-stub.ps1"
$script = Join-Path $fixture "separate.ps1"
$utf8 = New-Object System.Text.UTF8Encoding($false)

New-Item -ItemType Directory -Force -Path $project | Out-Null
try {
    Copy-Item -LiteralPath $ScriptPath -Destination $script
    [IO.File]::WriteAllText($audio, "audio", $utf8)
    [IO.File]::WriteAllText($worker, "", $utf8)
    [IO.File]::WriteAllText($stub, @'
[IO.File]::WriteAllText((Join-Path $PSScriptRoot "captured.json"),
    (ConvertTo-Json -InputObject @($args)), [Text.UTF8Encoding]::new($false))
$global:LASTEXITCODE = 0
'@, $utf8)
    $title = [Text.Encoding]::UTF8.GetString(
        [Convert]::FromBase64String("546L6I+yLeWuueaYk+WPl+S8pOeahOWls+S6ug==")
    )
    $document = @{
        title = $title
        separation = @{
            status = "ready"
            details = @{
                vocals = "stems/vocals.wav"
                accompaniment = "stems/accompaniment.wav"
            }
        }
    } | ConvertTo-Json -Depth 10
    [IO.File]::WriteAllText((Join-Path $project "project.json"), $document, $utf8)
    $config = @{
        projects_root = $projectRoot
        separation = @{ worker = $worker; profile = "quality" }
    } | ConvertTo-Json -Depth 10
    [IO.File]::WriteAllText((Join-Path $fixture "config.json"), $config, $utf8)

    $env:K3_BIN = $stub
    $env:K3_NO_OVERWRITE = "0"
    $env:K3_PROFILE = "quality"
    $env:K3_WORKER = ""
    $env:K3_DEVICE = "auto"
    & $script -f $audio -d $projectRoot | Out-Null
    Write-Output "UTF-8 project manifest parsed by Windows PowerShell"

    $pythonRoot = Join-Path $fixture "runtime\python"
    New-Item -ItemType Directory -Force -Path $pythonRoot | Out-Null
    $plan = @{
        schema_version = 1
        backend = "cuda"
        separation = @{
            profile = "fast"; model = "uvr-mdx-karaoke-2"; segment_size = 128
            autocast = $false; preserve_backing_vocals = $false
        }
    } | ConvertTo-Json -Depth 10
    [IO.File]::WriteAllText((Join-Path $pythonRoot "k3-hardware.json"), $plan, $utf8)
    $env:K3_PROFILE = ""
    & $script -f $audio -d $projectRoot | Out-Null
    $captured = Get-Content -LiteralPath (Join-Path $fixture "captured.json") -Raw -Encoding UTF8 | ConvertFrom-Json
    if ($captured[$captured.IndexOf("--profile") + 1] -ne "quality" -or
        $captured -contains "--segment-size" -or $captured -contains "--no-preserve-backing-vocals") {
        throw "Saved profile was replaced by hardware recommendations."
    }

    $config = @{ separation = @{ worker = $worker } } | ConvertTo-Json -Depth 10
    [IO.File]::WriteAllText((Join-Path $fixture "config.json"), $config, $utf8)
    foreach ($name in @("K3_MODEL", "K3_SEGMENT_SIZE", "K3_AUTOCAST", "K3_PRESERVE_BACKING_VOCALS")) {
        [Environment]::SetEnvironmentVariable($name, "", "Process")
    }
    & $script -f $audio -d $projectRoot | Out-Null
    $captured = Get-Content -LiteralPath (Join-Path $fixture "captured.json") -Raw -Encoding UTF8 | ConvertFrom-Json
    if ($captured[$captured.IndexOf("--profile") + 1] -ne "fast" -or
        $captured[$captured.IndexOf("--segment-size") + 1] -ne "128" -or
        $captured -notcontains "--no-autocast" -or $captured -notcontains "--no-preserve-backing-vocals") {
        throw "Hardware defaults did not reach the separation command."
    }
    $env:K3_PROFILE = "quality"
    $env:K3_MODEL = "mel-band-roformer-kim-vocal-2"
    $env:K3_SEGMENT_SIZE = "64"
    $env:K3_AUTOCAST = "true"
    $env:K3_PRESERVE_BACKING_VOCALS = "true"
    & $script -f $audio -d $projectRoot | Out-Null
    $captured = Get-Content -LiteralPath (Join-Path $fixture "captured.json") -Raw -Encoding UTF8 | ConvertFrom-Json
    if ($captured[$captured.IndexOf("--profile") + 1] -ne "quality" -or
        $captured[$captured.IndexOf("--segment-size") + 1] -ne "64" -or
        $captured -contains "--no-autocast" -or $captured -contains "--no-preserve-backing-vocals") {
        throw "Explicit settings did not override hardware recommendations."
    }
    Write-Output "Hardware defaults and explicit preferences passed on Windows PowerShell"
    foreach ($name in @("K3_PROFILE", "K3_MODEL", "K3_SEGMENT_SIZE", "K3_AUTOCAST", "K3_PRESERVE_BACKING_VOCALS")) {
        [Environment]::SetEnvironmentVariable($name, "", "Process")
    }
    foreach ($device in @("cpu", "gpu", "CPU", "GPU")) {
        & $script -f $audio -d $projectRoot -Device $device | Out-Null
        $captured = Get-Content -LiteralPath (Join-Path $fixture "captured.json") -Raw -Encoding UTF8 | ConvertFrom-Json
        $expectedSegment = if ($device -eq "cpu") { "256" } else { "128" }
        if ($captured[$captured.IndexOf("--device") + 1] -cne $device.ToLowerInvariant() -or
            $captured[$captured.IndexOf("--segment-size") + 1] -ne $expectedSegment) {
            throw "Explicit device selection did not reach the separation command."
        }
    }
    Write-Output "CPU-only and GPU-only commands passed on Windows PowerShell"

    # Exercise the real installer's parameter setup without creating a runtime
    # or downloading dependencies. ValidateSet accepts mixed-case input.
    $installerPath = Join-Path (Split-Path -Parent $ScriptPath) "install-separator.ps1"
    $installerText = Get-Content -LiteralPath $installerPath -Raw -Encoding UTF8
    $setupEnd = $installerText.IndexOf('$runtimeCache =')
    if ($setupEnd -lt 0) { throw "Installer parameter setup was not found." }
    $parameterSetup = [scriptblock]::Create($installerText.Substring(0, $setupEnd) + 'Write-Output $Backend')
    foreach ($backend in @("GPU", "CpU", "AUTO")) {
        $selected = & $parameterSetup -Backend $backend -VenvPath $fixture -ConfigPath (Join-Path $fixture "config.json")
        if ($selected -cne $backend.ToLowerInvariant()) { throw "Installer backend was not normalized." }
    }
    Write-Output "Installer backend case normalization passed on Windows PowerShell"
} finally {
    Remove-Item -LiteralPath $fixture -Recurse -Force
}
