$ErrorActionPreference = 'Stop'
[Console]::OutputEncoding = New-Object Text.UTF8Encoding($false)
$root = $env:K3_SHORTCUT_ROOT
$identity = $env:K3_SHORTCUT_ID
if (-not $root -or $identity -notmatch '^[a-f0-9]{12}$') { throw 'Invalid shortcut installation parameters' }
$shell = New-Object -ComObject WScript.Shell
$programs = [Environment]::GetFolderPath('Programs')
$targets = @((Join-Path $programs "K3-$identity.lnk"))
if ($env:K3_SHORTCUT_DESKTOP -eq '1') {
    $targets += Join-Path ([Environment]::GetFolderPath('DesktopDirectory')) "K3-$identity.lnk"
}
foreach ($target in $targets) {
    if (Test-Path -LiteralPath $target) { continue }
    [void][IO.Directory]::CreateDirectory((Split-Path -Parent $target))
    $shortcut = $shell.CreateShortcut($target)
    $shortcut.TargetPath = Join-Path $root 'k3-gui.exe'
    $shortcut.WorkingDirectory = [Environment]::GetFolderPath('MyDocuments')
    $shortcut.IconLocation = (Join-Path $root 'k3-gui.exe') + ',0'
    $shortcut.Description = 'K3 karaoke player and recorder'
    $shortcut.Save()
    Write-Output $target
}
