[CmdletBinding()]
param(
    [string] $Version = "0.1.0",
    [string] $IsccPath,
    [string] $InnoSignToolName,
    [string] $OutputDirectory,
    [string] $BuildRoot,
    [switch] $SkipBuild,
    [switch] $RequireSignedBinary,
    [switch] $Cpu
)

$ErrorActionPreference = "Stop"
Set-StrictMode -Version Latest

$repositoryRoot = Split-Path -Parent $PSScriptRoot
$installerScript = Join-Path $repositoryRoot "packaging\phorminx.iss"
# whisper.cpp's nested Vulkan shader project exceeds MSVC's reliable path
# depth even under LocalAppData. Keep the files in per-user storage, but expose
# that storage through a temporary drive root while native code is compiling.
# The versioned directory also prevents reuse of old CMake caches that recorded
# the former long path.
$releaseTargetDirectory = if ([string]::IsNullOrWhiteSpace($BuildRoot)) {
    Join-Path $env:LOCALAPPDATA "PhorminxBuild\short-v2"
} else {
    $candidate = [System.IO.Path]::GetFullPath($BuildRoot)
    if ([System.IO.Path]::GetPathRoot($candidate).TrimEnd('\') -eq $candidate.TrimEnd('\')) {
        throw "BuildRoot must be a dedicated build directory, not a drive root."
    }
    $candidate
}
$sourceExecutable = Join-Path $releaseTargetDirectory "release\phorminx-app.exe"
$webView2Bootstrapper = Join-Path $releaseTargetDirectory "MicrosoftEdgeWebView2Setup.exe"
if ([string]::IsNullOrWhiteSpace($OutputDirectory)) {
    $OutputDirectory = Join-Path $repositoryRoot "artifacts\installer"
}

if ($Version -notmatch '^\d+\.\d+\.\d+(?:[-+][0-9A-Za-z.-]+)?$') {
    throw "Version must be a SemVer-like value, for example 0.1.0 or 0.1.0-alpha.1."
}
$numericVersion = ([regex]::Match($Version, '^(\d+)\.(\d+)\.(\d+)')).Groups[1..3].Value -join '.'
$numericVersion += '.0'

function Mount-ShortBuildDrive {
    param([Parameter(Mandatory)][string] $BackingDirectory)

    New-Item -ItemType Directory -Force -Path $BackingDirectory | Out-Null
    $resolvedBackingDirectory = (Resolve-Path -LiteralPath $BackingDirectory).Path
    $substPath = Join-Path $env:SystemRoot "System32\subst.exe"
    if (-not (Test-Path -LiteralPath $substPath -PathType Leaf)) {
        throw "Windows subst.exe was not found; a short native build path cannot be created."
    }

    $occupied = @([System.IO.DriveInfo]::GetDrives() | ForEach-Object { $_.Name.Substring(0, 2) })
    foreach ($codePoint in 90..80) {
        $drive = "$([char]$codePoint):"
        if ($occupied -contains $drive) {
            continue
        }
        & $substPath $drive $resolvedBackingDirectory
        if ($LASTEXITCODE -eq 0) {
            if (Test-Path -LiteralPath "$drive\" -PathType Container) {
                return $drive
            }
            # Do not leak a mapping if Windows accepted it but the mapped
            # storage could not be resolved for Cargo.
            & $substPath $drive /D | Out-Null
        }
    }

    throw "No free drive letter was available for the short native build path."
}

function Dismount-ShortBuildDrive {
    param([Parameter(Mandatory)][string] $Drive)

    $substPath = Join-Path $env:SystemRoot "System32\subst.exe"
    & $substPath $Drive /D
    if ($LASTEXITCODE -ne 0) {
        throw "The temporary native build drive $Drive could not be removed."
    }
}

& (Join-Path $PSScriptRoot "Enter-PhorminxDevShell.ps1") -Vulkan:(-not $Cpu)
if ($LASTEXITCODE -ne 0) {
    throw "The Phorminx development shell could not be initialized."
}

if (-not $SkipBuild) {
    $env:PHORMINX_RELEASE_VERSION = $Version
    # The Tauri context, Windows resource, and updater identity must agree.
    $env:TAURI_CONFIG = @{ version = $Version } | ConvertTo-Json -Compress

    # The per-user installer deliberately ships one executable and cannot
    # elevate to install the Microsoft Visual C++ Redistributable. Pin the
    # release build to the static MSVC CRT so it also works on a clean host.
    Remove-Item Env:CARGO_ENCODED_RUSTFLAGS -ErrorAction SilentlyContinue
    $env:RUSTFLAGS = "-C target-feature=+crt-static"
    # whisper.cpp's CMake project predates CMP0091 and otherwise emits /MD
    # objects even when Rust is using crt-static. Supply /MT explicitly for
    # native release objects; the runtime-library variable also covers nested
    # targets that opt into the modern policy.
    $env:CMAKE_MSVC_RUNTIME_LIBRARY = "MultiThreaded"
    $env:CMAKE_C_FLAGS_RELEASE = "/MT /O2 /Ob2 /DNDEBUG"
    $env:CMAKE_CXX_FLAGS_RELEASE = "/MT /O2 /Ob2 /DNDEBUG"
    $features = if ($Cpu) { "desktop,tauri-shell" } else { "desktop,vulkan,tauri-shell" }
    $shortBuildDrive = Mount-ShortBuildDrive -BackingDirectory $releaseTargetDirectory
    try {
        $env:CARGO_TARGET_DIR = "$shortBuildDrive\"
        & cargo build --locked --release --package phorminx-app --features $features
        if ($LASTEXITCODE -ne 0) {
            throw "The desktop release build failed with exit code $LASTEXITCODE."
        }
    } finally {
        Dismount-ShortBuildDrive -Drive $shortBuildDrive
    }
}

if (-not (Test-Path -LiteralPath $sourceExecutable -PathType Leaf)) {
    throw "Release executable not found. Build it first: $sourceExecutable"
}

$embeddedVersion = (Get-Item -LiteralPath $sourceExecutable).VersionInfo.ProductVersion
if ($embeddedVersion -ne $Version) {
    throw "Binary release identity '$embeddedVersion' does not match installer version '$Version'. Rebuild without -SkipBuild."
}

# Prove that the single-file installer input is not relying on a separately
# installed Visual C++ redistributable. The UCRT API-set forwarders are part
# of the operating system on our minimum supported Windows 10 version.
# This also protects -SkipBuild from
# accidentally packaging an executable produced outside the release contract.
$dumpbin = Get-Command dumpbin.exe -ErrorAction SilentlyContinue
if ($null -eq $dumpbin) {
    throw "dumpbin.exe was not found. Run from the Phorminx development shell so release dependencies can be verified."
}
$dependencyOutput = & $dumpbin.Source /nologo /dependents $sourceExecutable
if ($LASTEXITCODE -ne 0) {
    throw "dumpbin could not inspect the release executable (exit code $LASTEXITCODE)."
}
$dependencyNames = @($dependencyOutput | ForEach-Object {
    if ($_ -match '^\s*([A-Za-z0-9_.-]+\.dll)\s*$') {
        $matches[1]
    }
})
if ($dependencyNames.Count -eq 0) {
    throw "The release executable dependency table could not be read."
}
$forbiddenRuntimeDependencies = @($dependencyNames | Where-Object {
    $_ -match '^(?i:msvcp|vcruntime|concrt)'
})
if ($forbiddenRuntimeDependencies.Count -gt 0) {
    throw "Release executable depends on an unbundled Microsoft C/C++ runtime: $($forbiddenRuntimeDependencies -join ', ')"
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

# Bundle Microsoft's small Evergreen bootstrapper. The app cannot present its
# Tauri setup screen on an older Windows host until WebView2 is available.
if (-not (Test-Path -LiteralPath $webView2Bootstrapper -PathType Leaf)) {
    New-Item -ItemType Directory -Force -Path $releaseTargetDirectory | Out-Null
    Invoke-WebRequest -Uri "https://go.microsoft.com/fwlink/p/?LinkId=2124703" -OutFile $webView2Bootstrapper
}
$webView2Signature = Get-AuthenticodeSignature -LiteralPath $webView2Bootstrapper
if ($webView2Signature.Status -ne [System.Management.Automation.SignatureStatus]::Valid -or
    $null -eq $webView2Signature.SignerCertificate -or
    $webView2Signature.SignerCertificate.Subject -notmatch 'O=Microsoft Corporation') {
    throw "The WebView2 bootstrapper is not validly signed by Microsoft. Refusing to package it."
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
$resolvedWebView2 = (Resolve-Path -LiteralPath $webView2Bootstrapper).Path

$compilerArguments = @(
    "/DAppVersion=$Version",
    "/DNumericVersion=$numericVersion",
    "/DSourceExe=$resolvedSource",
    "/DWebView2Bootstrapper=$resolvedWebView2",
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
