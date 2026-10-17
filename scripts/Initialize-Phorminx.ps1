#requires -Version 5.1
<#
.SYNOPSIS
Installs an English starter bundle for an already installed Phorminx (Windows x64).
.DESCRIPTION
Run as your normal Windows user with Phorminx closed. Downloads Vosk, Whisper
base.en, the Vulkan loader, Ollama, qwen2.5:1.5b and nomic-embed-text. Vulkan may
request administrator approval. Existing settings and saved text are preserved.
This does not install Phorminx, build tools, or GPU vendor drivers.
#>
[CmdletBinding()]
param([switch] $Plan, [switch] $FromInstaller, [switch] $FromApp, [int] $HostProcessId, [string] $RunId = '')

$ErrorActionPreference = 'Stop'
Set-StrictMode -Version 3

function Write-SetupStatus([string] $State, [string] $Message, [int] $Step = 0) {
    $statusDirectory = Join-Path $env:LOCALAPPDATA 'Phorminx'
    [IO.Directory]::CreateDirectory($statusDirectory) | Out-Null
    $statusRunId = if (Get-Variable -Name RunId -ErrorAction SilentlyContinue) { $RunId } else { '' }
    $status = @{ state = $State; message = $Message; step = $Step; total = 6; run_id = $statusRunId; updated_at = [DateTime]::UtcNow.ToString('o') } | ConvertTo-Json
    [IO.File]::WriteAllText((Join-Path $statusDirectory 'setup-status.json'), $status, [Text.UTF8Encoding]::new($false))
}

function Assert-SetupNotCancelled {
    if (-not $FromApp) { return }
    $marker = Join-Path (Join-Path $env:LOCALAPPDATA 'Phorminx') ("setup-cancel-$RunId.flag")
    if (Test-Path -LiteralPath $marker -PathType Leaf) {
        Write-SetupStatus 'cancelled' 'Preparation stopped after the current step. Verified downloads are kept for retry.'
        Remove-Item -LiteralPath $marker -Force
        exit 2
    }
}

trap {
    $setupFailure = $_.Exception.Message
    try { Write-SetupStatus 'failed' $setupFailure } catch { }
    Write-Host "Setup did not finish: $setupFailure" -ForegroundColor Red
    Write-Host 'Your saved text/settings were not reset. Retry with Prepare Phorminx from the Start menu; completed verified downloads are reused.'
    if ($FromInstaller) {
        try {
            Add-Type -AssemblyName System.Windows.Forms
            [Windows.Forms.MessageBox]::Show("Setup did not finish.`r`n`r`n$setupFailure`r`n`r`nRetry using Prepare Phorminx in the Start menu. Phorminx is installed; models may still be missing.", 'Phorminx setup needs attention') | Out-Null
        } catch { }
    }
    exit 1
}

function Get-VerifiedDownload([string] $Url, [string] $Path, [long] $Bytes, [string] $Sha256) {
    if (Test-Path -LiteralPath $Path -PathType Leaf) {
        if ((Get-Item -LiteralPath $Path).Length -eq $Bytes -and
            (Get-FileHash -LiteralPath $Path -Algorithm SHA256).Hash -eq $Sha256) { return $Path }
    }
    Write-Host "Downloading $([IO.Path]::GetFileName($Path)) ..."
    $partial = $Path + '.partial'
    Invoke-WebRequest -UseBasicParsing -Uri $Url -OutFile $partial
    if ((Get-Item -LiteralPath $partial).Length -ne $Bytes -or
        (Get-FileHash -LiteralPath $partial -Algorithm SHA256).Hash -ne $Sha256) {
        throw "Download verification failed: $Url. Nothing from this download was installed."
    }
    Move-Item -LiteralPath $partial -Destination $Path -Force
    return $Path
}

