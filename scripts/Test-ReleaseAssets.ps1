[CmdletBinding()]
param()

$ErrorActionPreference = "Stop"
Set-StrictMode -Version Latest

$repositoryRoot = Split-Path -Parent $PSScriptRoot
$installerPath = Join-Path $repositoryRoot "packaging\phorminx.iss"
$diagnosticsScript = Join-Path $PSScriptRoot "Collect-PhorminxDiagnostics.ps1"
$compatibilityScript = Join-Path $PSScriptRoot "Invoke-PhorminxCompatibility.ps1"
$failures = [System.Collections.Generic.List[string]]::new()

function Assert-True {
    param([bool] $Condition, [string] $Message)
    if (-not $Condition) {
        $script:failures.Add($Message)
    }
}

$installer = Get-Content -LiteralPath $installerPath -Raw
Assert-True ($installer -match '(?m)^PrivilegesRequired=lowest$') "Installer must be per-user."
Assert-True ($installer -match '(?m)^DefaultDirName=\{localappdata\}\\Programs\\Phorminx$') "Installer must target LocalAppData."
Assert-True ($installer -match '(?m)^Root: HKCU;.*CurrentVersion\\Run') "Autostart must use HKCU."
Assert-True ($installer -match '(?m)^Root: HKCU;.*ValueType: none;.*Flags: uninsdeletevalue$') "Uninstall must remove runtime-enabled autostart even when the install task was initially unchecked."
Assert-True ($installer -notmatch '(?im)runas|PrivilegesRequiredOverridesAllowed') "Installer must not request or offer elevation."
Assert-True ($installer -notmatch '(?im)^.*models[\\/].*\.bin') "Installer must not bundle speech models."

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
