# Phase 0: microphone and Whisper feasibility

## Prerequisites

- Rust 1.98 (`rust-toolchain.toml` pins it).
- Visual Studio 2022 Build Tools with the C++ workload and Windows SDK.
- CMake available on `PATH`.
- A local `whisper.cpp` GGML model under `models/`.
- LLVM/libclang for generating Windows bindings.
- For the optional Vulkan backend: Vulkan SDK and Ninja.

## Commands

```powershell
. .\scripts\Enter-PhorminxDevShell.ps1

cargo run --release -p phorminx-bench -- devices

cargo run --release -p phorminx-bench -- record `
  --seconds 8 `
  --output test-data/latest.wav

cargo run --release -p phorminx-bench -- transcribe `
  --model models/ggml-base.en.bin `
  --input test-data/latest.wav

cargo run --release -p phorminx-bench -- capture-transcribe `
  --model models/ggml-base.en.bin `
  --seconds 8 `
  --save-wav test-data/latest.wav

# AMD/NVIDIA/Intel GPU path after installing the Vulkan SDK:
. .\scripts\Enter-PhorminxDevShell.ps1 -Vulkan
cargo run --release -p phorminx-bench --features vulkan -- transcribe `
  --model models/ggml-small.en-q5_1.bin `
  --input test-data/latest.wav `
  --audio-context 512
```

The benchmark prints model-load time, inference time, real-time factor, peak amplitude, and RMS level. Personal recordings under `test-data/` and model weights under `models/` are ignored by Git.

## Initial comparison

1. `ggml-base.en.bin` establishes an unquantized baseline.
2. `ggml-small.en-q5_1.bin` is the expected memory-conscious quality candidate.
3. Compare transcript correctness for fillers, false starts, names, numbers, and technical terms.
4. Measure both cold model load and warm repeated inference before choosing a product default.

`--audio-context` is an experimental Whisper encoder-context override. The default model context is 1500 units for a 30-second window. A bounded context close to 50 units per second of audio can dramatically reduce short-dictation latency, but it must pass the accuracy corpus before becoming automatic product behavior.

## Exit criteria

- The default microphone appears in `devices`.
- A recording has non-zero RMS and no clipping.
- Both candidate models produce usable English transcripts.
- Real-time factor and working-set memory are recorded on the reference machine.
- No audio file is committed to Git.
