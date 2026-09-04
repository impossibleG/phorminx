# Production performance runtime validation

The production adapter for ADR-016 uses the real installed recognizers. It is
not a synthetic hardware score:

- candidate discovery accepts pinned Whisper files only after a full SHA-256
  match, and accepts managed Vosk only after receipt, slot, layout, and current
  runtime/model tree validation;
- Accurate candidates are explicit CPU or Vulkan loads; an explicit Vulkan
  load cannot silently fall back to CPU;
- Instant candidates are emitted only for languages covered by the installed
  pinned model. The current pinned Vosk catalog is English-only, so PT-BR
  Instant is reported as unavailable rather than benchmarked or recommended;
- the evidence context binds the benchmark protocol, running executable,
  processor/GPU identity, Windows kernel, Vulkan ICD manifest, and referenced
  Vulkan driver binary. Model, backend, and language are bound by the candidate;
- each run consumes three speech clips and one silence clip from a one-use,
  bounded, in-memory calibration object. Audio and recognized text are scored
  in memory, then dropped; only aggregate content-free evidence can persist;
- current process working set is sampled against a pre-load baseline. The
  Windows process-lifetime peak is deliberately not presented as a candidate
  peak;
- preflight atomically reserves the compute lane and refuses active dictation,
  Whisper, Ollama, low-memory, elevated-throttling, or high-CPU states. Resource
  sampling continues during recognition so a run that becomes contended is
  rejected by evidence aggregation;
- cancellation and deadlines abort Whisper work where supported. A native call
  that cannot return cannot cause replacement-worker growth: one process-global
  native worker permit remains held until that call exits.

## Automated gates

Run on the normal CPU build:

```powershell
cargo fmt --all -- --check
cargo clippy --workspace --all-targets -- -D warnings
cargo test --workspace
```

The automated suite covers bilingual protocol shape, bounded/one-use capture,
cold plus warm resident execution, exact candidate matching, benchmark versus
dictation races, Whisper/Ollama contention, thermal/memory/load failure,
content-free diagnostics, installed-tree and Vulkan-driver identity changes,
and the stalled-native-worker fuse.

## Physical-only gates

These remain release evidence, not claims made by unit tests:

1. Record all three displayed speech prompts and the silence prompt with a real
   microphone in English and PT-BR. Confirm no audio, prompt text, or transcript
   appears in logs, diagnostics, the history database, temporary files, or the
   persisted benchmark evidence.
2. Benchmark each installed Tiny/Base English and multilingual model on CPU.
   Confirm cold load occurs once, the following three cases use the same resident
   model, and evidence invalidates after replacing the executable or model.
3. Build the `vulkan` feature with the pinned Windows Vulkan SDK, then benchmark
   on the target Vulkan-capable GPU. Confirm the active backend is Vulkan, no fallback is
   counted, and evidence invalidates after a GPU driver update.
4. Start dictation, Whisper work, and Ollama generation/warm-up independently
   while attempting a benchmark. Each attempt must fail before calibration is
   consumed. Also begin dictation during a benchmark and confirm dictation is
   refused without disturbing the benchmark.
5. Exercise sustained CPU/GPU load, low available memory, Windows efficiency
   throttling, sleep/resume, and device/driver changes. Untrustworthy or changed
   conditions must yield no recommendation evidence.
6. Inject a non-returning native recognition call in a dedicated disposable
   process. Confirm cancellation returns control to the UI, repeated attempts do
   not increase worker/thread count, and process shutdown remains bounded.
7. Run a long repeated benchmark soak and inspect process handles, threads, and
   working set. Validate that the reported memory delta tracks each candidate
   rather than monotonically inheriting a previous candidate's process peak.

The current development host can validate the default CPU/Vosk build. Vulkan
compilation is blocked until the Vulkan SDK is installed and `VULKAN_SDK` is
set; this is a physical/toolchain gate and must not be recorded as a successful
Vulkan qualification.
