use std::path::PathBuf;
use std::sync::Arc;
use std::sync::atomic::{AtomicBool, AtomicU8, AtomicU64, Ordering};
use std::sync::mpsc::{self, Receiver, SyncSender, TrySendError};
use std::thread::{self, JoinHandle};
use std::time::Duration;

use cpal::traits::{DeviceTrait, HostTrait, StreamTrait};
use cpal::{FromSample, Sample, SampleFormat, StreamConfig};
use phorminx_core::AudioClip;
use phorminx_session::{
    AudioSpan, EncryptedAudioSpool, FileStorage, SampleRange, SpoolError, SpoolQuota, SpoolRoot,
    SpoolStorage, scavenge_orphans,
};
use ringbuf::traits::{Consumer, Observer, Producer, Split};
use ringbuf::{HeapCons, HeapProd, HeapRb};
use rubato::audioadapter_buffers::direct::InterleavedSlice;
use rubato::{Fft, FixedSync, Indexing, Resampler};
use zeroize::{Zeroize, Zeroizing};

use crate::{CaptureError, StreamingAudio, WHISPER_SAMPLE_RATE};

const DEFAULT_SHORT_LIMIT: Duration = Duration::from_secs(120);
const DEFAULT_TRANSITION_LEAD: Duration = Duration::from_secs(5);
const DEFAULT_CALLBACK_BUFFER: Duration = Duration::from_secs(2);
const DEFAULT_SPOOL_RECORD: Duration = Duration::from_secs(1);
const DEFAULT_PUMP_BATCH_FRAMES: usize = 8_192;
const DEFAULT_POLL_INTERVAL: Duration = Duration::from_millis(2);
const SNAPSHOT_TIMEOUT: Duration = Duration::from_secs(5);
const FINALIZATION_TIMEOUT: Duration = Duration::from_secs(5);
const RESAMPLER_CHUNK_FRAMES: usize = 1_024;
const MAX_PUMP_BATCH_FRAMES: usize = 262_144;

/// Extended capture policy. Durations are converted to exact canonical 16 kHz sample bounds.
#[derive(Clone, Debug)]
pub struct ExtendedCaptureConfig {
    pub spool_directory: PathBuf,
    pub spool_quota: SpoolQuota,
    pub short_memory_limit: Duration,
    pub transition_lead: Duration,
    pub callback_buffer_duration: Duration,
    pub spool_record_duration: Duration,
    pub pump_batch_frames: usize,
    pub pump_poll_interval: Duration,
}

/// Startup-validated capture resources reused by every hotkey activation.
#[derive(Clone)]
pub struct ExtendedCaptureFactory {
    config: ExtendedCaptureConfig,
    bounds: CaptureBounds,
    spool_root: SpoolRoot,
    finalizer_active: Arc<AtomicBool>,
}

impl ExtendedCaptureFactory {
    pub fn new(config: ExtendedCaptureConfig) -> Result<Self, CaptureError> {
        let bounds = config.bounds().map_err(CaptureError::Extended)?;
        let spool_root = SpoolRoot::open_or_create(&config.spool_directory)
            .map_err(map_spool_error)
            .map_err(CaptureError::Extended)?;
        scavenge_orphans(&spool_root)
            .map_err(map_spool_error)
            .map_err(CaptureError::Extended)?;
        Ok(Self {
            config,
            bounds,
            spool_root,
            finalizer_active: Arc::new(AtomicBool::new(false)),
        })
    }

    pub fn start_input(
        &self,
        device_name: Option<&str>,
    ) -> Result<ExtendedRecording, CaptureError> {
        start_extended_prepared(
            device_name,
            self.config.clone(),
            self.bounds,
            self.spool_root.clone(),
            FinalizerPermit::acquire(&self.finalizer_active)?,
        )
    }
}

struct FinalizerPermit {
    active: Arc<AtomicBool>,
    release_on_drop: bool,
}

impl FinalizerPermit {
    fn acquire(active: &Arc<AtomicBool>) -> Result<Self, CaptureError> {
        active
            .compare_exchange(false, true, Ordering::AcqRel, Ordering::Acquire)
            .map_err(|_| CaptureError::Extended(ExtendedCaptureFault::FinalizerBusy))?;
        Ok(Self {
            active: Arc::clone(active),
            release_on_drop: true,
        })
    }

    fn block(mut self) {
        self.release_on_drop = false;
    }
}

impl Drop for FinalizerPermit {
    fn drop(&mut self) {
        if self.release_on_drop {
            self.active.store(false, Ordering::Release);
        }
    }
}

impl ExtendedCaptureConfig {
    pub fn new(spool_directory: impl Into<PathBuf>) -> Self {
        Self {
            spool_directory: spool_directory.into(),
            spool_quota: SpoolQuota::default(),
            short_memory_limit: DEFAULT_SHORT_LIMIT,
            transition_lead: DEFAULT_TRANSITION_LEAD,
            callback_buffer_duration: DEFAULT_CALLBACK_BUFFER,
            spool_record_duration: DEFAULT_SPOOL_RECORD,
            pump_batch_frames: DEFAULT_PUMP_BATCH_FRAMES,
            pump_poll_interval: DEFAULT_POLL_INTERVAL,
        }
    }

