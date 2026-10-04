[CmdletBinding()]
param([string]$InstallDir = $PSScriptRoot, [switch]$Yes)
$ErrorActionPreference = 'Stop'
$InstallDir = $ExecutionContext.SessionState.Path.GetUnresolvedProviderPathFromPSPath($InstallDir)
if (-not $Yes) {
    $answer = Read-Host "Remove K3 installation files from $InstallDir? [y/N]"
    if ($answer -notin @('y', 'Y', 'yes', 'YES')) { Write-Host 'Cancelled. No installation files were removed.'; return }
}
$lock = Join-Path (Split-Path -Parent $InstallDir) ('.' + (Split-Path -Leaf $InstallDir) + '.k3-maintenance-lock')
[void](New-Item -ItemType Directory -Path $lock -ErrorAction Stop)
try {
    $python = Join-Path $InstallDir 'runtime\python\python.exe'
    if (-not (Test-Path -LiteralPath $python)) { throw 'No maintenance Python found; use uninstall.ps1 from your online installation' }
    $json = & $python -I -B (Join-Path $InstallDir 'scripts\uninstall-online.py') --prefix $InstallDir --plan
    if ($LASTEXITCODE -ne 0) { throw 'Installation file validation failed; nothing was removed' }
    $plan = $json | ConvertFrom-Json
    foreach ($path in $plan.kept) { Write-Host "Keeping changed installation file: $path" }
    # Check running executables before deleting any registered files.
    foreach ($path in $plan.files) {
        $item = Get-Item -LiteralPath $path -Force
        if (-not ($item.Attributes -band [IO.FileAttributes]::ReparsePoint)) {
            $stream = [IO.File]::Open($path, 'Open', 'Read', 'None')
            $stream.Dispose()
        }
    }
    foreach ($path in $plan.files) { Remove-Item -LiteralPath $path -Force }
    Remove-Item -LiteralPath (Join-Path $InstallDir 'installation-state.json') -Force
    foreach ($path in $plan.directories) {
        if (@(Get-ChildItem -LiteralPath $path -Force).Count -eq 0) { Remove-Item -LiteralPath $path -Force }
    }
    if (@(Get-ChildItem -LiteralPath $InstallDir -Force).Count -eq 0) { Remove-Item -LiteralPath $InstallDir -Force }
    else { Write-Host "Kept remaining user files in: $InstallDir" }
    Write-Host 'Uninstall complete. Personal projects, recordings and settings were preserved.'
} finally {
    Remove-Item -LiteralPath $lock -Force
}
