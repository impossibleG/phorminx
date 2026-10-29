[CmdletBinding()]
param()

$ErrorActionPreference = "Stop"
Set-StrictMode -Version Latest

$repositoryRoot = Split-Path -Parent $PSScriptRoot
$installerPath = Join-Path $repositoryRoot "packaging\phorminx.iss"
$diagnosticsScript = Join-Path $PSScriptRoot "Collect-PhorminxDiagnostics.ps1"
$compatibilityScript = Join-Path $PSScriptRoot "Invoke-PhorminxCompatibility.ps1"
$buildScript = Join-Path $PSScriptRoot "Build-PhorminxInstaller.ps1"
$failures = [System.Collections.Generic.List[string]]::new()

function Assert-True {
    param([bool] $Condition, [string] $Message)
    if (-not $Condition) {
        $script:failures.Add($Message)
    }
}

$installer = Get-Content -LiteralPath $installerPath -Raw
$releaseBuild = Get-Content -LiteralPath $buildScript -Raw
Assert-True ($releaseBuild -match 'PhorminxBuild') "Release build must use the short native target path."
Assert-True ($releaseBuild -match 'short-v2') "Release build must isolate caches created before short-path and native CRT alignment."
Assert-True ($releaseBuild -match '\[string\] \$BuildRoot') "Release build must permit a dedicated alternate drive when LocalAppData is full."
Assert-True ($releaseBuild -match 'subst\.exe') "Release build must map per-user storage to a short drive root."
Assert-True ($releaseBuild -match 'Dismount-ShortBuildDrive') "Release build must remove its temporary drive mapping."
Assert-True ($releaseBuild -match '(?s)finally\s*\{\s*Dismount-ShortBuildDrive') "Temporary drive cleanup must be protected by a finally boundary."
Assert-True ($releaseBuild -match 'CARGO_TARGET_DIR = "\$shortBuildDrive\\"') "Cargo must see the temporary drive root rather than the backing path."
Assert-True ($releaseBuild -match 'desktop,vulkan') "Default Windows release must compile the Vulkan backend."
Assert-True ($releaseBuild -match 'desktop,vulkan,tauri-shell') "Default Windows release must ship the Tauri product shell."
Assert-True ($releaseBuild -match 'TAURI_CONFIG =') "Tauri and installer release versions must be synchronized."
Assert-True ($releaseBuild -match 'MicrosoftEdgeWebView2Setup') "Release build must package the WebView2 prerequisite."
Assert-True ($releaseBuild -match "SignerCertificate.Subject -notmatch 'O=Microsoft Corporation'") "WebView2 bootstrapper must be Microsoft-signed."
Assert-True ($releaseBuild -match 'target-feature=\+crt-static') "Release build must statically link the MSVC CRT."
Assert-True ($releaseBuild -match 'CMAKE_MSVC_RUNTIME_LIBRARY\s*=\s*"MultiThreaded"') "Native CMake targets must select the static MSVC CRT."
Assert-True ($releaseBuild -match 'CMAKE_C_FLAGS_RELEASE\s*=\s*"/MT') "Native C release objects must compile with /MT."
Assert-True ($releaseBuild -match 'CMAKE_CXX_FLAGS_RELEASE\s*=\s*"/MT') "Native C++ release objects must compile with /MT."
Assert-True ($releaseBuild -match '/dependents') "Release packaging must inspect the executable dependency table."
Assert-True ($releaseBuild -match '\?i:msvcp\|vcruntime\|concrt') "Release packaging must reject redistributable-only Microsoft C/C++ runtime dependencies."
Assert-True ($installer -match '(?m)^PrivilegesRequired=lowest\r?$') "Installer must be per-user."
Assert-True ($installer -match '(?m)^DefaultDirName=\{localappdata\}\\Programs\\\{#AppFolder\}\r?$') "Installer must target LocalAppData."
Assert-True ($installer -match '(?ms)^#else\r?$.*?^\s*#define AppFolder "Phorminx"\r?$.*?^#endif') "The release installer must install into the Phorminx folder."
Assert-True ($installer -match '(?ms)^#else\r?$.*?^\s*#define AppTitle "Phorminx"\r?$.*?^#endif') "The release installer must be named Phorminx."
Assert-True ($installer -match '(?m)^Root: HKCU;.*CurrentVersion\\Run') "Autostart must use HKCU."
Assert-True ($installer -match '(?m)^Root: HKCU;.*ValueData: """\{app\}\\phorminx-app\.exe"" --background"') "Autostart must keep the product shell hidden."
Assert-True ($installer -match '(?m)^Root: HKCU;.*ValueType: none;.*Flags: dontcreatekey uninsdeletevalue\r?$') "Uninstall must remove runtime-enabled autostart without creating the Run key."
Assert-True ($installer -notmatch '(?im)runascurrentuser|PrivilegesRequiredOverridesAllowed') "Installer must not offer elevated application execution."
Assert-True ($installer -match 'Initialize-Phorminx\.ps1.*DestDir: "\{app\}\\setup"') "Installer must include the verified starter script."
Assert-True ($installer -notmatch '(?m)^Filename:.*powershell(?:\.exe)?') "Installer must not launch a visible PowerShell setup window."
Assert-True ($installer -match 'Prepare \{#AppTitle\}"; Filename: "\{app\}\\phorminx-app\.exe"; Parameters: "--setup"') "The setup shortcut must open the branded app, not a shell."
Assert-True ($installer -match 'WebView2Installed') "Installer must check for WebView2 before starting Tauri."
Assert-True ($installer -match 'MicrosoftEdgeWebView2Setup\.exe.*Check: not WebView2Installed') "Installer must bootstrap missing WebView2."
Assert-True ($installer -match 'postinstall skipifsilent runasoriginaluser') "Setup must be explicitly offered and run as the original user, never forced during silent updates."
Assert-True ($installer -match 'Description: "Open Phorminx to finish setup"') "First-run setup must begin in Phorminx."
Assert-True ($installer -match 'Name: "\{group\}\\Prepare \{#AppTitle\}"') "Installer must provide a visible setup retry shortcut."
Assert-True ($releaseBuild -match 'PHORMINX_RELEASE_VERSION = \$Version') "Built application must embed the installer release identity."
Assert-True ($releaseBuild -match 'embeddedVersion -ne \$Version') "SkipBuild must reject mismatched binary identity."
Assert-True ($installer -notmatch '(?im)^.*models[\\/].*\.bin') "Installer must not bundle speech models."
Assert-True ($installer -match '(?ms)#ifdef SignToolName.*^SignTool=\{#SignToolName\}.*^SignedUninstaller=yes') "Installer must support externally configured signing for release builds."
Assert-True ($installer -match '(?m)^SetupIconFile=\.\.\\design\\brand\\phorminx\.ico\r?$') "Installer must use the production Phorminx icon."
Assert-True ($installer -match '(?m)^WizardStyle=modern dark\r?$') "Installer wizard must use the Phorminx dark presentation."
Assert-True ($installer -match '(?m)^WizardImageFile=\.\.\\design\\brand\\png\\installer\\phorminx-wizard-600x1149\.png\r?$') "Installer wizard must show the branded art."
Assert-True ($installer -match '(?m)^Name: "\{autodesktop\}\\\{#AppTitle\}";.*Tasks: desktopicon\r?$') "Installer must offer the Phorminx desktop shortcut."

