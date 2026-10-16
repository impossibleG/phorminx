#requires -Version 5.1
[CmdletBinding()]
param()
$ErrorActionPreference = 'Stop'
Set-StrictMode -Version 3
$scriptPath = Join-Path $PSScriptRoot 'Initialize-Phorminx.ps1'
$tokens = $null
$errors = $null
$ast = [Management.Automation.Language.Parser]::ParseFile($scriptPath, [ref]$tokens, [ref]$errors)
if ($errors.Count) { throw ($errors | Out-String) }
# Import only function definitions: never run the real setup/install entry point.
foreach ($definition in $ast.FindAll({ param($node) $node -is [Management.Automation.Language.FunctionDefinitionAst] }, $false)) {
    . ([scriptblock]::Create($definition.Extent.Text))
}
function Assert-True([bool] $Condition, [string] $Message) {
    if (-not $Condition) { throw $Message }
}
function Assert-Throws([scriptblock] $Action, [string] $Message) {
    $failed = $false
    try { & $Action | Out-Null } catch { $failed = $true }
    Assert-True $failed $Message
}
Add-Type -AssemblyName System.IO.Compression
Add-Type -AssemblyName System.IO.Compression.FileSystem
$testRoot = Join-Path ([IO.Path]::GetTempPath()) ('phorminx-bootstrap-test-' + [Guid]::NewGuid().ToString('N'))
[IO.Directory]::CreateDirectory($testRoot) | Out-Null
function New-TestZip([string] $Path, [string[]] $Names) {
    $zip = [IO.Compression.ZipFile]::Open($Path, [IO.Compression.ZipArchiveMode]::Create)
    try {
        foreach ($name in $Names) {
            $entry = $zip.CreateEntry($name)
            $writer = [IO.StreamWriter]::new($entry.Open())
            try { $writer.Write('synthetic fixture') } finally { $writer.Dispose() }
        }
    } finally { $zip.Dispose() }
}
try {
    # Status reporting is tested against an isolated destination, never the real
    # LocalAppData profile. Do not execute the installer entry point or dialog.
    & {
        $RunId = 'synthetic-42'
        function Join-Path {
            param([string] $Path, [string] $ChildPath)
            if ($Path -eq $env:LOCALAPPDATA) {
                return [IO.Path]::Combine($testRoot, 'fake-local', $ChildPath)
            }
            return [IO.Path]::Combine($Path, $ChildPath)
        }
        Write-SetupStatus 'failed' 'synthetic download interruption' 2
        $statusPath = [IO.Path]::Combine($testRoot, 'fake-local', 'Phorminx', 'setup-status.json')
        $status = Get-Content -Raw -LiteralPath $statusPath | ConvertFrom-Json
        Assert-True ($status.state -eq 'failed') 'Failure status was not retained for retry.'
        Assert-True ($status.message -eq 'synthetic download interruption') 'Failure status message changed.'
        Assert-True ($status.run_id -eq $RunId -and $status.step -eq 2 -and $status.total -eq 6) 'App progress receipt is incomplete.'
        Write-SetupStatus 'complete' 'synthetic setup completed'
        $status = Get-Content -Raw -LiteralPath $statusPath | ConvertFrom-Json
        Assert-True ($status.state -eq 'complete') 'Retry completion did not replace failure status.'
    }
    $zipPath = Join-Path $testRoot 'valid.zip'
    New-TestZip $zipPath @('root/sub/file.txt')
    $destination = Join-Path $testRoot 'assets'
    Install-VerifiedZip $zipPath 'root' $destination
    Install-VerifiedZip $zipPath 'root' $destination
    Assert-True ((Get-Content -Raw -LiteralPath (Join-Path $destination 'sub/file.txt')) -eq 'synthetic fixture') 'Extraction/rerun failed.'
    foreach ($case in @(
        @{ Name = 'traversal'; Paths = @('root/good.txt', 'root/../../escape.txt') },
        @{ Name = 'absolute'; Paths = @('C:/escape.txt') },
        @{ Name = 'wrong-root'; Paths = @('other/file.txt') },
        @{ Name = 'duplicate'; Paths = @('root/file.txt', 'root/FILE.txt') },
        @{ Name = 'stream'; Paths = @('root/file.txt:stream') }
    )) {
        $badZip = Join-Path $testRoot ($case.Name + '.zip')
        New-TestZip $badZip $case.Paths
        $badDestination = Join-Path $testRoot $case.Name
        Assert-Throws { Install-VerifiedZip $badZip 'root' $badDestination } "Unsafe archive accepted: $($case.Name)"
        Assert-True (-not (Test-Path -LiteralPath $badDestination)) 'Extraction wrote files before validating the entire archive.'
    }
    # Mock networking: a cached valid download must not contact the network.
    function Invoke-WebRequest { throw 'Unexpected network request in test.' }
    $hash = (Get-FileHash -LiteralPath $zipPath -Algorithm SHA256).Hash
    $bytes = (Get-Item -LiteralPath $zipPath).Length
    Assert-True ((Get-VerifiedDownload 'https://invalid.example/test' $zipPath $bytes $hash) -eq $zipPath) 'Verified cache was not reused.'
    Assert-Throws { Get-VerifiedDownload 'https://invalid.example/test' $zipPath ($bytes + 1) $hash } 'Wrong length accepted.'
    Assert-Throws { Get-VerifiedDownload 'https://invalid.example/test' $zipPath $bytes ('0' * 64) } 'Wrong hash accepted.'
    # Explicit switch matches the real invocation; return invalid download bytes.
    function Invoke-WebRequest { param([switch]$UseBasicParsing, $Uri, $OutFile) [IO.File]::WriteAllText($OutFile, 'corrupt') }
    $downloadTarget = Join-Path $testRoot 'new-download.zip'
    Assert-Throws { Get-VerifiedDownload 'https://invalid.example/test' $downloadTarget 7 ('0' * 64) } 'Corrupt download accepted.'
    Assert-True (-not (Test-Path -LiteralPath $downloadTarget)) 'Corrupt download was promoted.'
    $settings = New-StarterSettings ('sha256:' + ('A' * 64)) 12345
    Assert-True ($settings.Contains('manifest_sha256 = "' + ('a' * 64) + '"')) 'Digest normalization failed.'
    Assert-True ($settings.Contains('mode = "instant"')) 'Starter recognition mode missing.'
    Assert-Throws { New-StarterSettings 'invalid' 123 } 'Invalid model digest accepted.'
    Assert-Throws { New-StarterSettings ('a' * 64) 0 } 'Invalid model size accepted.'
    function Get-AuthenticodeSignature { param($LiteralPath) return $script:testSignature }
    $script:testSignature = [pscustomobject]@{ Status = 'Valid'; SignerCertificate = [pscustomobject]@{ Subject = 'CN=Ollama Inc., O=Ollama Inc., C=US' } }
    Assert-SignedInstaller 'synthetic.exe' '(^|, )O=Ollama Inc\.(,|$)'
    Assert-Throws { Assert-SignedInstaller 'synthetic.exe' 'O="LunarG, Inc\."' } 'Wrong publisher accepted.'
    $script:testSignature.Status = 'NotSigned'
    Assert-Throws { Assert-SignedInstaller 'synthetic.exe' '(^|, )O=Ollama Inc\.(,|$)' } 'Unsigned installer accepted.'
    # Validate the exact generated TOML using Python if available (no third-party packages).
    if (Get-Command python -ErrorAction SilentlyContinue) {
        $settings | python -c "import sys,tomllib; s=tomllib.loads(sys.stdin.read()); assert s['schema_version'] == 7; assert s['formatting']['ollama_model_identity']['bytes'] == 12345; assert s['search']['embedding_model'] == 'nomic-embed-text:latest'"
        if ($LASTEXITCODE -ne 0) { throw 'Generated TOML failed validation.' }
    }
    Write-Host 'PASS: parse, extraction/rerun, five unsafe archive cases, cache reuse, bad length/hash, corrupt download, publisher/signature checks, model identity and starter TOML. No installers or model pulls were run.'
} finally {
    $resolved = [IO.Path]::GetFullPath($testRoot)
    $tempBase = [IO.Path]::GetFullPath([IO.Path]::GetTempPath()).TrimEnd('\') + '\'
    if (-not $resolved.StartsWith($tempBase, [StringComparison]::OrdinalIgnoreCase) -or
        [IO.Path]::GetFileName($resolved) -notlike 'phorminx-bootstrap-test-*') { throw 'Unsafe test cleanup path.' }
    Remove-Item -LiteralPath $resolved -Recurse -Force
}
