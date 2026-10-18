<#
.SYNOPSIS
Measures how long Phorminx takes to open, using an isolated copy of a profile.

.DESCRIPTION
Copies settings.toml (and phorminx.db, if present) from -Profile into a new
temporary folder, starts -Exe with --config pointing there, and reads the
startup_phase events the app writes to logs\lifecycle.log. Each phase is the
time since launch: settings, model_verified, database, recognizer, window.
The app is stopped after each run. Fails when the window phase exceeds
-MaxSeconds in any run.

Never point -Profile at the installed app's own folder while it is running;
the copy is what runs, but a live database may be copied mid-write.
#>
param(
    [Parameter(Mandatory)] [string] $Exe,
    [Parameter(Mandatory)] [string] $Profile,
    [int] $Runs = 3,
    [double] $MaxSeconds = 20
)
$ErrorActionPreference = 'Stop'
$failed = $false
for ($run = 1; $run -le $Runs; $run++) {
    $work = Join-Path ([IO.Path]::GetTempPath()) "phorminx-startup-$PID-$run"
    New-Item -ItemType Directory -Force $work | Out-Null
    Copy-Item (Join-Path $Profile 'settings.toml') $work
    foreach ($name in 'phorminx.db', 'phorminx.db-wal', 'phorminx.db-shm') {
        $source = Join-Path $Profile $name
        if (Test-Path $source) { Copy-Item $source $work }
    }
    $env:WEBVIEW2_USER_DATA_FOLDER = Join-Path $work 'webview2'
    $process = Start-Process -FilePath $Exe -ArgumentList @('--config', (Join-Path $work 'settings.toml'), '--background') -PassThru -WindowStyle Hidden
    $log = Join-Path $work 'logs\lifecycle.log'
    $phases = [ordered]@{}
    $deadline = (Get-Date).AddSeconds([Math]::Max(60, $MaxSeconds * 3))
    while ((Get-Date) -lt $deadline -and -not $phases.Contains('window') -and -not $process.HasExited) {
        Start-Sleep -Milliseconds 250
        if (Test-Path $log) {
            foreach ($line in Get-Content $log) {
                if ($line -match 'event=startup_phase phase=(\w+) ms=(\d+)') { $phases[$Matches[1]] = [int]$Matches[2] }
            }
        }
    }
    if (-not $process.HasExited) { Stop-Process -Id $process.Id -Force }
    $summary = ($phases.GetEnumerator() | ForEach-Object { '{0}={1:N1}s' -f $_.Key, ($_.Value / 1000) }) -join ' '
    if (-not $phases.Contains('window')) {
        "run ${run}: no window within the deadline ($summary)"
        $failed = $true
    } else {
        $seconds = $phases['window'] / 1000
        $verdict = if ($seconds -le $MaxSeconds) { 'ok' } else { $failed = $true; "over $MaxSeconds s" }
        "run ${run}: $summary ($verdict)"
    }
    Start-Sleep -Seconds 2
    Remove-Item -Recurse -Force $work -ErrorAction SilentlyContinue
}
if ($failed) { exit 1 }
