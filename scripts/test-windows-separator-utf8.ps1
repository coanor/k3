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
    [IO.File]::WriteAllText($stub, '$global:LASTEXITCODE = 0', $utf8)
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
    & $script -f $audio -d $projectRoot | Out-Null
    Write-Output "UTF-8 project manifest parsed by Windows PowerShell"
} finally {
    Remove-Item -LiteralPath $fixture -Recurse -Force
}
