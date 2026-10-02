; Windows 当前用户安装；卸载仅处理安装器登记的文件。
[Setup]
AppId={{C854EC4D-A6CC-42BA-9DE6-D6653C1E4E34}
AppName=K3
AppVersion={#AppVersion}
AppPublisher=K3 contributors
DefaultDirName={localappdata}\Programs\K3
DefaultGroupName=K3
PrivilegesRequired=lowest
ArchitecturesAllowed=x64compatible
ArchitecturesInstallIn64BitMode=x64compatible
MinVersion=10.0
OutputDir={#OutputDir}
OutputBaseFilename={#OutputName}
Compression=lzma2/fast
SolidCompression=yes
LZMANumBlockThreads=2
WizardStyle=modern
UninstallDisplayIcon={app}\k3-gui.exe
CloseApplications=yes
RestartApplications=no

[Languages]
Name: "chinesesimplified"; MessagesFile: "ChineseSimplified.isl"

[Tasks]
Name: "desktopicon"; Description: "创建桌面快捷方式"; GroupDescription: "快捷方式："; Flags: unchecked

[Files]
Source: "{#BundleDir}\*"; DestDir: "{app}"; Flags: ignoreversion recursesubdirs createallsubdirs
Source: "ChineseSimplified-LICENSE.txt"; DestDir: "{app}\licenses"; DestName: "InnoSetup-ChineseSimplified-MIT.txt"; Flags: ignoreversion

[Icons]
Name: "{group}\K3"; Filename: "{app}\k3-gui.exe"; WorkingDir: "{userdocs}"
Name: "{group}\卸载 K3"; Filename: "{uninstallexe}"
Name: "{autodesktop}\K3"; Filename: "{app}\k3-gui.exe"; WorkingDir: "{userdocs}"; Tasks: desktopicon

[Run]
Filename: "{app}\k3-gui.exe"; WorkingDir: "{userdocs}"; Description: "启动 K3"; Flags: nowait postinstall skipifsilent
