[CmdletBinding()]
param(
    [Parameter(Mandatory)]
    [string] $OutputPath,
    [string] $ApplicationPath,
    [string] $SettingsPath
)

$ErrorActionPreference = "Stop"
Set-StrictMode -Version Latest

function Write-Utf8NoBom {
    param([string] $Path, [string] $Content)
    $encoding = New-Object System.Text.UTF8Encoding($false)
    [System.IO.File]::WriteAllText($Path, $Content, $encoding)
}

function Get-SafeApplicationSummary {
    param([string] $Path)

    if ([string]::IsNullOrWhiteSpace($Path) -or -not (Test-Path -LiteralPath $Path -PathType Leaf)) {
        return [ordered]@{
            present = $false
            version = $null
            signature_status = "Unavailable"
        }
    }

    $item = Get-Item -LiteralPath $Path
    $signature = Get-AuthenticodeSignature -LiteralPath $Path
    return [ordered]@{
        present = $true
        version = $item.VersionInfo.ProductVersion
        signature_status = $signature.Status.ToString()
    }
}

function Test-LoopbackPort {
    param([int] $Port)

    $client = [System.Net.Sockets.TcpClient]::new()
    try {
        $operation = $client.ConnectAsync([System.Net.IPAddress]::Loopback, $Port)
        return $operation.Wait(250) -and $client.Connected
    } catch {
        return $false
    } finally {
        $client.Dispose()
    }
}

$resolvedOutput = [System.IO.Path]::GetFullPath($OutputPath)
if ([System.IO.Path]::GetExtension($resolvedOutput) -ne ".zip") {
    throw "OutputPath must end in .zip."
}
if (Test-Path -LiteralPath $resolvedOutput) {
    throw "Refusing to overwrite an existing diagnostic bundle: $resolvedOutput"
}

$outputParent = Split-Path -Parent $resolvedOutput
if ([string]::IsNullOrWhiteSpace($outputParent)) {
    throw "OutputPath must include a parent directory."
}
New-Item -ItemType Directory -Force -Path $outputParent | Out-Null

$temporaryRoot = Join-Path ([System.IO.Path]::GetTempPath()) ("phorminx-diagnostics-" + [guid]::NewGuid().ToString("N"))
New-Item -ItemType Directory -Path $temporaryRoot | Out-Null

try {
    $culture = [System.Globalization.CultureInfo]::CurrentCulture
    $os = [System.Environment]::OSVersion
    $system = [ordered]@{
        schema_version = 1
        generated_utc = [DateTimeOffset]::UtcNow.ToString("O")
        os_platform = $os.Platform.ToString()
        os_version = $os.Version.ToString()
        is_64_bit_os = [Environment]::Is64BitOperatingSystem
        process_architecture = [System.Runtime.InteropServices.RuntimeInformation]::ProcessArchitecture.ToString()
        locale = $culture.Name
        timezone_utc_offset_minutes = [int][TimeZoneInfo]::Local.GetUtcOffset([DateTimeOffset]::Now).TotalMinutes
        powershell_version = $PSVersionTable.PSVersion.ToString()
        logical_processor_count = [Environment]::ProcessorCount
    }

    $runtime = [ordered]@{
        schema_version = 1
        application = Get-SafeApplicationSummary -Path $ApplicationPath
        settings_present = (-not [string]::IsNullOrWhiteSpace($SettingsPath)) -and (Test-Path -LiteralPath $SettingsPath -PathType Leaf)
        phorminx_process_count = @(Get-Process -Name "phorminx-app" -ErrorAction SilentlyContinue).Count
        ollama_loopback_reachable = Test-LoopbackPort -Port 11434
    }

    $manifest = [ordered]@{
        schema_version = 1
        privacy_profile = "content-free-default"
        included_files = @("system.json", "runtime.json", "manifest.json")
        excluded_by_design = @(
            "audio",
            "transcripts and transformed text",
            "clipboard contents",
            "window titles and focused-control text",
            "user and machine names",
            "filesystem paths",
            "custom formatting instructions",
            "model names and prompts",
            "environment variables",
            "raw application and Windows event logs"
        )
    }

    Write-Utf8NoBom -Path (Join-Path $temporaryRoot "system.json") -Content ($system | ConvertTo-Json -Depth 6)
    Write-Utf8NoBom -Path (Join-Path $temporaryRoot "runtime.json") -Content ($runtime | ConvertTo-Json -Depth 6)
    Write-Utf8NoBom -Path (Join-Path $temporaryRoot "manifest.json") -Content ($manifest | ConvertTo-Json -Depth 6)

    Compress-Archive -LiteralPath (Join-Path $temporaryRoot "system.json"), (Join-Path $temporaryRoot "runtime.json"), (Join-Path $temporaryRoot "manifest.json") -DestinationPath $resolvedOutput -CompressionLevel Optimal
    $bundle = Get-Item -LiteralPath $resolvedOutput
    $hash = Get-FileHash -LiteralPath $resolvedOutput -Algorithm SHA256
    [pscustomobject]@{
        Bundle = $bundle.FullName
        Bytes = $bundle.Length
        Sha256 = $hash.Hash
        PrivacyProfile = "content-free-default"
    }
} finally {
    if (Test-Path -LiteralPath $temporaryRoot) {
        Remove-Item -LiteralPath $temporaryRoot -Recurse -Force
    }
}