function Install-VerifiedZip([string] $Archive, [string] $Root, [string] $Destination) {
    # Caller must verify the pinned archive hash before invoking this function.
    Add-Type -AssemblyName System.IO.Compression
    Add-Type -AssemblyName System.IO.Compression.FileSystem
    $base = [IO.Path]::GetFullPath($Destination).TrimEnd('\') + '\'
    $zip = [IO.Compression.ZipFile]::OpenRead($Archive)
    try {
        # Validate every path before modifying any destination files.
        $seen = [Collections.Generic.HashSet[string]]::new([StringComparer]::OrdinalIgnoreCase)
        $total = [long]0
        foreach ($entry in $zip.Entries) {
            $name = $entry.FullName.Replace('\', '/')
            $total += $entry.Length
            if ($zip.Entries.Count -gt 10000 -or $total -gt 1GB) { throw 'Archive exceeds extraction limits.' }
            if ($name -eq "$Root/") { continue }
            if (-not $name.StartsWith("$Root/", [StringComparison]::Ordinal) -or
                $name.Contains(':') -or $name.Split('/') -contains '..' -or
                (($entry.ExternalAttributes -shr 16) -band 0xF000) -eq 0xA000) {
                throw 'Unsafe archive path.'
            }
            $relative = $name.Substring($Root.Length + 1)
            $target = [IO.Path]::GetFullPath((Join-Path $base $relative))
            if (-not $target.StartsWith($base, [StringComparison]::OrdinalIgnoreCase) -or
                -not $seen.Add($target)) { throw 'Unsafe or duplicate archive path.' }
            # Do not follow pre-existing junctions or symlinks out of the asset directory.
            $ancestor = $target
            while ($ancestor) {
                if (Test-Path -LiteralPath $ancestor) {
                    if ((Get-Item -LiteralPath $ancestor -Force).Attributes -band [IO.FileAttributes]::ReparsePoint) {
                        throw "Refusing to write through a junction or symbolic link: $ancestor"
                    }
                }
                $ancestor = [IO.Path]::GetDirectoryName($ancestor)
            }
        }
        foreach ($entry in $zip.Entries) {
            $name = $entry.FullName.Replace('\', '/')
            if ($name -eq "$Root/" -or $name.EndsWith('/')) { continue }
            $target = Join-Path $base $name.Substring($Root.Length + 1)
            [IO.Directory]::CreateDirectory([IO.Path]::GetDirectoryName($target)) | Out-Null
            [IO.Compression.ZipFileExtensions]::ExtractToFile($entry, $target, $true)
        }
    } finally { $zip.Dispose() }
}

function Assert-SignedInstaller([string] $Path, [string] $PublisherPattern) {
    $signature = Get-AuthenticodeSignature -LiteralPath $Path
    if ($signature.Status -ne 'Valid' -or $signature.SignerCertificate.Subject -notmatch $PublisherPattern) {
        throw "Installer signature or publisher verification failed: $Path"
    }
}

function Get-LocalOllamaTags {
    try { return Invoke-RestMethod -Uri 'http://127.0.0.1:11434/api/tags' -TimeoutSec 3 }
    catch { return $null }
}

function New-StarterSettings([string] $Digest, [long] $Bytes) {
    $Digest = $Digest -replace '^sha256:', ''
    if ($Digest -notmatch '^[a-fA-F0-9]{64}$' -or $Bytes -le 0) { throw 'Ollama returned an invalid model identity.' }
    return @"
schema_version = 7

[recognition]
mode = "instant"
model_path = "models/ggml-base.en.bin"
instant_model_path = "models/vosk-model-small-en-us-0.15"
instant_runtime_path = "runtime/vosk"
accurate_model = "base_english"
accurate_backend = "auto"
language = "en"

[formatting]
strength = "balanced"
ollama_model = "qwen2.5:1.5b"
ollama_lifecycle = "balanced"

[formatting.ollama_model_identity]
manifest_sha256 = "$($Digest.ToLowerInvariant())"
bytes = $Bytes

[interaction]
recording_mode = "toggle"

[search]
embedding_model = "nomic-embed-text:latest"

[startup]
onboarding_complete = true
"@
}

if ($Plan) {
    Write-Output 'English starter: Vosk 0.3.45 + small-en-us-0.15; Whisper base.en; Vulkan runtime if missing; Ollama; qwen2.5:1.5b; nomic-embed-text. Creates starter settings only if none exist. Does not install Phorminx or GPU drivers.'
    return
}
if ($env:OS -ne 'Windows_NT' -or -not [Environment]::Is64BitProcess -or
    $env:PROCESSOR_ARCHITECTURE -ne 'AMD64') { throw 'Run in 64-bit PowerShell on Windows x64.' }
$appProcesses = @(Get-Process -Name 'phorminx-app' -ErrorAction SilentlyContinue)
if ($FromApp) {
    if ($HostProcessId -le 0 -or $RunId -notmatch '^[a-zA-Z0-9-]{1,80}$' -or
        $appProcesses.Count -ne 1 -or $appProcesses[0].Id -ne $HostProcessId) {
        throw 'Setup can run only from the single active Phorminx setup window.'
    }
} elseif ($appProcesses.Count -gt 0) { throw 'Close Phorminx (including the tray icon), then run this command again.' }
Write-SetupStatus 'working' 'Preparing local recognition, formatting, and search. No recordings or saved text are uploaded.' 0

[Net.ServicePointManager]::SecurityProtocol = [Net.ServicePointManager]::SecurityProtocol -bor [Net.SecurityProtocolType]::Tls12
$ProgressPreference = 'SilentlyContinue'
$data = Join-Path $env:LOCALAPPDATA 'Phorminx'
$cache = Join-Path $data 'setup-downloads'
[IO.Directory]::CreateDirectory($cache) | Out-Null
[IO.Directory]::CreateDirectory((Join-Path $data 'models')) | Out-Null

Write-SetupStatus 'working' 'Step 1 of 6 · Installing verified instant recognition.' 1
Assert-SetupNotCancelled
$runtime = Get-VerifiedDownload 'https://github.com/alphacep/vosk-api/releases/download/v0.3.45/vosk-win64-0.3.45.zip' (Join-Path $cache 'vosk-runtime.zip') 14882445 'f1dcc9cca460630f81ea8f71794f69c80bed6556d2a4e6237b5785e1d2dff34b'
$vosk = Get-VerifiedDownload 'https://alphacephei.com/vosk/models/vosk-model-small-en-us-0.15.zip' (Join-Path $cache 'vosk-english.zip') 41205931 '30f26242c4eb449f948e42cb302dd7a686cb29a3423a8367f99ff41780942498'
Install-VerifiedZip $runtime 'vosk-win64-0.3.45' (Join-Path $data 'runtime\vosk')
Install-VerifiedZip $vosk 'vosk-model-small-en-us-0.15' (Join-Path $data 'models\vosk-model-small-en-us-0.15')
Write-SetupStatus 'working' 'Step 2 of 6 · Installing verified accurate recognition.' 2
Assert-SetupNotCancelled
$null = Get-VerifiedDownload 'https://huggingface.co/ggerganov/whisper.cpp/resolve/c521a4b02f422512d734391fdf08bb08c0862f68/ggml-base.en.bin?download=true' (Join-Path $data 'models\ggml-base.en.bin') 147964211 'a03779c86df3323075f5e796cb2ce5029f00ec8869eee3fdfb897afe36c6d002'

Write-SetupStatus 'working' 'Step 3 of 6 · Checking optional Vulkan acceleration.' 3
Assert-SetupNotCancelled
if (-not (Test-Path -LiteralPath (Join-Path $env:WINDIR 'System32\vulkan-1.dll'))) {
    $vulkan = Get-VerifiedDownload 'https://sdk.lunarg.com/sdk/download/1.4.357.0/windows/VulkanRT-X64-1.4.357.0-Installer.exe' (Join-Path $cache 'VulkanRuntime.exe') 25720248 '58a9edee599e06dac0485df9ea7767016cc137c776d5d95ae08301fff2def81c'
    Assert-SignedInstaller $vulkan 'O="LunarG, Inc\."'
    Write-Host 'Installing Vulkan runtime. Approve the Windows administrator prompt if shown.'
    $install = Start-Process -FilePath $vulkan -ArgumentList '/S' -Verb RunAs -WindowStyle Hidden -PassThru
    $install.WaitForExit()
    if ($install.ExitCode -notin @(0, 3010) -or -not (Test-Path -LiteralPath (Join-Path $env:WINDIR 'System32\vulkan-1.dll'))) { throw 'Vulkan runtime installation failed. Retry after checking the installer or restarting Windows.' }
}

Write-SetupStatus 'working' 'Step 4 of 6 · Preparing the local language-model service.' 4
Assert-SetupNotCancelled
$ollama = Join-Path $env:LOCALAPPDATA 'Programs\Ollama\ollama.exe'
if (-not (Test-Path -LiteralPath $ollama)) {
    $command = Get-Command ollama.exe -ErrorAction SilentlyContinue
    if ($command) { $ollama = $command.Source }
}
if (-not (Test-Path -LiteralPath $ollama)) {
    $installer = Join-Path $cache 'OllamaSetup.exe'
    Write-Host 'Downloading and installing Ollama ...'
    Invoke-WebRequest -UseBasicParsing -Uri 'https://ollama.com/download/OllamaSetup.exe' -OutFile $installer
    Assert-SignedInstaller $installer '(^|, )O=Ollama Inc\.(,|$)'
    $install = Start-Process -FilePath $installer -ArgumentList '/VERYSILENT /NORESTART /SUPPRESSMSGBOXES' -WindowStyle Hidden -PassThru
    $install.WaitForExit()
    if ($install.ExitCode -ne 0 -or -not (Test-Path -LiteralPath $ollama)) { throw 'Ollama installation failed. No starter settings were written.' }
}

Write-SetupStatus 'working' 'Step 5 of 6 · Installing local formatting and search models.' 5
Assert-SetupNotCancelled
$oldHost = $env:OLLAMA_HOST
try {
    $env:OLLAMA_HOST = '127.0.0.1:11434'
    if (-not (Get-LocalOllamaTags)) {
        Start-Process -FilePath $ollama -ArgumentList 'serve' -WindowStyle Hidden | Out-Null
        $deadline = [DateTime]::UtcNow.AddSeconds(60)
        while (-not (Get-LocalOllamaTags)) {
            if ([DateTime]::UtcNow -gt $deadline) { throw 'Ollama did not become available on localhost:11434.' }
            Start-Sleep -Milliseconds 500
        }
    }
    foreach ($model in @('qwen2.5:1.5b', 'nomic-embed-text:latest')) {
        Assert-SetupNotCancelled
        Write-Host "Downloading local model $model ..."
        & $ollama pull $model
        if ($LASTEXITCODE -ne 0) { throw "Ollama could not download $model. Rerun this script to retry." }
        $info = Invoke-RestMethod -Uri 'http://127.0.0.1:11434/api/show' -Method Post -ContentType 'application/json' -Body (@{ model = $model } | ConvertTo-Json) -TimeoutSec 30
        if ($info.PSObject.Properties['remote_host'] -and $info.remote_host) { throw "Expected a local model, but $model is a remote model." }
        $capability = if ($model -like 'nomic-*') { 'embedding' } else { 'completion' }
        if (-not $info.PSObject.Properties['capabilities'] -or $info.capabilities -notcontains $capability) { throw "$model does not provide the expected $capability capability." }
    }
} finally { $env:OLLAMA_HOST = $oldHost }

Write-SetupStatus 'working' 'Step 6 of 6 · Validating starter settings.' 6
Assert-SetupNotCancelled
$settings = Join-Path $data 'settings.toml'
if (Test-Path -LiteralPath $settings) {
    Write-Host 'Existing settings preserved. If needed, select the downloaded models in Phorminx Settings/Models.'
} else {
    $tags = Get-LocalOllamaTags
    $selected = @($tags.models | Where-Object { $_.name -eq 'qwen2.5:1.5b' })
    if ($selected.Count -ne 1) { throw 'Could not verify the downloaded formatting model.' }
    $text = New-StarterSettings $selected[0].digest $selected[0].size
    $temporarySettings = Join-Path $data ('.starter-settings-' + [Guid]::NewGuid().ToString('N') + '.tmp')
    try {
        $stream = [IO.File]::Open($temporarySettings, [IO.FileMode]::CreateNew, [IO.FileAccess]::Write)
        $encoded = [Text.UTF8Encoding]::new($false).GetBytes($text)
        try { $stream.Write($encoded, 0, $encoded.Length); $stream.Flush($true) }
        finally { $stream.Dispose() }
        # Atomic publication; File.Move refuses to replace settings created meanwhile.
        [IO.File]::Move($temporarySettings, $settings)
    } finally {
        if (Test-Path -LiteralPath $temporarySettings) { Remove-Item -LiteralPath $temporarySettings }
    }
    Write-Host 'English starter settings created: Instant dictation, balanced formatting, local semantic search.'
}
Write-Host 'Downloads and setup complete. Open Phorminx and test dictation. GPU acceleration additionally requires a compatible GPU driver; Accurate mode can fall back to CPU.'
Write-SetupStatus 'complete' 'Starter assets installed. Existing settings were preserved where present; test recognition on this computer.' 6
