; Windows 当前用户安装；卸载仅处理安装器登记的文件。
[Setup]
#if CliOnly == "1"
AppId={{6F522DA6-E43A-4F10-B6E1-75072B818DA9}
AppName=K3 ARM64 CLI
DefaultDirName={localappdata}\Programs\K3-ARM64-CLI
DefaultGroupName=K3 ARM64 CLI
#else
AppId={{C854EC4D-A6CC-42BA-9DE6-D6653C1E4E34}
AppName=K3
DefaultDirName={localappdata}\Programs\K3
DefaultGroupName=K3
#endif
AppVersion={#AppVersion}
AppPublisher=K3 contributors
PrivilegesRequired=lowest
ArchitecturesAllowed={#TargetArchitecture}
ArchitecturesInstallIn64BitMode={#TargetArchitecture}
#if CliOnly == "1"
MinVersion=10.0.22000
#else
MinVersion=10.0
#endif
OutputDir={#OutputDir}
OutputBaseFilename={#OutputName}
Compression=lzma2/fast
SolidCompression=yes
LZMANumBlockThreads=2
WizardStyle=modern
#if CliOnly == "1"
UninstallDisplayIcon={app}\k3.exe
#else
UninstallDisplayIcon={app}\k3-gui.exe
#endif
CloseApplications=yes
RestartApplications=no

[Languages]
Name: "chinesesimplified"; MessagesFile: "ChineseSimplified.isl"

#if CliOnly == "0"
[Tasks]
Name: "desktopicon"; Description: "Create a desktop shortcut"; GroupDescription: "Shortcuts:"; Flags: unchecked

#endif

[Files]
Source: "{#BundleDir}\*"; DestDir: "{app}"; Flags: ignoreversion recursesubdirs createallsubdirs
Source: "ChineseSimplified-LICENSE.txt"; DestDir: "{app}\licenses"; DestName: "InnoSetup-ChineseSimplified-MIT.txt"; Flags: ignoreversion

[Icons]
#if CliOnly == "0"
Name: "{group}\K3"; Filename: "{app}\k3-gui.exe"; WorkingDir: "{userdocs}"
Name: "{autodesktop}\K3"; Filename: "{app}\k3-gui.exe"; WorkingDir: "{userdocs}"; Tasks: desktopicon

#endif
Name: "{group}\Uninstall K3"; Filename: "{uninstallexe}"

#if CliOnly == "0"
[Run]
Filename: "{app}\k3-gui.exe"; WorkingDir: "{userdocs}"; Description: "Launch K3"; Flags: nowait postinstall skipifsilent

#endif
