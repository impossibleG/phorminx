[CmdletBinding()]
param(
    [switch] $Vulkan,
    [switch] $Release,
    [string] $Model = (Join-Path $PSScriptRoot '../models/ggml-base.en.bin')
)
$ErrorActionPreference = 'Stop'
. (Join-Path $PSScriptRoot 'Enter-PhorminxDevShell.ps1') -Vulkan:$Vulkan
Add-Type -AssemblyName System.Speech
$fixturePath = Join-Path ([IO.Path]::GetTempPath()) ('phorminx-meeting-neutral-' + [Guid]::NewGuid().ToString('N') + '.wav')
$synth = $null
$previous = @{}
$names = @('PHORMINX_WHISPER_MODEL','PHORMINX_VOSK_RUNTIME','PHORMINX_VOSK_MODEL','PHORMINX_SPOKEN_ROLLOVER_WAV','PHORMINX_MEETING_TEST_VULKAN')
foreach ($name in $names) { $previous[$name] = [Environment]::GetEnvironmentVariable($name,'Process') }
try {
    $synth = New-Object System.Speech.Synthesis.SpeechSynthesizer
    $synth.SelectVoice('Microsoft David Desktop')
    $format = [System.Speech.AudioFormat.SpeechAudioFormatInfo]::new(16000,[System.Speech.AudioFormat.AudioBitsPerSample]::Sixteen,[System.Speech.AudioFormat.AudioChannel]::Mono)
    $synth.SetOutputToWaveFile($fixturePath,$format)
    $fixtureScript = Get-Content (Join-Path $PSScriptRoot 'Test-InstantSpokenRollover.ps1') -Raw
    $speechText = [regex]::Match($fixtureScript,"Speak\('([^']+)'\)").Groups[1].Value
    if (-not $speechText) { throw 'Neutral spoken fixture text was not found.' }
    $synth.Speak($speechText)
    $synth.Dispose()
    $synth = $null
    $env:PHORMINX_WHISPER_MODEL = (Resolve-Path -LiteralPath $Model).Path
    $env:PHORMINX_VOSK_RUNTIME = Join-Path $env:LOCALAPPDATA 'Phorminx/runtime/vosk'
    $env:PHORMINX_VOSK_MODEL = Join-Path $env:LOCALAPPDATA 'Phorminx/models/vosk-model-small-en-us-0.15'
    $env:PHORMINX_SPOKEN_ROLLOVER_WAV = $fixturePath
    if ($Vulkan) { $env:PHORMINX_MEETING_TEST_VULKAN = '1' } else { Remove-Item Env:PHORMINX_MEETING_TEST_VULKAN -ErrorAction SilentlyContinue }
    Push-Location (Split-Path $PSScriptRoot -Parent)
    try {
        $testArguments = @('test','-p','phorminx-app','--lib')
        if ($Release) { $testArguments += '--release' }
        if ($Vulkan) { $testArguments += @('--features','vulkan') }
        $testArguments += @('native_meeting_','--','--ignored','--nocapture','--test-threads=1')
        & cargo @testArguments
        if ($LASTEXITCODE -ne 0) { throw 'Meeting native streaming regression failed.' }
    } finally { Pop-Location }
} finally {
    if ($null -ne $synth) { $synth.Dispose() }
    foreach ($name in $names) { [Environment]::SetEnvironmentVariable($name,$previous[$name],'Process') }
    # This exact generated WAV is owned by this invocation, never a personal recording.
    if (Test-Path -LiteralPath $fixturePath) { Remove-Item -LiteralPath $fixturePath }
}