    fn bounds(&self) -> Result<CaptureBounds, ExtendedCaptureFault> {
        let short_limit = canonical_samples(self.short_memory_limit);
        let lead = canonical_samples(self.transition_lead);
        let callback_seconds = self.callback_buffer_duration.as_secs_f64();
        let record_samples = canonical_samples(self.spool_record_duration);
        if short_limit == 0
            || lead == 0
            || lead >= short_limit
            || !callback_seconds.is_finite()
            || callback_seconds <= 0.0
            || record_samples == 0
            || record_samples > u64::from(self.spool_quota.max_record_samples)
            || self.spool_quota.max_file_bytes == 0
            || self.spool_quota.max_samples == 0
            || self.spool_quota.max_records == 0
            || self.spool_quota.max_record_samples == 0
            || self.spool_quota.max_read_samples == 0
            || self.spool_quota.max_record_samples > self.spool_quota.max_read_samples
            || self.pump_batch_frames == 0
            || self.pump_batch_frames > MAX_PUMP_BATCH_FRAMES
            || self.pump_poll_interval.is_zero()
        {
            return Err(ExtendedCaptureFault::InvalidConfiguration);
        }
        Ok(CaptureBounds {
            prepare_at: short_limit - lead,
            transition_at: short_limit - lead,
            short_limit,
            record_samples: u32::try_from(record_samples)
                .map_err(|_| ExtendedCaptureFault::InvalidConfiguration)?,
        })
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
struct CaptureBounds {
    prepare_at: u64,
    transition_at: u64,
    short_limit: u64,
    record_samples: u32,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum ExtendedStorageKind {
    Memory,
    PreparedSpool,
    ExtendedSpool,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct ExtendedCaptureProgress {
    pub native_frames_observed: u64,
    pub canonical_samples: u64,
    pub resident_samples: u64,
    pub peak_resident_samples: u64,
    pub dropped_native_frames: u64,
    pub backend_warning_count: u64,
    pub storage: ExtendedStorageKind,
    pub sticky_fault: Option<ExtendedCaptureFault>,
}

impl ExtendedCaptureProgress {
    pub fn duration(self) -> Duration {
        Duration::from_secs_f64(self.canonical_samples as f64 / f64::from(WHISPER_SAMPLE_RATE))
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, thiserror::Error)]
pub enum ExtendedCaptureFault {
    #[error("extended capture configuration is invalid")]
    InvalidConfiguration,
    #[error("the real-time capture queue overflowed")]
    CallbackOverflow,
    #[error("canonical audio resampling failed")]
    Resampling,
    #[error("encrypted audio spool quota was exhausted")]
    SpoolQuota,
    #[error("encrypted audio spool integrity validation failed")]
    SpoolIntegrity,
    #[error("encrypted audio spool I/O failed")]
    SpoolIo,
    #[error("extended capture worker is unavailable")]
    WorkerUnavailable,
    #[error("extended capture worker panicked")]
    WorkerPanicked,
    #[error("absolute sample accounting overflowed")]
    SampleAccountingOverflow,
    #[error("the requested audio snapshot is invalid or exceeds its configured bound")]
    InvalidSnapshot,
    #[error("the microphone backend failed during capture")]
    StreamFailed,
    #[error("a prior audio finalizer is still stalled")]
    FinalizerBusy,
    #[error("audio finalization exceeded its bounded deadline")]
    FinalizationTimeout,
}

impl ExtendedCaptureFault {
    const fn code(self) -> u8 {
        match self {
            Self::InvalidConfiguration => 1,
            Self::CallbackOverflow => 2,
            Self::Resampling => 3,
            Self::SpoolQuota => 4,
            Self::SpoolIntegrity => 5,
            Self::SpoolIo => 6,
            Self::WorkerUnavailable => 7,
            Self::WorkerPanicked => 8,
            Self::SampleAccountingOverflow => 9,
            Self::InvalidSnapshot => 10,
            Self::StreamFailed => 11,
            Self::FinalizerBusy => 12,
            Self::FinalizationTimeout => 13,
        }
    }

    const fn from_code(code: u8) -> Option<Self> {
        match code {
            1 => Some(Self::InvalidConfiguration),
            2 => Some(Self::CallbackOverflow),
            3 => Some(Self::Resampling),
            4 => Some(Self::SpoolQuota),
            5 => Some(Self::SpoolIntegrity),
            6 => Some(Self::SpoolIo),
            7 => Some(Self::WorkerUnavailable),
            8 => Some(Self::WorkerPanicked),
            9 => Some(Self::SampleAccountingOverflow),
            10 => Some(Self::InvalidSnapshot),
            11 => Some(Self::StreamFailed),
            12 => Some(Self::FinalizerBusy),
            13 => Some(Self::FinalizationTimeout),
            _ => None,
        }
    }
}

struct SharedProgress {
    native_frames: AtomicU64,
    canonical_samples: AtomicU64,
    resident_samples: AtomicU64,
    peak_resident_samples: AtomicU64,
    dropped_frames: AtomicU64,
    backend_warnings: AtomicU64,
    storage: AtomicU8,
    fault: AtomicU8,
}

impl SharedProgress {
    fn new() -> Self {
        Self {
            native_frames: AtomicU64::new(0),
            canonical_samples: AtomicU64::new(0),
            resident_samples: AtomicU64::new(0),
            peak_resident_samples: AtomicU64::new(0),
            dropped_frames: AtomicU64::new(0),
            backend_warnings: AtomicU64::new(0),
            storage: AtomicU8::new(0),
            fault: AtomicU8::new(0),
        }
    }

    fn snapshot(&self) -> ExtendedCaptureProgress {
        let storage = match self.storage.load(Ordering::Acquire) {
            1 => ExtendedStorageKind::PreparedSpool,
            2 => ExtendedStorageKind::ExtendedSpool,
            _ => ExtendedStorageKind::Memory,
        };
        ExtendedCaptureProgress {
            native_frames_observed: self.native_frames.load(Ordering::Acquire),
            canonical_samples: self.canonical_samples.load(Ordering::Acquire),
            resident_samples: self.resident_samples.load(Ordering::Acquire),
            peak_resident_samples: self.peak_resident_samples.load(Ordering::Acquire),
            dropped_native_frames: self.dropped_frames.load(Ordering::Acquire),
            backend_warning_count: self.backend_warnings.load(Ordering::Acquire),
            storage,
            sticky_fault: ExtendedCaptureFault::from_code(self.fault.load(Ordering::Acquire)),
        }
    }

    fn set_fault(&self, fault: ExtendedCaptureFault) {
        let _ = self
            .fault
            .compare_exchange(0, fault.code(), Ordering::AcqRel, Ordering::Acquire);
    }
}

enum PumpCommand {
    Snapshot {
        range: SampleRange,
        reply: SyncSender<Result<AudioSpan, ExtendedCaptureFault>>,
        cancelled: Arc<AtomicBool>,
    },
}

struct PumpControl {
    commands: Receiver<PumpCommand>,
    stop: Arc<AtomicBool>,
    progress: Arc<SharedProgress>,
}

/// A microphone recording whose real-time callback only downmixes and enqueues samples.
pub struct ExtendedRecording {
    stream: Option<cpal::Stream>,
    stop: Arc<AtomicBool>,
    commands: SyncSender<PumpCommand>,
    worker: Option<JoinHandle<()>>,
    finalized: Option<Receiver<Result<FinalizedCapture, ExtendedCaptureFault>>>,
    progress: Arc<SharedProgress>,
    streaming_cursor: u64,
    auto_stopped: bool,
}

impl ExtendedRecording {
    pub const fn sample_rate(&self) -> u32 {
        WHISPER_SAMPLE_RATE
    }

    pub fn captured_duration(&self) -> Duration {
        self.progress().duration()
    }

    pub fn progress(&self) -> ExtendedCaptureProgress {
        self.progress.snapshot()
    }

    pub fn mark_auto_stopped(&mut self) {
        self.auto_stopped = true;
    }

    pub const fn was_auto_stopped(&self) -> bool {
        self.auto_stopped
    }

    /// Copies the most recent bounded canonical window for silence detection.
    pub fn recent_rms(&self, window: Duration) -> Result<f32, CaptureError> {
        let end = self.progress().canonical_samples;
        if end == 0 {
            return Ok(0.0);
        }
        let requested = canonical_samples(window).max(1);
        let start = end.saturating_sub(requested);
        let span = self.snapshot(
            SampleRange::new(start, end)
                .map_err(|_| CaptureError::Extended(ExtendedCaptureFault::InvalidSnapshot))?,
        )?;
        let mean_square = span
            .samples()
            .iter()
            .map(|sample| f64::from(*sample) * f64::from(*sample))
            .sum::<f64>()
            / span.samples().len() as f64;
        Ok(mean_square.sqrt() as f32)
    }

    /// Drains a contiguous bounded canonical span for streaming recognizers.
    pub fn drain_streaming(
        &mut self,
        maximum_samples: usize,
    ) -> Result<StreamingAudio, CaptureError> {
        let observed = self.progress();
        if let Some(fault) = observed.sticky_fault {
            return Err(CaptureError::Extended(fault));
        }
        let maximum = u64::try_from(maximum_samples)
            .map_err(|_| CaptureError::Extended(ExtendedCaptureFault::InvalidSnapshot))?;
        let end = observed
            .canonical_samples
            .min(self.streaming_cursor.saturating_add(maximum));
        if end <= self.streaming_cursor {
            return Ok(StreamingAudio {
                samples: Vec::new(),
                sample_rate: WHISPER_SAMPLE_RATE,
                dropped_samples: observed.dropped_native_frames,
            });
        }
        let range = SampleRange::new(self.streaming_cursor, end)
            .map_err(|_| CaptureError::Extended(ExtendedCaptureFault::InvalidSnapshot))?;
        let mut guarded = self.snapshot(range)?.into_samples();
        let samples = std::mem::take(&mut *guarded);
        self.streaming_cursor = end;
        Ok(StreamingAudio {
            samples,
            sample_rate: WHISPER_SAMPLE_RATE,
            dropped_samples: observed.dropped_native_frames,
        })
    }

    pub fn snapshot(&self, range: SampleRange) -> Result<AudioSpan, CaptureError> {
        let (reply, response) = mpsc::sync_channel(1);
        let cancelled = Arc::new(AtomicBool::new(false));
        match self.commands.try_send(PumpCommand::Snapshot {
            range,
            reply,
            cancelled: Arc::clone(&cancelled),
        }) {
            Ok(()) => {}
            Err(TrySendError::Full(_)) => {
                return Err(CaptureError::Extended(
                    ExtendedCaptureFault::WorkerUnavailable,
                ));
            }
            Err(TrySendError::Disconnected(_)) => {
                return Err(CaptureError::Extended(
                    ExtendedCaptureFault::WorkerUnavailable,
                ));
            }
        }
        match response.recv_timeout(SNAPSHOT_TIMEOUT) {
            Ok(result) => result.map_err(CaptureError::Extended),
            Err(_) => {
                // A request that outlives its caller must not later force a decrypt or
                // mutate spool record boundaries. The pump observes this tombstone.
                cancelled.store(true, Ordering::Release);
                Err(CaptureError::Extended(
                    ExtendedCaptureFault::WorkerUnavailable,
                ))
            }
        }
    }

    pub fn finalize(mut self) -> Result<ExtendedCapturedAudio, CaptureError> {
        self.stop_and_resolve().map(ExtendedCapturedAudio::from)
    }

    /// Stops the device immediately and waits for bounded spool finalization.
    pub fn defer_finalize(mut self) -> Result<DeferredCapturedAudio, CaptureError> {
        self.stream.take();
        self.stop.store(true, Ordering::Release);
        let worker = self.worker.take().ok_or(CaptureError::Extended(
            ExtendedCaptureFault::WorkerUnavailable,
        ))?;
        Ok(DeferredCapturedAudio {
            worker: Some(worker),
            finalized: self.finalized.take(),
        })
    }

    fn stop_and_resolve(&mut self) -> Result<FinalizedCapture, CaptureError> {
        self.stream.take();
        self.stop.store(true, Ordering::Release);
        let _worker = self.worker.take().ok_or(CaptureError::Extended(
            ExtendedCaptureFault::WorkerUnavailable,
        ))?;
        receive_finalized(&mut self.finalized, FINALIZATION_TIMEOUT)
    }
}

/// A stopped recording whose disk flush/join has deliberately been moved off
/// the latency-sensitive app thread.
pub struct DeferredCapturedAudio {
    worker: Option<JoinHandle<()>>,
    finalized: Option<Receiver<Result<FinalizedCapture, ExtendedCaptureFault>>>,
}

impl DeferredCapturedAudio {
    pub fn resolve(mut self) -> Result<ExtendedCapturedAudio, CaptureError> {
        self.resolve_with_timeout(FINALIZATION_TIMEOUT)
    }

    fn resolve_with_timeout(
        &mut self,
        timeout: Duration,
    ) -> Result<ExtendedCapturedAudio, CaptureError> {
        let _worker = self.worker.take().ok_or(CaptureError::Extended(
            ExtendedCaptureFault::WorkerUnavailable,
        ))?;
        receive_finalized(&mut self.finalized, timeout).map(ExtendedCapturedAudio::from)
    }
}

fn receive_finalized(
    finalized: &mut Option<Receiver<Result<FinalizedCapture, ExtendedCaptureFault>>>,
    timeout: Duration,
) -> Result<FinalizedCapture, CaptureError> {
    let receiver = finalized.take().ok_or(CaptureError::Extended(
        ExtendedCaptureFault::WorkerUnavailable,
    ))?;
    match receiver.recv_timeout(timeout) {
        Ok(result) => result.map_err(CaptureError::Extended),
        Err(mpsc::RecvTimeoutError::Timeout) => Err(CaptureError::Extended(
            ExtendedCaptureFault::FinalizationTimeout,
        )),
        Err(mpsc::RecvTimeoutError::Disconnected) => {
            Err(CaptureError::Extended(ExtendedCaptureFault::WorkerPanicked))
        }
    }
}

impl Drop for ExtendedRecording {
    fn drop(&mut self) {
        if self.worker.is_some() {
            self.stream.take();
            self.stop.store(true, Ordering::Release);
            // Detach rather than synchronously joining. The pump still owns
            // and cleans its encrypted spool, while cancellation and shutdown
            // remain bounded even if storage is stalled.
            self.worker.take();
        }
    }
}

pub struct ExtendedCapturedAudio {
    inner: Option<EngineFinalized<FileStorage>>,
    finalizer_permit: Option<FinalizerPermit>,
}

struct FinalizedCapture {
    inner: Option<EngineFinalized<FileStorage>>,
    finalizer_permit: Option<FinalizerPermit>,
}

impl From<FinalizedCapture> for ExtendedCapturedAudio {
    fn from(mut finalized: FinalizedCapture) -> Self {
        Self {
            inner: finalized.inner.take(),
            finalizer_permit: finalized.finalizer_permit.take(),
        }
    }
}

impl Drop for FinalizedCapture {
    fn drop(&mut self) {
        if let (Some(inner), Some(permit)) = (self.inner.take(), self.finalizer_permit.take()) {
            spawn_abandoned_cleanup(inner, permit);
        }
    }
}

impl ExtendedCapturedAudio {
    pub const fn total_samples(&self) -> u64 {
        self.inner
            .as_ref()
            .expect("captured audio owns finalized storage")
            .total_samples
    }

    pub const fn backend_warning_count(&self) -> u64 {
        self.inner
            .as_ref()
            .expect("captured audio owns finalized storage")
            .backend_warning_count
    }

    pub const fn is_extended(&self) -> bool {
        matches!(
            self.inner
                .as_ref()
                .expect("captured audio owns finalized storage")
                .storage,
            FinalizedStorage::Spool(_)
        )
    }

    pub fn snapshot(&mut self, range: SampleRange) -> Result<AudioSpan, CaptureError> {
        self.inner
            .as_mut()
            .expect("captured audio owns finalized storage")
            .snapshot(range)
            .map_err(CaptureError::Extended)
    }

    /// Converts only a behavior-compatible short capture into the existing `AudioClip` type.
    pub fn into_short_clip(mut self) -> Result<AudioClip, CaptureError> {
        let inner = self
            .inner
            .take()
            .expect("captured audio owns finalized storage");
        let finalizer_permit = self
            .finalizer_permit
            .take()
            .expect("captured audio owns a finalizer permit");
        match inner.storage {
            FinalizedStorage::Memory(mut samples) => {
                let samples = std::mem::take(&mut *samples);
                drop(finalizer_permit);
                AudioClip::new(samples, WHISPER_SAMPLE_RATE).map_err(CaptureError::Audio)
            }
            FinalizedStorage::Spool(_) => {
                finalizer_permit.block();
                Err(CaptureError::ExtendedCaptureRequiresSnapshots)
            }
        }
    }

    pub fn cleanup(self) -> Result<(), CaptureError> {
        self.cleanup_with_timeout(FINALIZATION_TIMEOUT)
    }

    fn cleanup_with_timeout(mut self, timeout: Duration) -> Result<(), CaptureError> {
        let inner = self
            .inner
            .take()
            .expect("captured audio owns finalized storage");
        let finalizer_permit = self
            .finalizer_permit
            .take()
            .expect("captured audio owns a finalizer permit");
        run_cleanup_with_timeout(move || cleanup_finalized(inner), finalizer_permit, timeout)
    }
}

impl Drop for ExtendedCapturedAudio {
    fn drop(&mut self) {
        if let (Some(inner), Some(permit)) = (self.inner.take(), self.finalizer_permit.take()) {
            spawn_abandoned_cleanup(inner, permit);
        }
    }
}

fn cleanup_finalized(inner: EngineFinalized<FileStorage>) -> Result<(), CaptureError> {
    match inner.storage {
        FinalizedStorage::Memory(_) => Ok(()),
        FinalizedStorage::Spool(spool) => spool
            .cleanup()
            .map_err(map_spool_error)
            .map_err(CaptureError::Extended),
    }
}

fn spawn_abandoned_cleanup(inner: EngineFinalized<FileStorage>, finalizer_permit: FinalizerPermit) {
    let _ = thread::Builder::new()
        .name("phorminx-audio-abandoned-cleanup".to_owned())
        .spawn(move || {
            if cleanup_finalized(inner).is_ok() {
                drop(finalizer_permit);
            } else {
                finalizer_permit.block();
            }
        });
}

fn run_cleanup_with_timeout(
    cleanup: impl FnOnce() -> Result<(), CaptureError> + Send + 'static,
    finalizer_permit: FinalizerPermit,
    timeout: Duration,
) -> Result<(), CaptureError> {
    let (result_tx, result_rx) = mpsc::sync_channel(1);
    thread::Builder::new()
        .name("phorminx-audio-cleanup".to_owned())
        .spawn(move || {
            let result = cleanup();
            if result.is_ok() {
                drop(finalizer_permit);
            } else {
                finalizer_permit.block();
            }
            let _ = result_tx.try_send(result);
        })
        .map_err(|_| CaptureError::Extended(ExtendedCaptureFault::WorkerUnavailable))?;
    match result_rx.recv_timeout(timeout) {
        Ok(result) => result,
        Err(mpsc::RecvTimeoutError::Timeout) => Err(CaptureError::Extended(
            ExtendedCaptureFault::FinalizationTimeout,
        )),
        Err(mpsc::RecvTimeoutError::Disconnected) => {
            Err(CaptureError::Extended(ExtendedCaptureFault::WorkerPanicked))
        }
    }
}

pub fn start_extended_default(
    config: ExtendedCaptureConfig,
) -> Result<ExtendedRecording, CaptureError> {
    start_extended_input(None, config)
}

pub fn start_extended_input(
    device_name: Option<&str>,
    config: ExtendedCaptureConfig,
) -> Result<ExtendedRecording, CaptureError> {
    ExtendedCaptureFactory::new(config)?.start_input(device_name)
}

fn start_extended_prepared(
    device_name: Option<&str>,
    config: ExtendedCaptureConfig,
    bounds: CaptureBounds,
    spool_root: SpoolRoot,
    finalizer_permit: FinalizerPermit,
) -> Result<ExtendedRecording, CaptureError> {
    let host = cpal::default_host();
    let device = match device_name {
        Some(name) => host
            .input_devices()
            .map_err(CaptureError::EnumerateDevices)?
            .find(|device| device.to_string() == name)
            .ok_or_else(|| CaptureError::InputDeviceNotFound(name.to_owned()))?,
        None => host
            .default_input_device()
            .ok_or(CaptureError::NoDefaultInputDevice)?,
    };
    let supported = device
        .default_input_config()
        .map_err(CaptureError::DefaultConfig)?;
    let sample_format = supported.sample_format();
    let stream_config: StreamConfig = supported.into();
    let channels = usize::from(stream_config.channels);
    let source_rate = stream_config.sample_rate;
    let callback_capacity =
        (config.callback_buffer_duration.as_secs_f64() * f64::from(source_rate)).ceil() as usize;
    let (producer, consumer) = HeapRb::<f32>::try_new(callback_capacity)
        .map_err(CaptureError::CaptureBufferAllocation)?
        .split();

    let progress = Arc::new(SharedProgress::new());
    let stop = Arc::new(AtomicBool::new(false));
    // The app only needs the newest synchronous snapshot. Bounding this lane prevents
    // stale requests from forming an unbounded FIFO behind microphone ingest.
    let (commands, command_rx) = mpsc::sync_channel(1);
    let stream = build_extended_stream(
        &device,
        &stream_config,
        sample_format,
        channels,
        producer,
        Arc::clone(&progress),
    )?;

    let worker_progress = Arc::clone(&progress);
    let worker_stop = Arc::clone(&stop);
    let (pump_ready_tx, pump_ready_rx) = mpsc::sync_channel(1);
    let (finalized_tx, finalized_rx) = mpsc::sync_channel(1);
    let worker = thread::Builder::new()
        .name("phorminx-audio-pump".to_owned())
        .spawn(move || {
            let result = run_pump(
                consumer,
                source_rate,
                config,
                bounds,
                spool_root,
                PumpControl {
                    commands: command_rx,
                    stop: worker_stop,
                    progress: worker_progress,
                },
                Some(pump_ready_tx),
            );
            match result {
                Ok(inner) => {
                    let _ = finalized_tx.try_send(Ok(FinalizedCapture {
                        inner: Some(inner),
                        finalizer_permit: Some(finalizer_permit),
                    }));
                }
                Err(fault) => {
                    finalizer_permit.block();
                    let _ = finalized_tx.try_send(Err(fault));
                }
            }
        })
        .map_err(CaptureError::PumpSpawn)?;

    match pump_ready_rx.recv_timeout(SNAPSHOT_TIMEOUT) {
        Ok(Ok(())) => {}
        Ok(Err(fault)) => {
            stop.store(true, Ordering::Release);
            let _ = worker.join();
            return Err(CaptureError::Extended(fault));
        }
        Err(_) => {
            stop.store(true, Ordering::Release);
            return Err(CaptureError::Extended(
                ExtendedCaptureFault::WorkerUnavailable,
            ));
        }
    }

    if let Err(error) = stream.play() {
        stop.store(true, Ordering::Release);
        let _ = worker.join();
        return Err(CaptureError::PlayStream(error));
    }

    Ok(ExtendedRecording {
        stream: Some(stream),
        stop,
        commands,
        worker: Some(worker),
        finalized: Some(finalized_rx),
        progress,
        streaming_cursor: 0,
        auto_stopped: false,
    })
}

fn build_extended_stream(
    device: &cpal::Device,
    config: &StreamConfig,
    sample_format: SampleFormat,
    channels: usize,
    producer: HeapProd<f32>,
    progress: Arc<SharedProgress>,
) -> Result<cpal::Stream, CaptureError> {
    macro_rules! build {
        ($sample:ty) => {
            build_typed_extended_stream::<$sample>(device, config, channels, producer, progress)
        };
    }
    match sample_format {
        SampleFormat::I8 => build!(i8),
        SampleFormat::I16 => build!(i16),
        SampleFormat::I24 => build!(cpal::I24),
        SampleFormat::I32 => build!(i32),
        SampleFormat::I64 => build!(i64),
        SampleFormat::U8 => build!(u8),
        SampleFormat::U16 => build!(u16),
        SampleFormat::U24 => build!(cpal::U24),
        SampleFormat::U32 => build!(u32),
        SampleFormat::U64 => build!(u64),
        SampleFormat::F32 => build!(f32),
        SampleFormat::F64 => build!(f64),
        other => Err(CaptureError::UnsupportedSampleFormat(other)),
    }
}

fn build_typed_extended_stream<T>(
    device: &cpal::Device,
    config: &StreamConfig,
    channels: usize,
    mut producer: HeapProd<f32>,
    progress: Arc<SharedProgress>,
) -> Result<cpal::Stream, CaptureError>
where
    T: cpal::SizedSample + Sample,
    f32: FromSample<T>,
{
    let error_progress = Arc::clone(&progress);
    device
        .build_input_stream(
            *config,
            move |data: &[T], _| {
                push_extended_mono_frames(&mut producer, data, channels, &progress);
            },
            move |_error| {
                error_progress
                    .backend_warnings
                    .fetch_add(1, Ordering::Relaxed);
                // CPAL's stream error callback means continuity is no longer
                // trustworthy. A captured prefix must never be returned as success.
                error_progress.set_fault(ExtendedCaptureFault::StreamFailed);
            },
            None,
        )
        .map_err(CaptureError::BuildStream)
}

/// This is the entire CPAL callback path: arithmetic, SPSC pushes, and atomics only.
fn push_extended_mono_frames<T>(
    producer: &mut HeapProd<f32>,
    data: &[T],
    channels: usize,
    progress: &SharedProgress,
) where
    T: Sample,
    f32: FromSample<T>,
{
    if channels == 0 {
        return;
    }
    let frames = data.chunks_exact(channels);
    let frame_count = frames.len();
    let pushed = producer.push_iter(frames.map(|frame| {
        frame
            .iter()
            .map(|sample| sample.to_sample::<f32>())
            .sum::<f32>()
            / channels as f32
    }));
    progress
        .native_frames
        .fetch_add(frame_count as u64, Ordering::Relaxed);
    let dropped = frame_count.saturating_sub(pushed);
    if dropped != 0 {
        progress
            .dropped_frames
            .fetch_add(dropped as u64, Ordering::Relaxed);
        progress.set_fault(ExtendedCaptureFault::CallbackOverflow);
    }
}

fn run_pump(
    mut consumer: HeapCons<f32>,
    source_rate: u32,
    config: ExtendedCaptureConfig,
    bounds: CaptureBounds,
    spool_root: SpoolRoot,
    control: PumpControl,
    ready: Option<SyncSender<Result<(), ExtendedCaptureFault>>>,
) -> Result<EngineFinalized<FileStorage>, ExtendedCaptureFault> {
    let spool_quota = config.spool_quota;
    let PumpControl {
        commands,
        stop,
        progress,
    } = control;
    let mut engine = match CaptureEngine::new(
        source_rate,
        bounds,
        move || EncryptedAudioSpool::create(&spool_root, spool_quota),
        Some(Arc::clone(&progress)),
    ) {
        Ok(engine) => engine,
        Err(fault) => {
            if let Some(ready) = ready {
                let _ = ready.try_send(Err(fault));
            }
            return Err(fault);
        }
    };
    if let Some(ready) = ready {
        let _ = ready.try_send(Ok(()));
    }
    let mut batch = Zeroizing::new(Vec::new());
    batch
        .try_reserve_exact(config.pump_batch_frames)
        .map_err(|_| ExtendedCaptureFault::InvalidConfiguration)?;
    batch.resize(config.pump_batch_frames, 0.0);
    loop {
        let count = consumer.pop_slice(&mut batch);
        propagate_shared_fault(&mut engine, &progress);
        if count != 0
            && engine.fault.is_none()
            && let Err(fault) = engine.ingest_native(&batch[..count])
        {
            progress.set_fault(fault);
        }
        batch[..count].zeroize();

        // Stop/finalize has priority over diagnostic snapshots, and the bounded
        // command lane permits at most one request per ingest turn.
        if stop.load(Ordering::Acquire) && consumer.is_empty() {
            break;
        }
        if let Ok(command) = commands.try_recv() {
            match command {
                PumpCommand::Snapshot {
                    range,
                    reply,
                    cancelled,
                } => {
                    if !cancelled.load(Ordering::Acquire) {
                        let result = engine.snapshot(range);
                        if !cancelled.load(Ordering::Acquire) {
                            let _ = reply.try_send(result);
                        }
                    }
                }
            }
        }
        if count == 0 {
            thread::sleep(config.pump_poll_interval);
        }
    }
    propagate_shared_fault(&mut engine, &progress);
    engine.finalize(progress.backend_warnings.load(Ordering::Acquire))
}

fn propagate_shared_fault<S, C>(engine: &mut CaptureEngine<S, C>, progress: &SharedProgress)
where
    S: SpoolStorage,
    C: FnMut() -> Result<EncryptedAudioSpool<S>, SpoolError>,
{
    if let Some(fault) = ExtendedCaptureFault::from_code(progress.fault.load(Ordering::Acquire)) {
        engine.set_fault(fault);
    }
}

enum FinalizedStorage<S: SpoolStorage> {
    Memory(Zeroizing<Vec<f32>>),
    Spool(EncryptedAudioSpool<S>),
}

struct EngineFinalized<S: SpoolStorage> {
    storage: FinalizedStorage<S>,
    total_samples: u64,
    backend_warning_count: u64,
    fault: Option<ExtendedCaptureFault>,
}

impl<S: SpoolStorage> EngineFinalized<S> {
    fn snapshot(&mut self, range: SampleRange) -> Result<AudioSpan, ExtendedCaptureFault> {
        if let Some(fault) = self.fault {
            return Err(fault);
        }
        if range.is_empty() || range.end() > self.total_samples {
            return Err(ExtendedCaptureFault::InvalidSnapshot);
        }
        let result = match &mut self.storage {
            FinalizedStorage::Memory(samples) => {
                let start = usize::try_from(range.start())
                    .map_err(|_| ExtendedCaptureFault::InvalidSnapshot)?;
                let end = usize::try_from(range.end())
                    .map_err(|_| ExtendedCaptureFault::InvalidSnapshot)?;
                AudioSpan::new(range.start(), samples[start..end].to_vec())
                    .map_err(|_| ExtendedCaptureFault::InvalidSnapshot)
            }
            FinalizedStorage::Spool(spool) => spool.read_range(range).map_err(map_spool_error),
        };
        if let Err(fault) = result
            && is_sticky_spool_fault(fault)
        {
            self.fault = Some(fault);
        }
        result
    }
}

struct CaptureEngine<S, C>
where
    S: SpoolStorage,
    C: FnMut() -> Result<EncryptedAudioSpool<S>, SpoolError>,
{
    canonicalizer: StreamingCanonicalizer,
    bounds: CaptureBounds,
    _create_spool: C,
    spool: Option<EncryptedAudioSpool<S>>,
    spool_pending: Zeroizing<Vec<f32>>,
    spooled_through: u64,
    memory: Zeroizing<Vec<f32>>,
    canonical_samples: u64,
    fault: Option<ExtendedCaptureFault>,
    progress: Option<Arc<SharedProgress>>,
}

impl<S, C> CaptureEngine<S, C>
where
    S: SpoolStorage,
    C: FnMut() -> Result<EncryptedAudioSpool<S>, SpoolError>,
{
    fn new(
        source_rate: u32,
        bounds: CaptureBounds,
        create_spool: C,
        progress: Option<Arc<SharedProgress>>,
    ) -> Result<Self, ExtendedCaptureFault> {
        let memory_capacity = usize::try_from(bounds.short_limit)
            .map_err(|_| ExtendedCaptureFault::InvalidConfiguration)?;
        let mut memory = Zeroizing::new(Vec::new());
        memory
            .try_reserve_exact(memory_capacity)
            .map_err(|_| ExtendedCaptureFault::InvalidConfiguration)?;
        Ok(Self {
            canonicalizer: StreamingCanonicalizer::new(source_rate)?,
            bounds,
            _create_spool: create_spool,
            spool: None,
            spool_pending: Zeroizing::new(Vec::with_capacity(bounds.record_samples as usize)),
            spooled_through: 0,
            memory,
            canonical_samples: 0,
            fault: None,
            progress,
        })
    }

    fn ingest_native(&mut self, native: &[f32]) -> Result<(), ExtendedCaptureFault> {
        self.ensure_healthy()?;
        if native.is_empty() {
            return Ok(());
        }
        if native.iter().any(|sample| !sample.is_finite()) {
            return Err(self.set_fault(ExtendedCaptureFault::Resampling));
        }
        let canonical = self
            .canonicalizer
            .push(native)
            .map_err(|fault| self.set_fault(fault))?;
        self.store_canonical(&canonical)
    }

    fn snapshot(&mut self, range: SampleRange) -> Result<AudioSpan, ExtendedCaptureFault> {
        self.ensure_healthy()?;
        if range.is_empty() || range.end() > self.canonical_samples {
            return Err(ExtendedCaptureFault::InvalidSnapshot);
        }
        if !self.memory.is_empty() {
            let start = usize::try_from(range.start())
                .map_err(|_| ExtendedCaptureFault::InvalidSnapshot)?;
            let end =
                usize::try_from(range.end()).map_err(|_| ExtendedCaptureFault::InvalidSnapshot)?;
            return AudioSpan::new(range.start(), self.memory[start..end].to_vec())
                .map_err(|_| ExtendedCaptureFault::InvalidSnapshot);
        }
        // Live snapshots must not force a partial record to disk: doing so makes
        // record count and quota consumption depend on UI polling frequency.
        let pending_start = self.spooled_through;
        if range.start() >= pending_start {
            let start = usize::try_from(range.start() - pending_start)
                .map_err(|_| ExtendedCaptureFault::InvalidSnapshot)?;
            let end = usize::try_from(range.end() - pending_start)
                .map_err(|_| ExtendedCaptureFault::InvalidSnapshot)?;
            return AudioSpan::new(range.start(), self.spool_pending[start..end].to_vec())
                .map_err(|_| ExtendedCaptureFault::InvalidSnapshot);
        }

        let committed_end = range.end().min(pending_start);
        let committed = SampleRange::new(range.start(), committed_end)
            .map_err(|_| ExtendedCaptureFault::InvalidSnapshot)?;
        let mut samples = {
            let spool = self
                .spool
                .as_mut()
                .ok_or(ExtendedCaptureFault::SampleAccountingOverflow)?;
            spool
                .read_range(committed)
                .map_err(map_spool_error)?
                .into_samples()
        };
        if range.end() > pending_start {
            let pending_end = usize::try_from(range.end() - pending_start)
                .map_err(|_| ExtendedCaptureFault::InvalidSnapshot)?;
            samples.extend_from_slice(&self.spool_pending[..pending_end]);
        }
        AudioSpan::new(range.start(), std::mem::take(&mut *samples))
            .map_err(|_| ExtendedCaptureFault::InvalidSnapshot)
    }

    fn finalize(
        mut self,
        backend_warning_count: u64,
    ) -> Result<EngineFinalized<S>, ExtendedCaptureFault> {
        self.ensure_healthy()?;
        let tail = self
            .canonicalizer
            .finish()
            .map_err(|fault| self.set_fault(fault))?;
        self.store_canonical(&tail)?;
        self.ensure_healthy()?;

        if backend_warning_count != 0 {
            return Err(self.set_fault(ExtendedCaptureFault::StreamFailed));
        }

        if self.canonical_samples <= self.bounds.short_limit {
            if let Some(spool) = self.spool.take() {
                spool.cleanup().map_err(map_spool_error)?;
            }
            return Ok(EngineFinalized {
                storage: FinalizedStorage::Memory(self.memory),
                total_samples: self.canonical_samples,
                backend_warning_count,
                fault: None,
            });
        }
        self.flush_spool_pending(true)?;
        let mut spool = self
            .spool
            .take()
            .ok_or(ExtendedCaptureFault::SampleAccountingOverflow)?;
        spool.flush().map_err(map_spool_error)?;
        Ok(EngineFinalized {
            storage: FinalizedStorage::Spool(spool),
            total_samples: self.canonical_samples,
            backend_warning_count,
            fault: None,
        })
    }

    fn store_canonical(&mut self, samples: &[f32]) -> Result<(), ExtendedCaptureFault> {
        if samples.is_empty() {
            return Ok(());
        }
        let sample_count = u64::try_from(samples.len())
            .map_err(|_| self.set_fault(ExtendedCaptureFault::SampleAccountingOverflow))?;
        let next = self
            .canonical_samples
            .checked_add(sample_count)
            .ok_or_else(|| self.set_fault(ExtendedCaptureFault::SampleAccountingOverflow))?;

        if next <= self.bounds.short_limit {
            self.memory.extend_from_slice(samples);
            if self.spool.is_none() && next >= self.bounds.prepare_at {
                self.spool = Some((self._create_spool)().map_err(|error| {
                    let fault = map_spool_error(error);
                    self.set_fault(fault)
                })?);
            }
            self.backfill_memory(Some(4))?;
        } else if !self.memory.is_empty() {
            if self.spool.is_none() {
                self.spool = Some((self._create_spool)().map_err(|error| {
                    let fault = map_spool_error(error);
                    self.set_fault(fault)
                })?);
            }
            self.backfill_memory(None)?;
            let pending_start = usize::try_from(self.spooled_through)
                .map_err(|_| ExtendedCaptureFault::SampleAccountingOverflow)?;
            self.spool_pending
                .extend_from_slice(&self.memory[pending_start..]);
            self.spool_pending.extend_from_slice(samples);
            self.flush_spool_pending(false)?;
            self.memory.zeroize();
            self.memory.clear();
            self.memory.shrink_to_fit();
        } else {
            self.spool_pending.extend_from_slice(samples);
            self.flush_spool_pending(false)?;
        }
        self.canonical_samples = next;
        self.update_progress();
        Ok(())
    }

    fn backfill_memory(
        &mut self,
        maximum_records: Option<usize>,
    ) -> Result<(), ExtendedCaptureFault> {
        let available = u64::try_from(self.memory.len())
            .map_err(|_| self.set_fault(ExtendedCaptureFault::SampleAccountingOverflow))?;
        if self.spool.is_none() || self.spooled_through >= available {
            return Ok(());
        }
        let record = u64::from(self.bounds.record_samples);
        let complete_end = available / record * record;
        let capped_end = maximum_records.map_or(complete_end, |maximum| {
            complete_end.min(
                self.spooled_through
                    .saturating_add(record.saturating_mul(maximum as u64)),
            )
        });
        if capped_end <= self.spooled_through {
            return Ok(());
        }
        let start = usize::try_from(self.spooled_through)
            .map_err(|_| ExtendedCaptureFault::SampleAccountingOverflow)?;
        let end = usize::try_from(capped_end)
            .map_err(|_| ExtendedCaptureFault::SampleAccountingOverflow)?;
        let result = append_records(
            self.spool
                .as_mut()
                .ok_or(ExtendedCaptureFault::SampleAccountingOverflow)?,
            self.spooled_through,
            &self.memory[start..end],
            self.bounds.record_samples,
        );
        if let Err(error) = result {
            let fault = map_spool_error(error);
            return Err(self.set_fault(fault));
        }
        self.spooled_through = capped_end;
        Ok(())
    }

    fn flush_spool_pending(&mut self, force: bool) -> Result<(), ExtendedCaptureFault> {
        if self.spool_pending.is_empty() {
            return Ok(());
        }
        let record = self.bounds.record_samples as usize;
        let flush_len = if force {
            self.spool_pending.len()
        } else {
            self.spool_pending.len() / record * record
        };
        if flush_len == 0 {
            return Ok(());
        }

        let remainder = Zeroizing::new(self.spool_pending.split_off(flush_len));
        let to_flush = std::mem::replace(&mut self.spool_pending, remainder);
        let result = append_records(
            self.spool
                .as_mut()
                .ok_or(ExtendedCaptureFault::SampleAccountingOverflow)?,
            self.spooled_through,
            &to_flush,
            self.bounds.record_samples,
        );
        if let Err(error) = result {
            let fault = map_spool_error(error);
            return Err(self.set_fault(fault));
        }
        self.spooled_through = self
            .spooled_through
            .checked_add(flush_len as u64)
            .ok_or_else(|| self.set_fault(ExtendedCaptureFault::SampleAccountingOverflow))?;
        Ok(())
    }

    fn ensure_healthy(&self) -> Result<(), ExtendedCaptureFault> {
        self.fault.map_or(Ok(()), Err)
    }

    fn set_fault(&mut self, fault: ExtendedCaptureFault) -> ExtendedCaptureFault {
        let sticky = *self.fault.get_or_insert(fault);
        if let Some(progress) = &self.progress {
            progress.set_fault(sticky);
        }
        sticky
    }

    fn update_progress(&self) {
        if let Some(progress) = &self.progress {
            progress
                .canonical_samples
                .store(self.canonical_samples, Ordering::Release);
            progress.resident_samples.store(
                self.memory.len() as u64
                    + self.spool_pending.len() as u64
                    + self.canonicalizer.pending.len() as u64,
                Ordering::Release,
            );
            progress.peak_resident_samples.fetch_max(
                self.memory.len() as u64
                    + self.spool_pending.len() as u64
                    + self.canonicalizer.pending.len() as u64,
                Ordering::AcqRel,
            );
            let storage = if self.canonical_samples < self.bounds.transition_at {
                0
            } else if self.memory.is_empty() {
                2
            } else {
                1
            };
            progress.storage.store(storage, Ordering::Release);
        }
    }
}

fn append_records<S: SpoolStorage>(
    spool: &mut EncryptedAudioSpool<S>,
    start: u64,
    samples: &[f32],
    record_samples: u32,
) -> Result<(), SpoolError> {
    for (index, chunk) in samples.chunks(record_samples as usize).enumerate() {
        let offset = u64::try_from(index)
            .ok()
            .and_then(|index| index.checked_mul(u64::from(record_samples)))
            .and_then(|offset| start.checked_add(offset))
            .ok_or(SpoolError::QuotaExceeded)?;
        let span = AudioSpan::new(offset, chunk.to_vec())?;
        spool.append(&span)?;
    }
    Ok(())
}

fn map_spool_error(error: SpoolError) -> ExtendedCaptureFault {
    match error {
        SpoolError::QuotaExceeded | SpoolError::RecordTooLarge => ExtendedCaptureFault::SpoolQuota,
        SpoolError::AuthenticationFailed
        | SpoolError::Truncated
        | SpoolError::CorruptHeader
        | SpoolError::CorruptRecord
        | SpoolError::IndexCoverageMismatch
        | SpoolError::NonContiguousAppend { .. } => ExtendedCaptureFault::SpoolIntegrity,
        SpoolError::EmptyRead
        | SpoolError::EmptySpool
        | SpoolError::ReadOutOfBounds { .. }
        | SpoolError::ReadTooLarge => ExtendedCaptureFault::InvalidSnapshot,
        _ => ExtendedCaptureFault::SpoolIo,
    }
}

const fn is_sticky_spool_fault(fault: ExtendedCaptureFault) -> bool {
    matches!(
        fault,
        ExtendedCaptureFault::SpoolQuota
            | ExtendedCaptureFault::SpoolIntegrity
            | ExtendedCaptureFault::SpoolIo
    )
}

struct StreamingCanonicalizer {
    source_rate: u32,
    resampler: Option<Fft<f32>>,
    pending: Zeroizing<Vec<f32>>,
    output_buffer: Zeroizing<Vec<f32>>,
    delay_remaining: usize,
    native_received: u64,
    canonical_emitted: u64,
    finished: bool,
}

impl StreamingCanonicalizer {
    fn new(source_rate: u32) -> Result<Self, ExtendedCaptureFault> {
        if source_rate == 0 {
            return Err(ExtendedCaptureFault::InvalidConfiguration);
        }
        if source_rate == WHISPER_SAMPLE_RATE {
            return Ok(Self {
                source_rate,
                resampler: None,
                pending: Zeroizing::new(Vec::new()),
                output_buffer: Zeroizing::new(Vec::new()),
                delay_remaining: 0,
                native_received: 0,
                canonical_emitted: 0,
                finished: false,
            });
        }
        let resampler = Fft::<f32>::new(
            source_rate as usize,
            WHISPER_SAMPLE_RATE as usize,
            RESAMPLER_CHUNK_FRAMES,
            1,
            FixedSync::Both,
        )
        .map_err(|_| ExtendedCaptureFault::Resampling)?;
        let delay_remaining = resampler.output_delay();
        let output_buffer = Zeroizing::new(vec![0.0; resampler.output_frames_max()]);
        Ok(Self {
            source_rate,
            resampler: Some(resampler),
            pending: Zeroizing::new(Vec::with_capacity(RESAMPLER_CHUNK_FRAMES * 2)),
            output_buffer,
            delay_remaining,
            native_received: 0,
            canonical_emitted: 0,
            finished: false,
        })
    }

    fn push(&mut self, input: &[f32]) -> Result<Zeroizing<Vec<f32>>, ExtendedCaptureFault> {
        if self.finished {
            return Err(ExtendedCaptureFault::Resampling);
        }
        self.native_received = self
            .native_received
            .checked_add(input.len() as u64)
            .ok_or(ExtendedCaptureFault::SampleAccountingOverflow)?;
        if self.resampler.is_none() {
            self.canonical_emitted = self.native_received;
            return Ok(Zeroizing::new(input.to_vec()));
        }

        self.pending.extend_from_slice(input);
        let mut output = Zeroizing::new(Vec::new());
        loop {
            let needed = self
                .resampler
                .as_ref()
                .expect("non-canonical rates own a resampler")
                .input_frames_next();
            if self.pending.len() < needed {
                break;
            }
            let chunk = Zeroizing::new(self.pending[..needed].to_vec());
            self.process_chunk(&chunk, None, u64::MAX, &mut output)?;
            self.pending[..needed].zeroize();
            self.pending.drain(..needed);
        }
        Ok(output)
    }

    fn finish(&mut self) -> Result<Zeroizing<Vec<f32>>, ExtendedCaptureFault> {
        if self.finished {
            return Err(ExtendedCaptureFault::Resampling);
        }
        self.finished = true;
        if self.resampler.is_none() {
            return Ok(Zeroizing::new(Vec::new()));
        }
        let target = resampled_len(self.native_received, self.source_rate)?;
        let mut output = Zeroizing::new(Vec::new());
        if !self.pending.is_empty() {
            let pending = std::mem::replace(&mut self.pending, Zeroizing::new(Vec::new()));
            self.process_chunk(&pending, Some(pending.len()), target, &mut output)?;
        }
        let empty: [f32; 0] = [];
        let mut flushes = 0;
        while self.canonical_emitted < target {
            self.process_chunk(&empty, Some(0), target, &mut output)?;
            flushes += 1;
            if flushes > 8 {
                return Err(ExtendedCaptureFault::Resampling);
            }
        }
        Ok(output)
    }

    fn process_chunk(
        &mut self,
        input: &[f32],
        partial_len: Option<usize>,
        cap: u64,
        output: &mut Zeroizing<Vec<f32>>,
    ) -> Result<(), ExtendedCaptureFault> {
        let input_adapter = InterleavedSlice::new(input, 1, input.len())
            .map_err(|_| ExtendedCaptureFault::Resampling)?;
        let output_len = self.output_buffer.len();
        let mut output_adapter = InterleavedSlice::new_mut(&mut self.output_buffer, 1, output_len)
            .map_err(|_| ExtendedCaptureFault::Resampling)?;
        let indexing = partial_len.map(|frames| Indexing::new().partial_len(frames));
        let (_, produced) = self
            .resampler
            .as_mut()
            .expect("non-canonical rates own a resampler")
            .process_into_buffer(&input_adapter, &mut output_adapter, indexing.as_ref())
            .map_err(|_| ExtendedCaptureFault::Resampling)?;

        let skip = self.delay_remaining.min(produced);
        self.delay_remaining -= skip;
        let available = &self.output_buffer[skip..produced];
        let remaining = cap.saturating_sub(self.canonical_emitted);
        let take = available
            .len()
            .min(usize::try_from(remaining).unwrap_or(usize::MAX));
        output.extend_from_slice(&available[..take]);
        self.canonical_emitted = self
            .canonical_emitted
            .checked_add(take as u64)
            .ok_or(ExtendedCaptureFault::SampleAccountingOverflow)?;
        Ok(())
    }
}

fn canonical_samples(duration: Duration) -> u64 {
    let whole = duration
        .as_secs()
        .saturating_mul(u64::from(WHISPER_SAMPLE_RATE));
    let fractional = u64::from(duration.subsec_nanos())
        .saturating_mul(u64::from(WHISPER_SAMPLE_RATE))
        / 1_000_000_000;
    whole.saturating_add(fractional)
}

fn resampled_len(native_frames: u64, source_rate: u32) -> Result<u64, ExtendedCaptureFault> {
    let numerator = u128::from(native_frames) * u128::from(WHISPER_SAMPLE_RATE);
    let denominator = u128::from(source_rate);
    let output = numerator.div_ceil(denominator);
    u64::try_from(output).map_err(|_| ExtendedCaptureFault::SampleAccountingOverflow)
}

#[cfg(test)]
mod tests {
    use std::io::{self, Read, Seek, SeekFrom, Write};
    use std::sync::atomic::AtomicBool;
    use std::sync::{Arc, Mutex};

    use super::*;

    #[derive(Default)]
    struct MemoryState {
        bytes: Vec<u8>,
        cursor: u64,
        cleaned: bool,
        write_limit: Option<usize>,
        write_chunk: Option<usize>,
        cleanup_failures_remaining: usize,
        cleanup_attempts: usize,
    }

    #[derive(Clone, Default)]
    struct MemoryHandle(Arc<Mutex<MemoryState>>);

    type TestSpoolCreator =
        Box<dyn FnMut() -> Result<EncryptedAudioSpool<MemoryStorage>, SpoolError>>;

    struct MemoryStorage(MemoryHandle);

    impl MemoryStorage {
        fn new() -> (Self, MemoryHandle) {
            let handle = MemoryHandle::default();
            (Self(handle.clone()), handle)
        }
    }

    impl Read for MemoryStorage {
        fn read(&mut self, output: &mut [u8]) -> io::Result<usize> {
            let mut state = self.0.0.lock().unwrap();
            let cursor = state.cursor as usize;
            let len = output.len().min(state.bytes.len().saturating_sub(cursor));
            output[..len].copy_from_slice(&state.bytes[cursor..cursor + len]);
            state.cursor += len as u64;
            Ok(len)
        }
    }

    impl Write for MemoryStorage {
        fn write(&mut self, input: &[u8]) -> io::Result<usize> {
            let mut state = self.0.0.lock().unwrap();
            let cursor = state.cursor as usize;
            if state.write_limit.is_some_and(|limit| cursor >= limit) {
                return Err(io::Error::other("injected disk failure"));
            }
            let allowed = state.write_limit.map_or(input.len(), |limit| {
                input.len().min(limit.saturating_sub(cursor))
            });
            let allowed = state
                .write_chunk
                .map_or(allowed, |chunk| allowed.min(chunk));
            if allowed == 0 {
                return Err(io::Error::other("injected disk failure"));
            }
            let end = cursor + allowed;
            if state.bytes.len() < end {
                state.bytes.resize(end, 0);
            }
            state.bytes[cursor..end].copy_from_slice(&input[..allowed]);
            state.cursor += allowed as u64;
            Ok(allowed)
        }

        fn flush(&mut self) -> io::Result<()> {
            Ok(())
        }
    }

    impl Seek for MemoryStorage {
        fn seek(&mut self, position: SeekFrom) -> io::Result<u64> {
            let mut state = self.0.0.lock().unwrap();
            let len = state.bytes.len() as i128;
            let next = match position {
                SeekFrom::Start(value) => i128::from(value),
                SeekFrom::End(value) => len + i128::from(value),
                SeekFrom::Current(value) => i128::from(state.cursor) + i128::from(value),
            };
            if next < 0 || next > i128::from(u64::MAX) {
                return Err(io::Error::other("invalid seek"));
            }
            state.cursor = next as u64;
            Ok(state.cursor)
        }
    }

    impl SpoolStorage for MemoryStorage {
        fn byte_len(&mut self) -> io::Result<u64> {
            Ok(self.0.0.lock().unwrap().bytes.len() as u64)
        }

        fn set_len(&mut self, len: u64) -> io::Result<()> {
            self.0.0.lock().unwrap().bytes.resize(len as usize, 0);
            Ok(())
        }

        fn sync_all(&mut self) -> io::Result<()> {
            Ok(())
        }

        fn cleanup(&mut self) -> io::Result<()> {
            let mut state = self.0.0.lock().unwrap();
            state.cleanup_attempts += 1;
            if state.cleanup_failures_remaining != 0 {
                state.cleanup_failures_remaining -= 1;
                return Err(io::Error::other("injected cleanup failure"));
            }
            state.bytes.clear();
            state.cleaned = true;
            Ok(())
        }
    }

    impl Drop for MemoryStorage {
        fn drop(&mut self) {
            let _ = SpoolStorage::cleanup(self);
        }
    }

    struct CountingStorage {
        len: u64,
        cursor: u64,
        cleaned: Arc<AtomicBool>,
    }

    impl Read for CountingStorage {
        fn read(&mut self, _output: &mut [u8]) -> io::Result<usize> {
            Ok(0)
        }
    }

    impl Write for CountingStorage {
        fn write(&mut self, input: &[u8]) -> io::Result<usize> {
            self.cursor = self
                .cursor
                .checked_add(input.len() as u64)
                .ok_or_else(|| io::Error::other("length overflow"))?;
            self.len = self.len.max(self.cursor);
            Ok(input.len())
        }

        fn flush(&mut self) -> io::Result<()> {
            Ok(())
        }
    }

    impl Seek for CountingStorage {
        fn seek(&mut self, position: SeekFrom) -> io::Result<u64> {
            let next = match position {
                SeekFrom::Start(value) => i128::from(value),
                SeekFrom::End(value) => i128::from(self.len) + i128::from(value),
                SeekFrom::Current(value) => i128::from(self.cursor) + i128::from(value),
            };
            if next < 0 || next > i128::from(u64::MAX) {
                return Err(io::Error::other("invalid seek"));
            }
            self.cursor = next as u64;
            Ok(self.cursor)
        }
    }

    impl SpoolStorage for CountingStorage {
        fn byte_len(&mut self) -> io::Result<u64> {
            Ok(self.len)
        }

        fn set_len(&mut self, len: u64) -> io::Result<()> {
            self.len = len;
            self.cursor = self.cursor.min(len);
            Ok(())
        }

        fn sync_all(&mut self) -> io::Result<()> {
            Ok(())
        }

        fn cleanup(&mut self) -> io::Result<()> {
            self.len = 0;
            self.cursor = 0;
            self.cleaned.store(true, Ordering::Release);
            Ok(())
        }
    }

    fn test_bounds(short_seconds: f64) -> CaptureBounds {
        let short_limit = (short_seconds * f64::from(WHISPER_SAMPLE_RATE)) as u64;
        CaptureBounds {
            prepare_at: short_limit.saturating_sub(u64::from(WHISPER_SAMPLE_RATE)),
            transition_at: short_limit.saturating_sub(u64::from(WHISPER_SAMPLE_RATE)),
            short_limit,
            record_samples: WHISPER_SAMPLE_RATE,
        }
    }

    fn test_engine(
        source_rate: u32,
        short_seconds: f64,
        quota: SpoolQuota,
    ) -> (
        CaptureEngine<MemoryStorage, TestSpoolCreator>,
        Arc<Mutex<Vec<MemoryHandle>>>,
    ) {
        let handles = Arc::new(Mutex::new(Vec::new()));
        let captured_handles = Arc::clone(&handles);
        let creator: TestSpoolCreator = Box::new(move || {
            let (storage, handle) = MemoryStorage::new();
            captured_handles.lock().unwrap().push(handle);
            EncryptedAudioSpool::from_empty_storage(storage, quota)
        });
        (
            CaptureEngine::new(source_rate, test_bounds(short_seconds), creator, None).unwrap(),
            handles,
        )
    }

    fn feed_zeros<S, C>(engine: &mut CaptureEngine<S, C>, frames: u64, chunk: usize)
    where
        S: SpoolStorage,
        C: FnMut() -> Result<EncryptedAudioSpool<S>, SpoolError>,
    {
        let block = vec![0.0; chunk];
        let mut remaining = frames;
        while remaining != 0 {
            let take = remaining.min(chunk as u64) as usize;
            engine.ingest_native(&block[..take]).unwrap();
            remaining -= take as u64;
        }
    }

    fn pattern_sample(index: u64) -> f32 {
        (index % 997) as f32 / 997.0 - 0.5
    }

    fn feed_pattern<S, C>(engine: &mut CaptureEngine<S, C>, frames: u64, chunk: usize)
    where
        S: SpoolStorage,
        C: FnMut() -> Result<EncryptedAudioSpool<S>, SpoolError>,
    {
        let mut start = 0;
        while start < frames {
            let end = (start + chunk as u64).min(frames);
            let block: Vec<f32> = (start..end).map(pattern_sample).collect();
            engine.ingest_native(&block).unwrap();
            start = end;
        }
    }

    fn feed_pattern_random_chunks<S, C>(engine: &mut CaptureEngine<S, C>, frames: u64)
    where
        S: SpoolStorage,
        C: FnMut() -> Result<EncryptedAudioSpool<S>, SpoolError>,
    {
        let mut start = 0_u64;
        let mut state = 0xA5A5_1234_u64;
        while start < frames {
            state = state
                .wrapping_mul(6_364_136_223_846_793_005)
                .wrapping_add(1);
            let chunk = 1 + (state % 509);
            let end = frames.min(start.saturating_add(chunk));
            let block: Vec<f32> = (start..end).map(pattern_sample).collect();
            engine.ingest_native(&block).unwrap();
            start = end;
        }
    }

    #[test]
    fn streaming_resampler_matches_exact_rational_accounting() {
        for source_rate in [44_100, 48_000, 96_000] {
            for frames in [1_u64, 1_001, u64::from(source_rate) * 7 + 317] {
                let mut canonicalizer = StreamingCanonicalizer::new(source_rate).unwrap();
                let block = vec![0.0_f32; 733];
                let mut remaining = frames;
                let mut output = Vec::new();
                while remaining != 0 {
                    let take = remaining.min(block.len() as u64) as usize;
                    output.extend_from_slice(&canonicalizer.push(&block[..take]).unwrap());
                    remaining -= take as u64;
                }
                output.extend_from_slice(&canonicalizer.finish().unwrap());
                assert_eq!(
                    output.len() as u64,
                    resampled_len(frames, source_rate).unwrap(),
                    "source rate {source_rate}, input frames {frames}"
                );
            }
        }
    }

    #[test]
    fn streaming_short_path_matches_existing_fft_resampler() {
        for source_rate in [44_100, 48_000, 96_000] {
            let frames = source_rate as usize * 2 + 317;
            let input: Vec<f32> = (0..frames)
                .map(|index| {
                    (2.0 * std::f32::consts::PI * 440.0 * index as f32 / source_rate as f32).sin()
                })
                .collect();
            let expected = crate::resample(&input, source_rate, WHISPER_SAMPLE_RATE).unwrap();
            let mut canonicalizer = StreamingCanonicalizer::new(source_rate).unwrap();
            let mut actual = Vec::new();
            for chunk in input.chunks(733) {
                actual.extend_from_slice(&canonicalizer.push(chunk).unwrap());
            }
            actual.extend_from_slice(&canonicalizer.finish().unwrap());
            assert_eq!(actual.len(), expected.len());
            let mismatch_count = actual
                .iter()
                .zip(&expected)
                .filter(|(actual, expected)| (*actual - *expected).abs() >= 1.0e-5)
                .count();
            let rms_error = (actual
                .iter()
                .zip(&expected)
                .map(|(actual, expected)| (actual - expected).powi(2))
                .sum::<f32>()
                / actual.len() as f32)
                .sqrt();
            assert!(
                mismatch_count <= 1 && rms_error < 0.01,
                "source {source_rate}, mismatches {mismatch_count}, RMS error {rms_error}"
            );
        }
    }

    #[test]
    fn boundary_119_9_120_120_1_is_lossless_and_switches_only_after_limit() {
        let quota = SpoolQuota {
            max_file_bytes: 64 * 1024 * 1024,
            max_samples: u64::from(WHISPER_SAMPLE_RATE) * 121,
            max_records: 1_000,
            max_record_samples: WHISPER_SAMPLE_RATE,
            max_read_samples: WHISPER_SAMPLE_RATE * 121,
        };
        for (tenths, extended) in [(1_199_u64, false), (1_200, false), (1_201, true)] {
            let samples = tenths * u64::from(WHISPER_SAMPLE_RATE) / 10;
            let (mut engine, _) = test_engine(WHISPER_SAMPLE_RATE, 120.0, quota);
            feed_pattern_random_chunks(&mut engine, samples);
            assert!(engine.memory.len() as u64 <= u64::from(WHISPER_SAMPLE_RATE) * 120);
            assert!(engine.spool_pending.len() < WHISPER_SAMPLE_RATE as usize);
            let mut captured = engine.finalize(0).unwrap();
            assert_eq!(captured.total_samples, samples);
            assert_eq!(
                matches!(captured.storage, FinalizedStorage::Spool(_)),
                extended
            );
            let transition = u64::from(WHISPER_SAMPLE_RATE) * 119;
            for range in [
                SampleRange::new(0, 32).unwrap(),
                SampleRange::new(transition - 16, transition + 16).unwrap(),
                SampleRange::new(samples - 32, samples).unwrap(),
            ] {
                let snapshot = captured.snapshot(range).unwrap();
                let expected: Vec<f32> = (range.start()..range.end()).map(pattern_sample).collect();
                assert_eq!(snapshot.samples(), expected);
            }
        }
    }

    #[test]
    fn transition_backfill_survives_short_storage_writes_without_coverage_gaps() {
        let handles = Arc::new(Mutex::new(Vec::new()));
        let captured_handles = Arc::clone(&handles);
        let creator = move || {
            let (storage, handle) = MemoryStorage::new();
            handle.0.lock().unwrap().write_chunk = Some(4_096);
            captured_handles.lock().unwrap().push(handle);
            EncryptedAudioSpool::from_empty_storage(storage, SpoolQuota::default())
        };
        let mut engine =
            CaptureEngine::new(WHISPER_SAMPLE_RATE, test_bounds(120.0), creator, None).unwrap();
        let total = u64::from(WHISPER_SAMPLE_RATE) * 1_201 / 10;
        feed_pattern_random_chunks(&mut engine, total);
        let mut captured = engine.finalize(0).unwrap();
        for range in [
            SampleRange::new(0, 257).unwrap(),
            SampleRange::new(
                u64::from(WHISPER_SAMPLE_RATE) * 115 - 137,
                u64::from(WHISPER_SAMPLE_RATE) * 115 + 263,
            )
            .unwrap(),
            SampleRange::new(total - 509, total).unwrap(),
        ] {
            let actual = captured.snapshot(range).unwrap();
            let expected = (range.start()..range.end())
                .map(pattern_sample)
                .collect::<Vec<_>>();
            assert_eq!(actual.samples(), expected);
        }
    }

    #[test]
    fn thirty_one_minutes_keeps_only_bounded_resident_audio() {
        let quota = SpoolQuota {
            max_file_bytes: 256 * 1024 * 1024,
            max_samples: u64::from(WHISPER_SAMPLE_RATE) * 60 * 32,
            max_records: 2_000,
            max_record_samples: WHISPER_SAMPLE_RATE,
            max_read_samples: WHISPER_SAMPLE_RATE,
        };
        let cleaned = Arc::new(AtomicBool::new(false));
        let cleanup_probe = Arc::clone(&cleaned);
        let creator = move || {
            EncryptedAudioSpool::from_empty_storage(
                CountingStorage {
                    len: 0,
                    cursor: 0,
                    cleaned: Arc::clone(&cleanup_probe),
                },
                quota,
            )
        };
        let mut engine =
            CaptureEngine::new(WHISPER_SAMPLE_RATE, test_bounds(120.0), creator, None).unwrap();
        let total = u64::from(WHISPER_SAMPLE_RATE) * 60 * 31;
        feed_zeros(&mut engine, total, WHISPER_SAMPLE_RATE as usize);
        assert!(engine.memory.is_empty());
        assert!(engine.spool_pending.len() <= WHISPER_SAMPLE_RATE as usize);
        assert_eq!(engine.canonical_samples, total);
        let captured = engine.finalize(0).unwrap();
        assert!(matches!(captured.storage, FinalizedStorage::Spool(_)));
        assert_eq!(captured.total_samples, total);
        drop(captured);
        assert!(cleaned.load(Ordering::Acquire));
    }

    #[test]
    fn empty_stalls_do_not_create_coverage_gaps() {
        let (mut engine, _) = test_engine(48_000, 120.0, SpoolQuota::default());
        engine.ingest_native(&vec![0.0; 48_000]).unwrap();
        let before = engine.canonicalizer.native_received;
        for _ in 0..100 {
            engine.ingest_native(&[]).unwrap();
        }
        assert_eq!(engine.canonicalizer.native_received, before);
        let captured = engine.finalize(0).unwrap();
        assert_eq!(captured.total_samples, u64::from(WHISPER_SAMPLE_RATE));
    }

    #[test]
    fn callback_overflow_is_observable_without_callback_allocation() {
        let (mut producer, mut consumer) = HeapRb::<f32>::new(2).split();
        let progress = SharedProgress::new();
        push_extended_mono_frames(&mut producer, &[1.0_f32, 2.0, 3.0], 1, &progress);
        assert_eq!(consumer.pop_iter().collect::<Vec<_>>(), vec![1.0, 2.0]);
        let snapshot = progress.snapshot();
        assert_eq!(snapshot.native_frames_observed, 3);
        assert_eq!(snapshot.dropped_native_frames, 1);
        assert_eq!(
            snapshot.sticky_fault,
            Some(ExtendedCaptureFault::CallbackOverflow)
        );
    }

    #[test]
    fn quota_error_is_sticky_and_cleans_spool() {
        let quota = SpoolQuota {
            max_file_bytes: 1024,
            max_samples: 1_000_000,
            max_records: 10,
            max_record_samples: WHISPER_SAMPLE_RATE,
            max_read_samples: WHISPER_SAMPLE_RATE,
        };
        let (mut engine, handles) = test_engine(WHISPER_SAMPLE_RATE, 2.0, quota);
        let error = engine.ingest_native(&vec![0.0; WHISPER_SAMPLE_RATE as usize * 2]);
        assert_eq!(error.unwrap_err(), ExtendedCaptureFault::SpoolQuota);
        assert_eq!(
            engine.ingest_native(&[0.0]).unwrap_err(),
            ExtendedCaptureFault::SpoolQuota
        );
        drop(engine);
        assert!(handles.lock().unwrap()[0].0.lock().unwrap().cleaned);
    }

    #[test]
    fn cancellation_drop_cleans_a_prepared_spool_without_finalization() {
        let (mut engine, handles) = test_engine(WHISPER_SAMPLE_RATE, 2.0, SpoolQuota::default());
        feed_zeros(&mut engine, u64::from(WHISPER_SAMPLE_RATE) * 3 / 2, 4_000);
        assert!(engine.spool.is_some());
        drop(engine);
        assert!(handles.lock().unwrap()[0].0.lock().unwrap().cleaned);
    }

    #[test]
    fn injected_disk_failure_is_sticky_and_fail_closed() {
        let handles = Arc::new(Mutex::new(Vec::new()));
        let captured_handles = Arc::clone(&handles);
        let creator = move || {
            let (storage, handle) = MemoryStorage::new();
            let spool = EncryptedAudioSpool::from_empty_storage(storage, SpoolQuota::default())?;
            handle.0.lock().unwrap().write_limit = Some(128);
            captured_handles.lock().unwrap().push(handle);
            Ok(spool)
        };
        let mut engine =
            CaptureEngine::new(WHISPER_SAMPLE_RATE, test_bounds(2.0), creator, None).unwrap();
        assert_eq!(
            engine
                .ingest_native(&vec![0.0; WHISPER_SAMPLE_RATE as usize * 2])
                .unwrap_err(),
            ExtendedCaptureFault::SpoolIo
        );
        assert!(matches!(
            engine.finalize(0),
            Err(ExtendedCaptureFault::SpoolIo)
        ));
        drop(handles);
    }

    #[test]
    fn finalized_spool_tamper_fails_closed_and_drop_cleans() {
        let quota = SpoolQuota {
            max_record_samples: WHISPER_SAMPLE_RATE,
            max_read_samples: WHISPER_SAMPLE_RATE * 3,
            ..SpoolQuota::default()
        };
        let (mut engine, handles) = test_engine(WHISPER_SAMPLE_RATE, 2.0, quota);
        feed_zeros(
            &mut engine,
            u64::from(WHISPER_SAMPLE_RATE) * 3,
            WHISPER_SAMPLE_RATE as usize,
        );
        let mut captured = engine.finalize(0).unwrap();
        {
            let handle = handles.lock().unwrap()[0].clone();
            let mut state = handle.0.lock().unwrap();
            let last = state.bytes.len() - 1;
            state.bytes[last] ^= 1;
        }
        assert_eq!(
            captured
                .snapshot(SampleRange::new(0, u64::from(WHISPER_SAMPLE_RATE) * 3).unwrap())
                .unwrap_err(),
            ExtendedCaptureFault::SpoolIntegrity
        );
        assert_eq!(
            captured
                .snapshot(SampleRange::new(0, 1).unwrap())
                .unwrap_err(),
            ExtendedCaptureFault::SpoolIntegrity,
            "integrity failures remain sticky after the first failed read"
        );
        drop(captured);
        assert!(handles.lock().unwrap()[0].0.lock().unwrap().cleaned);
    }

    #[test]
    fn backend_error_fails_closed_and_drops_prepared_spool() {
        let (mut engine, handles) = test_engine(WHISPER_SAMPLE_RATE, 2.0, SpoolQuota::default());
        feed_zeros(&mut engine, u64::from(WHISPER_SAMPLE_RATE) * 3 / 2, 4_000);
        assert!(engine.spool.is_some());
        assert!(matches!(
            engine.finalize(1),
            Err(ExtendedCaptureFault::StreamFailed)
        ));
        assert!(handles.lock().unwrap()[0].0.lock().unwrap().cleaned);
    }

    #[test]
    fn backend_fault_published_at_stop_is_imported_before_finalization() {
        let progress = SharedProgress::new();
        let (mut engine, _) = test_engine(WHISPER_SAMPLE_RATE, 120.0, SpoolQuota::default());
        feed_zeros(&mut engine, u64::from(WHISPER_SAMPLE_RATE), 4_000);

        // Models the callback publishing a device failure after the app's last
        // progress snapshot but before the stopped pump returns its result.
        progress.set_fault(ExtendedCaptureFault::StreamFailed);
        propagate_shared_fault(&mut engine, &progress);

        assert!(matches!(
            engine.finalize(1),
            Err(ExtendedCaptureFault::StreamFailed)
        ));
    }

    #[test]
    fn explicit_extended_cleanup_surfaces_storage_failure() {
        let handles = Arc::new(Mutex::new(Vec::new()));
        let captured_handles = Arc::clone(&handles);
        let creator = move || {
            let (storage, handle) = MemoryStorage::new();
            captured_handles.lock().unwrap().push(handle);
            EncryptedAudioSpool::from_empty_storage(storage, SpoolQuota::default())
        };
        let mut engine =
            CaptureEngine::new(WHISPER_SAMPLE_RATE, test_bounds(2.0), creator, None).unwrap();
        feed_zeros(
            &mut engine,
            u64::from(WHISPER_SAMPLE_RATE) * 3,
            WHISPER_SAMPLE_RATE as usize,
        );
        handles.lock().unwrap()[0]
            .0
            .lock()
            .unwrap()
            .cleanup_failures_remaining = usize::MAX;
        let captured = engine.finalize(0).unwrap();
        let FinalizedStorage::Spool(spool) = captured.storage else {
            panic!("extended capture must own a spool");
        };
        assert!(matches!(spool.cleanup(), Err(SpoolError::Io(_))));
        assert!(!handles.lock().unwrap()[0].0.lock().unwrap().cleaned);
    }

    #[test]
    fn prepared_spool_cleanup_failure_prevents_short_success() {
        let handles = Arc::new(Mutex::new(Vec::new()));
        let captured_handles = Arc::clone(&handles);
        let creator = move || {
            let (storage, handle) = MemoryStorage::new();
            captured_handles.lock().unwrap().push(handle);
            EncryptedAudioSpool::from_empty_storage(storage, SpoolQuota::default())
        };
        let mut engine =
            CaptureEngine::new(WHISPER_SAMPLE_RATE, test_bounds(2.0), creator, None).unwrap();
        feed_zeros(&mut engine, u64::from(WHISPER_SAMPLE_RATE) * 3 / 2, 4_000);
        handles.lock().unwrap()[0]
            .0
            .lock()
            .unwrap()
            .cleanup_failures_remaining = usize::MAX;
        assert!(matches!(
            engine.finalize(0),
            Err(ExtendedCaptureFault::SpoolIo)
        ));
    }

    #[test]
    fn transient_cleanup_failure_is_retried_but_still_fails_closed() {
        let handles = Arc::new(Mutex::new(Vec::new()));
        let captured_handles = Arc::clone(&handles);
        let creator = move || {
            let (storage, handle) = MemoryStorage::new();
            captured_handles.lock().unwrap().push(handle);
            EncryptedAudioSpool::from_empty_storage(storage, SpoolQuota::default())
        };
        let mut engine =
            CaptureEngine::new(WHISPER_SAMPLE_RATE, test_bounds(2.0), creator, None).unwrap();
        feed_zeros(&mut engine, u64::from(WHISPER_SAMPLE_RATE) * 3 / 2, 4_000);
        let handle = handles.lock().unwrap()[0].clone();
        handle.0.lock().unwrap().cleanup_failures_remaining = 1;

        assert!(matches!(
            engine.finalize(0),
            Err(ExtendedCaptureFault::SpoolIo)
        ));
        let state = handle.0.lock().unwrap();
        assert!(state.cleaned, "drop must retry a transient cleanup failure");
        assert!(state.cleanup_attempts >= 2);
    }

    #[test]
    fn cancellation_retries_transient_cleanup_without_reporting_success() {
        let (mut engine, handles) = test_engine(WHISPER_SAMPLE_RATE, 2.0, SpoolQuota::default());
        feed_zeros(&mut engine, u64::from(WHISPER_SAMPLE_RATE) * 3 / 2, 4_000);
        let handle = handles.lock().unwrap()[0].clone();
        handle.0.lock().unwrap().cleanup_failures_remaining = 1;
        drop(engine);

        let state = handle.0.lock().unwrap();
        assert!(state.cleaned);
        assert!(state.cleanup_attempts >= 2);
    }

    #[test]
    fn ordinary_short_recording_never_creates_a_spool() {
        let (mut engine, handles) = test_engine(WHISPER_SAMPLE_RATE, 120.0, SpoolQuota::default());
        feed_zeros(&mut engine, u64::from(WHISPER_SAMPLE_RATE) * 10, 4_000);
        assert!(engine.spool.is_none());
        assert!(handles.lock().unwrap().is_empty());
        let captured = engine.finalize(0).unwrap();
        assert!(matches!(captured.storage, FinalizedStorage::Memory(_)));
        assert!(handles.lock().unwrap().is_empty());
    }

    #[test]
    fn dropping_an_active_recording_detaches_a_stalled_worker() {
        let stop = Arc::new(AtomicBool::new(false));
        let progress = Arc::new(SharedProgress::new());
        let (commands, _requests) = mpsc::sync_channel(1);
        let (finalized_tx, finalized_rx) = mpsc::sync_channel(1);
        let worker = thread::spawn(move || {
            thread::sleep(Duration::from_millis(150));
            let _ = finalized_tx.try_send(Err(ExtendedCaptureFault::SpoolIo));
        });
        let recording = ExtendedRecording {
            stream: None,
            stop: Arc::clone(&stop),
            commands,
            worker: Some(worker),
            finalized: Some(finalized_rx),
            progress,
            streaming_cursor: 0,
            auto_stopped: false,
        };
        let started = std::time::Instant::now();
        drop(recording);
        assert!(started.elapsed() < Duration::from_millis(50));
        assert!(stop.load(Ordering::Acquire));
    }

    #[test]
    fn live_snapshot_frequency_does_not_fragment_spool_records() {
        let quota = SpoolQuota {
            max_record_samples: WHISPER_SAMPLE_RATE,
            max_read_samples: WHISPER_SAMPLE_RATE * 2,
            ..SpoolQuota::default()
        };
        let (mut engine, _) = test_engine(WHISPER_SAMPLE_RATE, 2.0, quota);
        feed_pattern(&mut engine, u64::from(WHISPER_SAMPLE_RATE) * 5 / 2, 4_000);
        let records_before = engine.spool.as_ref().unwrap().indexed_ranges().len();
        let spooled_before = engine.spooled_through;
        let start = u64::from(WHISPER_SAMPLE_RATE) * 3 / 2;
        let end = u64::from(WHISPER_SAMPLE_RATE) * 5 / 2;
        for _ in 0..100 {
            let span = engine
                .snapshot(SampleRange::new(start, end).unwrap())
                .unwrap();
            assert_eq!(span.range().len(), u64::from(WHISPER_SAMPLE_RATE));
        }
        assert_eq!(
            engine.spool.as_ref().unwrap().indexed_ranges().len(),
            records_before
        );
        assert_eq!(engine.spooled_through, spooled_before);
        assert_eq!(engine.spool_pending.len(), WHISPER_SAMPLE_RATE as usize / 2);
    }

    #[test]
    fn deferred_finalization_drop_never_waits_for_storage_worker() {
        let (finalized_tx, finalized_rx) = mpsc::sync_channel(1);
        let worker = thread::spawn(move || {
            thread::sleep(Duration::from_millis(150));
            let _ = finalized_tx.try_send(Err(ExtendedCaptureFault::SpoolIo));
        });
        let deferred = DeferredCapturedAudio {
            worker: Some(worker),
            finalized: Some(finalized_rx),
        };
        let started = std::time::Instant::now();
        drop(deferred);
        assert!(started.elapsed() < Duration::from_millis(50));
    }

    #[test]
    fn deferred_finalization_has_a_finite_deadline() {
        let (release_tx, release_rx) = mpsc::sync_channel(1);
        let (finalized_tx, finalized_rx) = mpsc::sync_channel(1);
        let worker = thread::spawn(move || {
            let _ = release_rx.recv();
            let _ = finalized_tx.try_send(Err(ExtendedCaptureFault::SpoolIo));
        });
        let mut deferred = DeferredCapturedAudio {
            worker: Some(worker),
            finalized: Some(finalized_rx),
        };
        let started = std::time::Instant::now();
        assert!(matches!(
            deferred.resolve_with_timeout(Duration::from_millis(20)),
            Err(CaptureError::Extended(
                ExtendedCaptureFault::FinalizationTimeout
            ))
        ));
        assert!(started.elapsed() < Duration::from_secs(1));
        release_tx.send(()).unwrap();
    }

    #[test]
    fn one_stalled_finalizer_blocks_repeated_worker_creation_until_release() {
        let active = Arc::new(AtomicBool::new(false));
        let permit = FinalizerPermit::acquire(&active).unwrap();
        let (release_tx, release_rx) = mpsc::sync_channel(1);
        let worker = thread::spawn(move || {
            let _permit = permit;
            let _ = release_rx.recv();
        });
        for _ in 0..100 {
            assert!(matches!(
                FinalizerPermit::acquire(&active),
                Err(CaptureError::Extended(ExtendedCaptureFault::FinalizerBusy))
            ));
        }
        release_tx.send(()).unwrap();
        worker.join().unwrap();
        let replacement = FinalizerPermit::acquire(&active).unwrap();
        assert!(active.load(Ordering::Acquire));
        drop(replacement);
        assert!(!active.load(Ordering::Acquire));
    }

    #[test]
    fn cleanup_timeout_detaches_one_owner_and_holds_the_global_permit() {
        let active = Arc::new(AtomicBool::new(false));
        let permit = FinalizerPermit::acquire(&active).unwrap();
        let (release_tx, release_rx) = mpsc::sync_channel(1);
        let started = std::time::Instant::now();
        assert!(matches!(
            run_cleanup_with_timeout(
                move || {
                    let _ = release_rx.recv();
                    Ok(())
                },
                permit,
                Duration::from_millis(20),
            ),
            Err(CaptureError::Extended(
                ExtendedCaptureFault::FinalizationTimeout
            ))
        ));
        assert!(started.elapsed() < Duration::from_millis(100));
        assert!(active.load(Ordering::Acquire));
        assert!(matches!(
            FinalizerPermit::acquire(&active),
            Err(CaptureError::Extended(ExtendedCaptureFault::FinalizerBusy))
        ));
        release_tx.send(()).unwrap();
        let deadline = std::time::Instant::now() + Duration::from_secs(1);
        while active.load(Ordering::Acquire) && std::time::Instant::now() < deadline {
            thread::sleep(Duration::from_millis(1));
        }
        assert!(!active.load(Ordering::Acquire));
    }
}
