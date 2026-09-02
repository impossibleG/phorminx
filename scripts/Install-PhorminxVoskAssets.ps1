[CmdletBinding()]
param(
    [Parameter(Mandatory)] [string] $RuntimeArchive,
    [Parameter(Mandatory)] [string] $ModelArchive,
    [string] $DestinationRoot = (Join-Path $env:LOCALAPPDATA 'Phorminx')
)

$ErrorActionPreference = 'Stop'
$runtimeHash = 'F1DCC9CCA460630F81EA8F71794F69C80BED6556D2A4E6237B5785E1D2DFF34B'
$modelHash = '30F26242C4EB449F948E42CB302DD7A686CB29A3423A8367F99FF41780942498'
$runtimeBytes = 14882445
$modelBytes = 41205931
$maximumEntries = 10000
$maximumExpandedBytes = 1024MB

function Assert-Archive([string] $Path, [long] $Length, [string] $Sha256) {
    $file = Get-Item -LiteralPath $Path -ErrorAction Stop
    if (-not $file.PSIsContainer -and $file.Length -eq $Length) {
        $actual = (Get-FileHash -LiteralPath $file.FullName -Algorithm SHA256).Hash
        if ($actual -eq $Sha256) { return $file.FullName }
    }
    throw "Archive verification failed: $Path"
}

function Expand-VerifiedZip(
    [string] $Archive,
    [string] $Stage,
    [string] $ExpectedRoot
) {
    Add-Type -AssemblyName System.IO.Compression.FileSystem
    $stageFull = [IO.Path]::GetFullPath($Stage).TrimEnd('\') + '\'
    [IO.Directory]::CreateDirectory($stageFull) | Out-Null
    $seen = [Collections.Generic.HashSet[string]]::new([StringComparer]::OrdinalIgnoreCase)
    $zip = [IO.Compression.ZipFile]::OpenRead($Archive)
    try {
        if ($zip.Entries.Count -gt $maximumEntries) { throw 'Archive contains too many entries.' }
        $expanded = [long]0
        foreach ($entry in $zip.Entries) {
            $name = $entry.FullName.Replace('/', '\')
            if ([IO.Path]::IsPathRooted($name) -or $name.Contains(':')) {
                throw "Archive contains a rooted path."
            }
            $segments = $name.Split('\', [StringSplitOptions]::RemoveEmptyEntries)
            if ($segments.Count -eq 0 -or $segments[0] -ne $ExpectedRoot -or $segments -contains '..') {
                throw "Archive entry escaped or did not match its pinned root."
            }
            $target = [IO.Path]::GetFullPath((Join-Path $stageFull $name))
            if (-not $target.StartsWith($stageFull, [StringComparison]::OrdinalIgnoreCase)) {
                throw 'Archive entry escaped the staging directory.'
            }
            if (-not $seen.Add($target)) { throw 'Archive contains duplicate paths.' }
            $unixMode = ($entry.ExternalAttributes -shr 16) -band 0xF000
            if ($unixMode -eq 0xA000) { throw 'Archive contains a symbolic link.' }
            $expanded += $entry.Length
            if ($expanded -gt $maximumExpandedBytes) { throw 'Archive expands beyond the safety limit.' }
            if ($name.EndsWith('\')) {
                [IO.Directory]::CreateDirectory($target) | Out-Null
            } else {
                [IO.Directory]::CreateDirectory([IO.Path]::GetDirectoryName($target)) | Out-Null
                $input = $entry.Open()
                $output = [IO.File]::Open($target, [IO.FileMode]::CreateNew, [IO.FileAccess]::Write)
                try { $input.CopyTo($output) } finally { $output.Dispose(); $input.Dispose() }
            }
        }
    } finally {
        $zip.Dispose()
    }
    return (Join-Path $stageFull $ExpectedRoot)
}

$runtimeArchivePath = Assert-Archive $RuntimeArchive $runtimeBytes $runtimeHash
$modelArchivePath = Assert-Archive $ModelArchive $modelBytes $modelHash
$destination = [IO.Path]::GetFullPath($DestinationRoot)
[IO.Directory]::CreateDirectory($destination) | Out-Null
$runtimeTarget = Join-Path $destination 'runtime\vosk'
$modelTarget = Join-Path $destination 'models\vosk-model-small-en-us-0.15'
if ((Test-Path -LiteralPath $runtimeTarget) -or (Test-Path -LiteralPath $modelTarget)) {
    throw 'A destination already exists. Remove it explicitly before reinstalling.'
}

$stage = Join-Path $destination ('.vosk-stage-' + [Guid]::NewGuid().ToString('N'))
try {
    $runtimeSource = Expand-VerifiedZip $runtimeArchivePath (Join-Path $stage 'runtime') 'vosk-win64-0.3.45'
    $modelSource = Expand-VerifiedZip $modelArchivePath (Join-Path $stage 'model') 'vosk-model-small-en-us-0.15'
    foreach ($required in 'libvosk.dll','libgcc_s_seh-1.dll','libstdc++-6.dll','libwinpthread-1.dll') {
        if (-not (Test-Path -LiteralPath (Join-Path $runtimeSource $required) -PathType Leaf)) {
            throw "Runtime is missing a pinned required file."
        }
    }
    [IO.Directory]::CreateDirectory([IO.Path]::GetDirectoryName($runtimeTarget)) | Out-Null
    [IO.Directory]::CreateDirectory([IO.Path]::GetDirectoryName($modelTarget)) | Out-Null
    Move-Item -LiteralPath $runtimeSource -Destination $runtimeTarget
    try {
        Move-Item -LiteralPath $modelSource -Destination $modelTarget
    } catch {
        Remove-Item -LiteralPath $runtimeTarget -Recurse -Force
        throw
    }
} finally {
    if (Test-Path -LiteralPath $stage) { Remove-Item -LiteralPath $stage -Recurse -Force }
}

Write-Host "Verified Vosk runtime: $runtimeTarget"
Write-Host "Verified English model: $modelTarget"
