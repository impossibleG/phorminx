[CmdletBinding()]
param(
    [string] $Version = "0.1.0",
    [string] $IsccPath,
    [string] $InnoSignToolName,
    [string] $OutputDirectory,
    [switch] $SkipBuild,
    [switch] $RequireSignedBinary
)

$ErrorActionPreference = "Stop"
Set-StrictMode -Version Latest

$repositoryRoot = Split-Path -Parent $PSScriptRoot
$installerScript = Join-Path $repositoryRoot "packaging\phorminx.iss"
$sourceExecutable = Join-Path $repositoryRoot "target\release\phorminx-app.exe"
if ([string]::IsNullOrWhiteSpace($OutputDirectory)) {
    $OutputDirectory = Join-Path $repositoryRoot "artifacts\installer"
}

if ($Version -notmatch '^\d+\.\d+\.\d+(?:[-+][0-9A-Za-z.-]+)?$') {
    throw "Version must be a SemVer-like value, for example 0.1.0 or 0.1.0-alpha.1."
}
$numericVersion = ([regex]::Match($Version, '^(\d+)\.(\d+)\.(\d+)')).Groups[1..3].Value -join '.'
$numericVersion += '.0'

if (-not $SkipBuild) {
    & (Join-Path $PSScriptRoot "Enter-PhorminxDevShell.ps1")
    if ($LASTEXITCODE -ne 0) {
        throw "The Phorminx development shell could not be initialized."
    }

    & cargo build --locked --release --package phorminx-app --features desktop
    if ($LASTEXITCODE -ne 0) {
        throw "The desktop release build failed with exit code $LASTEXITCODE."
    }
}

if (-not (Test-Path -LiteralPath $sourceExecutable -PathType Leaf)) {
    throw "Release executable not found. Build it first: $sourceExecutable"
}

$signature = Get-AuthenticodeSignature -LiteralPath $sourceExecutable
if ($RequireSignedBinary -and $signature.Status -ne [System.Management.Automation.SignatureStatus]::Valid) {
    throw "The release executable is not validly Authenticode-signed (status: $($signature.Status))."
}
if (-not $RequireSignedBinary -and $signature.Status -ne [System.Management.Automation.SignatureStatus]::Valid) {
    Write-Warning "Building a development installer around an unsigned binary. Do not distribute it publicly."
}
if ($RequireSignedBinary -and [string]::IsNullOrWhiteSpace($InnoSignToolName)) {
    throw "RequireSignedBinary also requires -InnoSignToolName naming an Inno Setup signing tool configured outside this repository."
}
if (-not [string]::IsNullOrWhiteSpace($InnoSignToolName) -and $InnoSignToolName -notmatch '^[A-Za-z0-9_.-]+$') {
    throw "InnoSignToolName may contain only letters, digits, dots, underscores, and hyphens."
}

if ([string]::IsNullOrWhiteSpace($IsccPath)) {
    $command = Get-Command ISCC.exe -ErrorAction SilentlyContinue
    if ($null -ne $command) {
        $IsccPath = $command.Source
    } else {
        $candidates = @(
            (Join-Path ${env:ProgramFiles(x86)} "Inno Setup 6\ISCC.exe"),
            (Join-Path $env:LOCALAPPDATA "Programs\Inno Setup 6\ISCC.exe")
        )
        $IsccPath = $candidates | Where-Object { Test-Path -LiteralPath $_ -PathType Leaf } | Select-Object -First 1
    }
}

if ([string]::IsNullOrWhiteSpace($IsccPath) -or -not (Test-Path -LiteralPath $IsccPath -PathType Leaf)) {
    throw "Inno Setup 6 compiler (ISCC.exe) was not found. Pass -IsccPath explicitly."
}

New-Item -ItemType Directory -Force -Path $OutputDirectory | Out-Null
$resolvedOutput = (Resolve-Path -LiteralPath $OutputDirectory).Path
$resolvedSource = (Resolve-Path -LiteralPath $sourceExecutable).Path

$compilerArguments = @(
    "/DAppVersion=$Version",
    "/DNumericVersion=$numericVersion",
    "/DSourceExe=$resolvedSource",
    "/DOutputDir=$resolvedOutput",
    $installerScript
)
if (-not [string]::IsNullOrWhiteSpace($InnoSignToolName)) {
    $compilerArguments = @("/DSignToolName=$InnoSignToolName") + $compilerArguments
}
& $IsccPath @compilerArguments
if ($LASTEXITCODE -ne 0) {
    throw "Inno Setup failed with exit code $LASTEXITCODE."
}

$installer = Get-ChildItem -LiteralPath $resolvedOutput -Filter "Phorminx-$Version-x64-setup.exe" |
    Select-Object -First 1
if ($null -eq $installer) {
    throw "Inno Setup reported success, but the expected installer was not found."
}

$installerSignature = Get-AuthenticodeSignature -LiteralPath $installer.FullName
if ($RequireSignedBinary -and $installerSignature.Status -ne [System.Management.Automation.SignatureStatus]::Valid) {
    throw "The installer is not validly Authenticode-signed. Configure Inno Setup signing before a public release."
}

$hash = Get-FileHash -LiteralPath $installer.FullName -Algorithm SHA256
[pscustomobject]@{
    Installer = $installer.FullName
    Bytes = $installer.Length
    Sha256 = $hash.Hash
    BinarySignature = $signature.Status.ToString()
    InstallerSignature = $installerSignature.Status.ToString()
}
