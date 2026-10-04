[CmdletBinding()]
param([string]$InstallDir, [string]$Repo,
      [string]$Version = 'latest', [string]$SourceDir, [string]$ModelCache, [switch]$Yes)
$ErrorActionPreference = 'Stop'
if (-not $InstallDir) { $InstallDir = $PSScriptRoot }
if (-not $Repo) {
    $Repo = $env:K3_RELEASE_REPO
    if (-not $Repo) {
        $python = Join-Path $InstallDir 'runtime\python\python.exe'
        if (Test-Path -LiteralPath $python) {
            $Repo = & $python -I -B (Join-Path $InstallDir 'scripts\installation_state.py') --prefix $InstallDir
            if ($LASTEXITCODE -ne 0) { throw 'Cannot read installation release repository' }
        } else { $Repo = 'coanor/k3' }
    }
}
$arguments = @{ InstallDir = $InstallDir; Repo = $Repo; Version = $Version; Update = $true; Yes = $Yes }
if ($SourceDir) { $arguments.SourceDir = $SourceDir }
if ($ModelCache) { $arguments.ModelCache = $ModelCache }
& (Join-Path $PSScriptRoot 'install.ps1') @arguments
