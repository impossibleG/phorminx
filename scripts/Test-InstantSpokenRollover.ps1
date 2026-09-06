[CmdletBinding()]
param(
    [string] $AssetsRoot = (Join-Path $env:LOCALAPPDATA 'Phorminx'),
    [string] $Voice = 'Microsoft David Desktop'
)

$ErrorActionPreference = 'Stop'
. (Join-Path $PSScriptRoot 'Enter-PhorminxDevShell.ps1')
Add-Type -AssemblyName System.Speech
$fixture = Join-Path ([IO.Path]::GetTempPath()) ('phorminx-neutral-rollover-' + [Guid]::NewGuid().ToString('N') + '.wav')
$synth = New-Object System.Speech.Synthesis.SpeechSynthesizer
$previousRuntime = $env:PHORMINX_VOSK_RUNTIME
$previousModel = $env:PHORMINX_VOSK_MODEL
$previousFixture = $env:PHORMINX_SPOKEN_ROLLOVER_WAV
try {
    $synth.SelectVoice($Voice)
    $synth.Rate = 0
    $format = [System.Speech.AudioFormat.SpeechAudioFormatInfo]::new(
        16000,
        [System.Speech.AudioFormat.AudioBitsPerSample]::Sixteen,
        [System.Speech.AudioFormat.AudioChannel]::Mono
    )
    $synth.SetOutputToWaveFile($fixture, $format)
    # Deliberately continuous, neutral text: long enough for two production
    # recognition runs without relying on any private recordings. The tests
    # cover natural endpoints and explicitly triggered restart/replay boundaries.
    $synth.Speak('the morning train arrived at the station and the passengers walked across the platform carrying their bags while the conductor checked the schedule and the engineer prepared the engine for another journey through the valley beyond the river where the old stone bridge stood beside a quiet village and a small school with a garden full of yellow flowers and tall green trees that provided shade for the children playing outside before their lessons began and their teacher opened the classroom windows to let the cool morning air enter the room while she arranged the books on the wooden desk and wrote a list of questions on the board about the mountains and the forests and the animals that lived nearby where the farmers grew apples and potatoes and sold fresh vegetables at the market every Saturday morning when people came from neighboring towns to meet their friends and share stories about the weather and their families and their plans for the coming summer holiday near the ocean where the water was clear and the beaches were covered with soft sand and colorful shells that the children collected carefully before returning home for dinner with their grandparents who had prepared warm bread and vegetable soup for everyone to enjoy around the kitchen table as the evening light faded and the stars began to appear in the dark blue sky above the village where another peaceful day was coming to an end')
    $synth.Dispose()
    $synth = $null
    $env:PHORMINX_VOSK_RUNTIME = Join-Path $AssetsRoot 'runtime\vosk'
    $env:PHORMINX_VOSK_MODEL = Join-Path $AssetsRoot 'models\vosk-model-small-en-us-0.15'
    $env:PHORMINX_SPOKEN_ROLLOVER_WAV = $fixture
    Push-Location (Split-Path $PSScriptRoot -Parent)
    try {
        cargo test -p phorminx-app --bin phorminx-app native_spoken_audio -- --ignored --nocapture --test-threads=1
        if ($LASTEXITCODE -ne 0) { throw 'Native spoken rollover regression failed.' }
    } finally {
        Pop-Location
    }
} finally {
    if ($null -ne $synth) { $synth.Dispose() }
    $env:PHORMINX_VOSK_RUNTIME = $previousRuntime
    $env:PHORMINX_VOSK_MODEL = $previousModel
    $env:PHORMINX_SPOKEN_ROLLOVER_WAV = $previousFixture
    if (Test-Path -LiteralPath $fixture) { Remove-Item -LiteralPath $fixture }
}
