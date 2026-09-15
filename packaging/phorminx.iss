#define AppPublisher "Phorminx"
#define AppURL "https://github.com/impossibleG/phorminx"

#ifndef AppVersion
  #define AppVersion "0.1.0"
#endif

#ifndef NumericVersion
  #define NumericVersion "0.1.0.0"
#endif

#ifndef SourceExe
  #define SourceExe "..\target\release\phorminx-app.exe"
#endif

#ifndef OutputDir
  #define OutputDir "..\artifacts\installer"
#endif

[Setup]
AppId={{6C4860D4-9A6C-42C4-BBD6-A41597B6BB16}
AppName=Phorminx
AppVersion={#AppVersion}
AppPublisher={#AppPublisher}
AppPublisherURL={#AppURL}
AppSupportURL={#AppURL}/issues
AppUpdatesURL={#AppURL}/releases
DefaultDirName={localappdata}\Programs\Phorminx
DefaultGroupName=Phorminx
DisableProgramGroupPage=yes
PrivilegesRequired=lowest
ArchitecturesAllowed=x64compatible
ArchitecturesInstallIn64BitMode=x64compatible
MinVersion=10.0.17763
OutputDir={#OutputDir}
OutputBaseFilename=Phorminx-{#AppVersion}-x64-setup
Compression=lzma2/max
SolidCompression=yes
WizardStyle=modern
CloseApplications=yes
RestartApplications=no
SetupLogging=yes
SetupIconFile=..\design\brand\phorminx.ico
UninstallDisplayIcon={app}\phorminx-app.exe
VersionInfoVersion={#NumericVersion}
VersionInfoCompany={#AppPublisher}
VersionInfoDescription=Phorminx per-user installer
VersionInfoProductName=Phorminx
VersionInfoProductVersion={#NumericVersion}
#ifdef SignToolName
SignTool={#SignToolName}
SignedUninstaller=yes
#endif

[Tasks]
Name: "desktopicon"; Description: "Create a desktop shortcut"; GroupDescription: "Shortcuts:"; Flags: checkedonce
Name: "startup"; Description: "Launch Phorminx when I sign in"; GroupDescription: "Startup:"; Flags: unchecked

[Files]
Source: "{#SourceExe}"; DestDir: "{app}"; DestName: "phorminx-app.exe"; Flags: ignoreversion

[Icons]
Name: "{group}\Phorminx"; Filename: "{app}\phorminx-app.exe"; WorkingDir: "{app}"
Name: "{autodesktop}\Phorminx"; Filename: "{app}\phorminx-app.exe"; WorkingDir: "{app}"; Tasks: desktopicon
Name: "{group}\Uninstall Phorminx"; Filename: "{uninstallexe}"

[Registry]
Root: HKCU; Subkey: "Software\Microsoft\Windows\CurrentVersion\Run"; ValueType: none; ValueName: "Phorminx"; Flags: dontcreatekey uninsdeletevalue
Root: HKCU; Subkey: "Software\Microsoft\Windows\CurrentVersion\Run"; ValueType: string; ValueName: "Phorminx"; ValueData: """{app}\phorminx-app.exe"" --background"; Flags: uninsdeletevalue; Tasks: startup

[Run]
Filename: "{app}\phorminx-app.exe"; Description: "Launch Phorminx"; Flags: nowait postinstall skipifsilent unchecked
