[CmdletBinding()]
param(
    [Parameter(Mandatory)] [string] $Version,
    # Must be greater than every sequence published before; the updater
    # refuses anything at or below the highest sequence it has seen.
    [Parameter(Mandatory)] [UInt64] $Sequence,
    # Absolute path to the offline Ed25519 private key, outside the repository.
    [Parameter(Mandatory)] [string] $PrivateKeyPath,
    [string] $InstallerDirectory,
    [string] $ReleaseTool,
    [string] $NotesFile,
    # Without -Publish the GitHub Release stays a draft for review.
    [switch] $Publish
)

$ErrorActionPreference = "Stop"
Set-StrictMode -Version Latest

$repositoryRoot = Split-Path -Parent $PSScriptRoot
$repository = "impossibleG/phorminx"
if ([string]::IsNullOrWhiteSpace($InstallerDirectory)) {
    $InstallerDirectory = Join-Path $repositoryRoot "artifacts\installer"
}
if ([string]::IsNullOrWhiteSpace($ReleaseTool)) {
    $ReleaseTool = Join-Path $env:LOCALAPPDATA "PhorminxBuild\short-v2\release\phorminx_release.exe"
}
if ($Version -notmatch '^\d+\.\d+\.\d+$') {
    throw "Publish only stable versions such as 0.2.0; the updater ignores pre-releases."
}
foreach ($required in @($PrivateKeyPath, $ReleaseTool)) {
    if (-not (Test-Path -LiteralPath $required -PathType Leaf)) {
        throw "Required file not found: $required"
    }
}
$gh = Get-Command gh -ErrorAction SilentlyContinue
if ($null -eq $gh) {
    throw "GitHub CLI (gh) was not found. Install it and run 'gh auth login'."
}

# Tag the commit the installer was built from, which must already be pushed.
$commit = (& git -C $repositoryRoot rev-parse HEAD).Trim()
if (-not (& git -C $repositoryRoot branch -r --contains $commit)) {
    throw "Commit $commit is not on any pushed branch. Push it before publishing."
}
if (& git -C $repositoryRoot status --porcelain --untracked-files=no) {
    throw "The working tree has uncommitted changes; the tag would not match the build."
}

$installer = Join-Path $InstallerDirectory "Phorminx-$Version-x64-setup.exe"
if (-not (Test-Path -LiteralPath $installer -PathType Leaf)) {
    throw "Installer not found: $installer. Build it with Build-PhorminxInstaller.ps1 -Version $Version."
}
# Inno Setup stamps a padded four-part version such as "0.2.0.0".
$embedded = "$((Get-Item -LiteralPath $installer).VersionInfo.ProductVersion)".Trim()
if ($embedded -and $embedded -notin @($Version, "$Version.0")) {
    throw "Installer reports version '$embedded', not '$Version'."
}

# Stage the three uploads in a fresh directory; the sign tool never overwrites.
$staging = Join-Path $InstallerDirectory "release-v$Version"
if (Test-Path -LiteralPath $staging) {
    throw "$staging already exists. Remove it only if that release was never published."
}
New-Item -ItemType Directory -Path $staging | Out-Null
$versioned = Join-Path $staging "Phorminx-$Version-x64-setup.exe"
Copy-Item -LiteralPath $installer -Destination $versioned
$manifest = Join-Path $staging "phorminx-update.json"
& $ReleaseTool sign $PrivateKeyPath $Version $Sequence $versioned $manifest
if ($LASTEXITCODE -ne 0) {
    throw "Signing the release manifest failed with exit code $LASTEXITCODE."
}
# A fixed name lets the website link to releases/latest/download/... forever.
$fixed = Join-Path $staging "Phorminx-x64-setup.exe"
Copy-Item -LiteralPath $versioned -Destination $fixed

$arguments = @(
    "release", "create", "v$Version", $versioned, $manifest, $fixed,
    "--repo", $repository,
    "--target", $commit,
    "--title", "Phorminx $Version",
    "--latest"
)
if ([string]::IsNullOrWhiteSpace($NotesFile)) {
    $arguments += @("--generate-notes")
} else {
    $arguments += @("--notes-file", $NotesFile)
}
if (-not $Publish) {
    $arguments += "--draft"
}
& $gh.Source @arguments
if ($LASTEXITCODE -ne 0) {
    throw "gh release create failed with exit code $LASTEXITCODE."
}

$hash = Get-FileHash -LiteralPath $versioned -Algorithm SHA256
[pscustomobject]@{
    Tag = "v$Version"
    Sequence = $Sequence
    Draft = -not $Publish
    Sha256 = $hash.Hash
    Staging = $staging
}
