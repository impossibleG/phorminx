[CmdletBinding(DefaultParameterSetName = "List")]
param(
    [Parameter(ParameterSetName = "List")]
    [switch] $List,

    [Parameter(Mandatory, ParameterSetName = "Start")]
    [switch] $Start,
    [Parameter(Mandatory, ParameterSetName = "Start")]
    [string] $RunDirectory,

    [Parameter(Mandatory, ParameterSetName = "Record")]
    [switch] $Record,
    [Parameter(Mandatory, ParameterSetName = "Record")]
    [string] $RunFile,
    [Parameter(Mandatory, ParameterSetName = "Record")]
    [ValidateSet(
        "sleep_resume", "microphone_hotplug", "default_device_change", "focus_change",
        "closed_target", "clipboard_race", "elevated_target", "password_field",
        "multi_monitor_dpi", "ollama_unavailable", "ollama_model_eviction"
    )]
    [string] $Scenario,
    [Parameter(Mandatory, ParameterSetName = "Record")]
    [ValidateSet("pass", "fail", "blocked", "not_applicable")]
    [string] $Outcome,
    [Parameter(ParameterSetName = "Record")]
    [ValidateSet(
        "none", "app_crash", "app_hang", "capture_unavailable", "clipped_audio",
        "wrong_target", "clipboard_changed", "unsafe_insertion", "fallback_missing",
        "recovery_failed", "unexpected_network", "performance_regression", "other"
    )]
    [string[]] $IssueCode = @("none"),

    [Parameter(Mandatory, ParameterSetName = "Summary")]
    [switch] $Summary,
    [Parameter(Mandatory, ParameterSetName = "Summary")]
    [string] $SummaryRunFile
)

$ErrorActionPreference = "Stop"
Set-StrictMode -Version Latest

function Write-Utf8NoBom {
    param([string] $Path, [string] $Content)
    $encoding = New-Object System.Text.UTF8Encoding($false)
    [System.IO.File]::WriteAllText($Path, $Content, $encoding)
}

$scenarioDefinitions = @(
    [ordered]@{ id = "sleep_resume"; category = "lifecycle"; requires_physical_test = $true },
    [ordered]@{ id = "microphone_hotplug"; category = "audio"; requires_physical_test = $true },
    [ordered]@{ id = "default_device_change"; category = "audio"; requires_physical_test = $true },
    [ordered]@{ id = "focus_change"; category = "insertion"; requires_physical_test = $true },
    [ordered]@{ id = "closed_target"; category = "insertion"; requires_physical_test = $true },
    [ordered]@{ id = "clipboard_race"; category = "insertion"; requires_physical_test = $true },
    [ordered]@{ id = "elevated_target"; category = "security"; requires_physical_test = $true },
    [ordered]@{ id = "password_field"; category = "security"; requires_physical_test = $true },
    [ordered]@{ id = "multi_monitor_dpi"; category = "display"; requires_physical_test = $true },
    [ordered]@{ id = "ollama_unavailable"; category = "cleanup"; requires_physical_test = $true },
    [ordered]@{ id = "ollama_model_eviction"; category = "cleanup"; requires_physical_test = $true }
)

function Read-RunFile {
    param([string] $Path)
    if (-not (Test-Path -LiteralPath $Path -PathType Leaf)) {
        throw "Compatibility run file not found: $Path"
    }
    return Get-Content -LiteralPath $Path -Raw | ConvertFrom-Json
}

function Write-RunFile {
    param([object] $Run, [string] $Path)
    $temporaryPath = "$Path.tmp"
    Write-Utf8NoBom -Path $temporaryPath -Content ($Run | ConvertTo-Json -Depth 8)
    Move-Item -LiteralPath $temporaryPath -Destination $Path -Force
}

switch ($PSCmdlet.ParameterSetName) {
    "List" {
        $scenarioDefinitions | ForEach-Object { [pscustomobject]$_ }
    }
    "Start" {
        New-Item -ItemType Directory -Force -Path $RunDirectory | Out-Null
        $resolvedDirectory = (Resolve-Path -LiteralPath $RunDirectory).Path
        $runId = [DateTimeOffset]::UtcNow.ToString("yyyyMMddTHHmmssZ") + "-" + [guid]::NewGuid().ToString("N").Substring(0, 8)
        $runPath = Join-Path $resolvedDirectory "compatibility-$runId.json"
        $run = [ordered]@{
            schema_version = 1
            run_id = $runId
            started_utc = [DateTimeOffset]::UtcNow.ToString("O")
            privacy_profile = "content-free"
            environment = [ordered]@{
                os_version = [Environment]::OSVersion.Version.ToString()
                process_architecture = [System.Runtime.InteropServices.RuntimeInformation]::ProcessArchitecture.ToString()
            }
            scenarios = @($scenarioDefinitions | ForEach-Object {
                [ordered]@{
                    id = $_.id
                    category = $_.category
                    requires_physical_test = $_.requires_physical_test
                    outcome = "pending"
                    issue_codes = @()
                    recorded_utc = $null
                }
            })
        }
        Write-RunFile -Run $run -Path $runPath
        Get-Item -LiteralPath $runPath
    }
    "Record" {
        $resolvedRunFile = (Resolve-Path -LiteralPath $RunFile).Path
        $run = Read-RunFile -Path $resolvedRunFile
        $entry = @($run.scenarios | Where-Object { $_.id -eq $Scenario })
        if ($entry.Count -ne 1) {
            throw "Scenario '$Scenario' is missing or duplicated in the run file."
        }
        if ($Outcome -eq "pass" -and @($IssueCode | Where-Object { $_ -ne "none" }).Count -gt 0) {
            throw "A passing scenario cannot contain failure issue codes."
        }
        $entry[0].outcome = $Outcome
        $entry[0].issue_codes = @($IssueCode | Where-Object { $_ -ne "none" } | Select-Object -Unique)
        $entry[0].recorded_utc = [DateTimeOffset]::UtcNow.ToString("O")
        Write-RunFile -Run $run -Path $resolvedRunFile
        $entry[0]
    }
    "Summary" {
        $run = Read-RunFile -Path $SummaryRunFile
        $groups = @($run.scenarios | Group-Object outcome | ForEach-Object {
            [pscustomobject]@{ outcome = $_.Name; count = $_.Count }
        })
        [pscustomobject]@{
            run_id = $run.run_id
            total = @($run.scenarios).Count
            complete = @($run.scenarios | Where-Object { $_.outcome -ne "pending" }).Count
            failures = @($run.scenarios | Where-Object { $_.outcome -eq "fail" }).Count
            outcomes = $groups
        }
    }
}
