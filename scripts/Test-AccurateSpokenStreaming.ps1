[CmdletBinding()]
param([switch] $Vulkan, [switch] $Release, [string] $Model = (Join-Path $PSScriptRoot '../models/ggml-base.en.bin'))
$ErrorActionPreference = 'Stop'
. (Join-Path $PSScriptRoot 'Enter-PhorminxDevShell.ps1') -Vulkan:$Vulkan
Add-Type -AssemblyName System.Speech
if (-not ('PhorminxNativeStreamingTestPower' -as [type])) {
    Add-Type 'public static class PhorminxNativeStreamingTestPower { [System.Runtime.InteropServices.DllImport("kernel32.dll")] public static extern uint SetThreadExecutionState(uint state); }'
}
$previousExecutionState = 0
$fixturePath = Join-Path ([IO.Path]::GetTempPath()) ('phorminx-accurate-neutral-' + [Guid]::NewGuid().ToString('N') + '.wav')
$synth = $null
$previousModel = $env:PHORMINX_WHISPER_MODEL
$previousFixture = $env:PHORMINX_SPOKEN_ROLLOVER_WAV
$previousReference = $env:PHORMINX_ACCURATE_REFERENCE
try {
    $previousExecutionState = [PhorminxNativeStreamingTestPower]::SetThreadExecutionState([uint32]2147483649)
    if ($previousExecutionState -eq 0) { throw 'Could not hold the temporary native-test awake request.' }
    $synth = New-Object System.Speech.Synthesis.SpeechSynthesizer
    $synth.SelectVoice('Microsoft David Desktop')
    $format = [System.Speech.AudioFormat.SpeechAudioFormatInfo]::new(16000,[System.Speech.AudioFormat.AudioBitsPerSample]::Sixteen,[System.Speech.AudioFormat.AudioChannel]::Mono)
    $synth.SetOutputToWaveFile($fixturePath,$format)
    # Reuse the established neutral speech fixture, never personal dictation.
    $fixtureScript = Get-Content (Join-Path $PSScriptRoot 'Test-InstantSpokenRollover.ps1') -Raw
    $speechText = [regex]::Match($fixtureScript,"Speak\('([^']+)'\)").Groups[1].Value
    if (-not $speechText) { throw 'Neutral spoken fixture text was not found.' }
    $synth.Speak($speechText)
    $synth.Dispose()
    $synth = $null
    $env:PHORMINX_WHISPER_MODEL = (Resolve-Path -LiteralPath $Model).Path
    $env:PHORMINX_SPOKEN_ROLLOVER_WAV = $fixturePath
    $env:PHORMINX_ACCURATE_REFERENCE = $speechText
    Push-Location (Split-Path $PSScriptRoot -Parent)
    try {
        $testArguments = @('test', '-p', 'phorminx-app', '--bin', 'phorminx-app')
        if ($Release) { $testArguments += '--release' }
        if ($Vulkan) { $testArguments += @('--features', 'vulkan') }
        $testArguments += @('native_accurate_speech', '--', '--ignored', '--nocapture', '--test-threads=1')
        & cargo @testArguments
        if ($LASTEXITCODE -ne 0) { throw 'Native Accurate streaming regression failed.' }
    } finally { Pop-Location }
} finally {
    if ($previousExecutionState -ne 0) { [void][PhorminxNativeStreamingTestPower]::SetThreadExecutionState($previousExecutionState) }
    if ($null -ne $synth) { $synth.Dispose() }
    $env:PHORMINX_WHISPER_MODEL = $previousModel
    $env:PHORMINX_SPOKEN_ROLLOVER_WAV = $previousFixture
    $env:PHORMINX_ACCURATE_REFERENCE = $previousReference
    if (Test-Path -LiteralPath $fixturePath) { Remove-Item -LiteralPath $fixturePath }
}
