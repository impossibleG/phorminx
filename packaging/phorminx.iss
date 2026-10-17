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

#ifndef WebView2Bootstrapper
  #error "WebView2Bootstrapper must be supplied by the verified release build."
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
WizardStyle=modern dark
WizardImageFile=..\design\brand\png\installer\phorminx-wizard-600x1149.png
WizardSmallImageFile=..\design\brand\png\app\phorminx-app-128.png
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
Source: "{#WebView2Bootstrapper}"; DestDir: "{tmp}"; DestName: "MicrosoftEdgeWebView2Setup.exe"; Flags: deleteafterinstall
Source: "..\scripts\Initialize-Phorminx.ps1"; DestDir: "{app}\setup"; Flags: ignoreversion

[Icons]
Name: "{group}\Phorminx"; Filename: "{app}\phorminx-app.exe"; WorkingDir: "{app}"
Name: "{group}\Prepare Phorminx"; Filename: "{app}\phorminx-app.exe"; Parameters: "--setup"; WorkingDir: "{app}"
Name: "{autodesktop}\Phorminx"; Filename: "{app}\phorminx-app.exe"; WorkingDir: "{app}"; Tasks: desktopicon
Name: "{group}\Uninstall Phorminx"; Filename: "{uninstallexe}"

[Registry]
Root: HKCU; Subkey: "Software\Microsoft\Windows\CurrentVersion\Run"; ValueType: none; ValueName: "Phorminx"; Flags: dontcreatekey uninsdeletevalue
Root: HKCU; Subkey: "Software\Microsoft\Windows\CurrentVersion\Run"; ValueType: string; ValueName: "Phorminx"; ValueData: """{app}\phorminx-app.exe"" --background"; Flags: uninsdeletevalue; Tasks: startup

[Run]
Filename: "{tmp}\MicrosoftEdgeWebView2Setup.exe"; Parameters: "/silent /install"; StatusMsg: "Preparing the Windows interface..."; Check: not WebView2Installed; Flags: runasoriginaluser
Filename: "{app}\phorminx-app.exe"; Description: "Open Phorminx to finish setup"; Flags: nowait postinstall skipifsilent runasoriginaluser

[Code]
function InstalledRuntimeVersion(RootKey: Integer; Subkey: String): Boolean;
var
  Version: String;
begin
  Result := RegQueryStringValue(RootKey, Subkey, 'pv', Version) and
    (Version <> '') and (Version <> '0.0.0.0');
end;

function WebView2Installed: Boolean;
begin
  Result := InstalledRuntimeVersion(HKLM,
      'SOFTWARE\WOW6432Node\Microsoft\EdgeUpdate\Clients\{F3017226-FE2A-4295-8BDF-00C3A9A7E4C5}') or
    InstalledRuntimeVersion(HKCU,
      'Software\Microsoft\EdgeUpdate\Clients\{F3017226-FE2A-4295-8BDF-00C3A9A7E4C5}');
end;
