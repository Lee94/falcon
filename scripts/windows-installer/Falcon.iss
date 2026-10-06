; Falcon 原生客户端的 Windows 安装包（Inno Setup 6）。由 scripts/build-windows-installer.mjs
; 传入下面这几个 /D 定义后编译，不要直接拿 ISCC 跑。
;
; 只装原生客户端：Windows 上没有本机服务（SEA 不支持 Windows 目标，见 README），装好后
; 在客户端里连别处的 falcon 服务端。所以这里也没有 macOS pkg 那种 postinstall 注册服务。
;
; 默认按当前用户装（不要管理员，装到 %LOCALAPPDATA%\Programs\Falcon），向导里可以改成
; 为所有用户安装。卸载不动用户数据：服务端连接与偏好在 %APPDATA%\falcon\Falcon\data
; （prefs.rs 的 ProjectDirs），登录凭据在
; Windows 凭据管理器里，重装后照旧能用。

#ifndef AppVersion
  #error 缺 /DAppVersion
#endif
#ifndef SourceExe
  #error 缺 /DSourceExe
#endif
#ifndef SourceIcon
  #error 缺 /DSourceIcon
#endif
#ifndef OutputDir
  #error 缺 /DOutputDir
#endif
#ifndef OutputBaseFilename
  #error 缺 /DOutputBaseFilename
#endif

[Setup]
; AppId 是升级 / 卸载认同一个程序的依据，定下来就别改
AppId={{6E0C2B7A-3F4D-4E8B-9A51-0C7D2F1B8E43}
AppName=Falcon
AppVersion={#AppVersion}
AppVerName=Falcon {#AppVersion}
AppPublisher=Falcon
DefaultDirName={autopf}\Falcon
DefaultGroupName=Falcon
DisableProgramGroupPage=yes
PrivilegesRequired=lowest
PrivilegesRequiredOverridesAllowed=dialog
ArchitecturesAllowed=x64compatible
ArchitecturesInstallIn64BitMode=x64compatible
MinVersion=10.0
OutputDir={#OutputDir}
OutputBaseFilename={#OutputBaseFilename}
SetupIconFile={#SourceIcon}
UninstallDisplayIcon={app}\Falcon.exe
UninstallDisplayName=Falcon
Compression=lzma2/max
SolidCompression=yes
WizardStyle=modern
; 升级时客户端还开着：经重启管理器关掉它再覆盖（会话活在服务端，关客户端只是 Detach）
CloseApplications=yes
RestartApplications=no
ShowLanguageDialog=no

[Languages]
; 简体中文不在 Inno Setup 的官方语言包里，用的是 issrc 仓库 Unofficial 目录里那份（已放在本目录）
Name: "zh_CN"; MessagesFile: "ChineseSimplified.isl"

[Tasks]
Name: "desktopicon"; Description: "{cm:CreateDesktopIcon}"; GroupDescription: "{cm:AdditionalIcons}"; Flags: unchecked

[Files]
Source: "{#SourceExe}"; DestDir: "{app}"; DestName: "Falcon.exe"; Flags: ignoreversion

[Icons]
Name: "{autoprograms}\Falcon"; Filename: "{app}\Falcon.exe"
Name: "{autodesktop}\Falcon"; Filename: "{app}\Falcon.exe"; Tasks: desktopicon

[Run]
Filename: "{app}\Falcon.exe"; Description: "{cm:LaunchProgram,Falcon}"; Flags: nowait postinstall skipifsilent