$temporaryRoot = Join-Path ([System.IO.Path]::GetTempPath()) ("phorminx-release-test-" + [guid]::NewGuid().ToString("N"))
New-Item -ItemType Directory -Path $temporaryRoot | Out-Null
try {
    $bundlePath = Join-Path $temporaryRoot "diagnostics.zip"
    & $diagnosticsScript -OutputPath $bundlePath -ApplicationPath "C:\sensitive\user\phorminx-app.exe" -SettingsPath "C:\sensitive\settings.toml" | Out-Null
    Assert-True (Test-Path -LiteralPath $bundlePath -PathType Leaf) "Diagnostics bundle was not created."

    $expandedPath = Join-Path $temporaryRoot "expanded"
    Expand-Archive -LiteralPath $bundlePath -DestinationPath $expandedPath
    $entries = @(Get-ChildItem -LiteralPath $expandedPath -File | Select-Object -ExpandProperty Name | Sort-Object)
    Assert-True (($entries -join ',') -eq 'manifest.json,runtime.json,system.json') "Diagnostics bundle contains an unexpected file."

    $combinedJson = (Get-Content -LiteralPath (Join-Path $expandedPath "system.json") -Raw) +
        (Get-Content -LiteralPath (Join-Path $expandedPath "runtime.json") -Raw)
    Assert-True ($combinedJson -notmatch [regex]::Escape("C:\sensitive")) "Diagnostics leaked a supplied path."
    Assert-True ($combinedJson -notmatch '(?i)username|machinename|window_title|clipboard_content|transcript') "Diagnostics contains a forbidden content field."

    $runDirectory = Join-Path $temporaryRoot "compat"
    $runFile = (& $compatibilityScript -Start -RunDirectory $runDirectory).FullName
    & $compatibilityScript -Record -RunFile $runFile -Scenario focus_change -Outcome pass | Out-Null
    $summary = & $compatibilityScript -Summary -SummaryRunFile $runFile
    Assert-True ($summary.total -eq 11) "Compatibility harness scenario count changed unexpectedly."
    Assert-True ($summary.complete -eq 1) "Compatibility harness did not persist an outcome."
    Assert-True ($summary.failures -eq 0) "Compatibility harness recorded an unexpected failure."
} finally {
    if (Test-Path -LiteralPath $temporaryRoot) {
        Remove-Item -LiteralPath $temporaryRoot -Recurse -Force
    }
}

if ($failures.Count -gt 0) {
    $failures | ForEach-Object { Write-Error $_ }
    throw "$($failures.Count) release asset validation(s) failed."
}

Write-Host "Release asset validations passed."
