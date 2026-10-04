$ErrorActionPreference = 'Stop'
[Console]::OutputEncoding = New-Object Text.UTF8Encoding($false)
$root = $env:K3_SHORTCUT_ROOT
$identity = $env:K3_SHORTCUT_ID
if (-not $root -or $identity -notmatch '^[a-f0-9]{12}$') { throw 'Invalid shortcut installation parameters' }
# Use the Unicode shell-link interface; WScript.Shell rejects some Unicode targets.
Add-Type -TypeDefinition @'
using System;
using System.Runtime.InteropServices;
using System.Runtime.InteropServices.ComTypes;
using System.Text;

[ComImport, Guid("00021401-0000-0000-C000-000000000046")]
internal class K3ShellLink { }

[ComImport, Guid("000214F9-0000-0000-C000-000000000046"), InterfaceType(ComInterfaceType.InterfaceIsIUnknown)]
internal interface IK3ShellLinkW {
    void GetPath([Out, MarshalAs(UnmanagedType.LPWStr)] StringBuilder path, int count, IntPtr findData, uint flags);
    void GetIDList(out IntPtr idList);
    void SetIDList(IntPtr idList);
    void GetDescription([Out, MarshalAs(UnmanagedType.LPWStr)] StringBuilder description, int count);
    void SetDescription([MarshalAs(UnmanagedType.LPWStr)] string description);
    void GetWorkingDirectory([Out, MarshalAs(UnmanagedType.LPWStr)] StringBuilder directory, int count);
    void SetWorkingDirectory([MarshalAs(UnmanagedType.LPWStr)] string directory);
    void GetArguments([Out, MarshalAs(UnmanagedType.LPWStr)] StringBuilder arguments, int count);
    void SetArguments([MarshalAs(UnmanagedType.LPWStr)] string arguments);
    void GetHotkey(out short hotkey);
    void SetHotkey(short hotkey);
    void GetShowCmd(out int command);
    void SetShowCmd(int command);
    void GetIconLocation([Out, MarshalAs(UnmanagedType.LPWStr)] StringBuilder path, int count, out int index);
    void SetIconLocation([MarshalAs(UnmanagedType.LPWStr)] string path, int index);
    void SetRelativePath([MarshalAs(UnmanagedType.LPWStr)] string path, uint reserved);
    void Resolve(IntPtr window, uint flags);
    void SetPath([MarshalAs(UnmanagedType.LPWStr)] string path);
}

public static class K3Shortcut {
    public static void Create(string linkPath, string target, string directory) {
        object instance = new K3ShellLink();
        try {
            IK3ShellLinkW link = (IK3ShellLinkW)instance;
            link.SetPath(target);
            link.SetWorkingDirectory(directory);
            link.SetIconLocation(target, 0);
            link.SetDescription("K3 karaoke player and recorder");
            ((IPersistFile)instance).Save(linkPath, true);
        } finally { Marshal.FinalReleaseComObject(instance); }
    }
}
'@
$programs = [Environment]::GetFolderPath('Programs')
$targets = @((Join-Path $programs "K3-$identity.lnk"))
if ($env:K3_SHORTCUT_DESKTOP -eq '1') {
    $targets += Join-Path ([Environment]::GetFolderPath('DesktopDirectory')) "K3-$identity.lnk"
}
foreach ($target in $targets) {
    if (Test-Path -LiteralPath $target) { continue }
    [void][IO.Directory]::CreateDirectory((Split-Path -Parent $target))
    [K3Shortcut]::Create($target, (Join-Path $root 'k3-gui.exe'), [Environment]::GetFolderPath('MyDocuments'))
    Write-Output $target
}
