//! Microphone capture and WAV utilities for Phorminx.

use std::collections::TryReserveError;
use std::path::Path;
use std::sync::Arc;
use std::sync::atomic::{AtomicU64, Ordering};
use std::thread;
use std::time::Duration;

use cpal::traits::{DeviceTrait, HostTrait, StreamTrait};
use cpal::{FromSample, Sample, SampleFormat, StreamConfig};
use phorminx_core::AudioClip;
use ringbuf::traits::{Consumer, Observer, Producer, Split};
use ringbuf::{HeapCons, HeapProd, HeapRb};
use rubato::audioadapter_buffers::direct::InterleavedSlice;
use rubato::{Fft, FixedSync, Resampler};

pub const WHISPER_SAMPLE_RATE: u32 = 16_000;
/// Phase 1 rejects longer recordings instead of growing memory without a bound.
pub const MAX_RECORDING_DURATION: Duration = Duration::from_secs(120);
const RESAMPLER_CHUNK_FRAMES: usize = 1_024;

#[derive(Clone, Debug)]
pub struct InputDevice {
    pub name: String,
    pub is_default: bool,
}

pub fn input_devices() -> Result<Vec<InputDevice>, CaptureError> {
    let host = cpal::default_host();
    let default_name = host.default_input_device().map(|device| device.to_string());

    let devices = host
        .input_devices()
        .map_err(CaptureError::EnumerateDevices)?
        .map(|device| {
            let name = device.to_string();
            InputDevice {
                is_default: default_name.as_deref() == Some(name.as_str()),
                name,
            }
        })
        .collect();

    Ok(devices)
}

/// An in-progress recording from the default microphone.
pub struct ActiveRecording {
    stream: cpal::Stream,
    captured: HeapCons<f32>,
    dropped_samples: Arc<AtomicU64>,
    backend_warning_count: Arc<AtomicU64>,
    sample_rate: u32,
}

#[derive(Clone, Debug)]
pub struct CapturedAudio {
    pub clip: AudioClip,
    /// Recoverable backend notifications observed while usable audio was captured.
    pub backend_warning_count: u64,
}

impl ActiveRecording {
    /// Stops capture and converts the recording to Whisper's 16 kHz mono format.
    pub fn finish(self) -> Result<AudioClip, CaptureError> {
        Ok(self.finish_with_diagnostics()?.clip)
    }

    /// Stops capture while retaining recoverable backend diagnostics.
    pub fn finish_with_diagnostics(self) -> Result<CapturedAudio, CaptureError> {
        let Self {
            stream,
            mut captured,
            dropped_samples,
            backend_warning_count,
            sample_rate,
        } = self;
        drop(stream);

        let dropped_samples = dropped_samples.load(Ordering::Acquire);
        if dropped_samples != 0 {
            return Err(CaptureError::RecordingLimitReached {
                max_seconds: MAX_RECORDING_DURATION.as_secs(),
                dropped_samples,
            });
        }

        let warning_count = backend_warning_count.load(Ordering::Acquire);
        let mut native_samples = Vec::with_capacity(captured.occupied_len());
        native_samples.extend(captured.pop_iter());
        drop(captured);
        if native_samples.is_empty() && warning_count != 0 {
            return Err(CaptureError::Stream { warning_count });
        }

        let samples = if sample_rate == WHISPER_SAMPLE_RATE {
            native_samples
        } else {
            resample(&native_samples, sample_rate, WHISPER_SAMPLE_RATE)?
        };
        let clip = AudioClip::new(samples, WHISPER_SAMPLE_RATE).map_err(CaptureError::Audio)?;
        Ok(CapturedAudio {
            clip,
            backend_warning_count: warning_count,
        })
    }
}

/// Starts recording from the default microphone and returns immediately.
pub fn start_default() -> Result<ActiveRecording, CaptureError> {
    start_input(None)
}

/// Starts recording from an explicitly selected microphone, or the Windows
/// default when `device_name` is `None`.
///
/// Device names are the stable identifiers exposed by CPAL on Windows. The
/// caller may recover a missing saved device by retrying with `None`; this
/// function never silently changes the requested input.
pub fn start_input(device_name: Option<&str>) -> Result<ActiveRecording, CaptureError> {
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
    let config: StreamConfig = supported.into();
    let channels = usize::from(config.channels);
    let sample_rate = config.sample_rate;
    let capacity = usize::try_from(
        u64::from(sample_rate)
            .checked_mul(MAX_RECORDING_DURATION.as_secs())
            .ok_or(CaptureError::CaptureCapacityOverflow)?,
    )
    .map_err(|_| CaptureError::CaptureCapacityOverflow)?;
    let (producer, captured) = HeapRb::<f32>::try_new(capacity)
        .map_err(CaptureError::CaptureBufferAllocation)?
        .split();
    let dropped_samples = Arc::new(AtomicU64::new(0));
    let backend_warning_count = Arc::new(AtomicU64::new(0));

    let stream = match sample_format {
        SampleFormat::I8 => build_stream::<i8>(
            &device,
            &config,
            channels,
            producer,
            Arc::clone(&dropped_samples),
            Arc::clone(&backend_warning_count),
        ),
        SampleFormat::I16 => build_stream::<i16>(
            &device,
            &config,
            channels,
            producer,
            Arc::clone(&dropped_samples),
            Arc::clone(&backend_warning_count),
        ),
        SampleFormat::I24 => build_stream::<cpal::I24>(
            &device,
            &config,
            channels,
            producer,
            Arc::clone(&dropped_samples),
            Arc::clone(&backend_warning_count),
        ),
        SampleFormat::I32 => build_stream::<i32>(
            &device,
            &config,
            channels,
            producer,
            Arc::clone(&dropped_samples),
            Arc::clone(&backend_warning_count),
        ),
        SampleFormat::I64 => build_stream::<i64>(
            &device,
            &config,
            channels,
            producer,
            Arc::clone(&dropped_samples),
            Arc::clone(&backend_warning_count),
        ),
        SampleFormat::U8 => build_stream::<u8>(
            &device,
            &config,
            channels,
            producer,
            Arc::clone(&dropped_samples),
            Arc::clone(&backend_warning_count),
        ),
        SampleFormat::U16 => build_stream::<u16>(
            &device,
            &config,
            channels,
            producer,
            Arc::clone(&dropped_samples),
            Arc::clone(&backend_warning_count),
        ),
        SampleFormat::U24 => build_stream::<cpal::U24>(
            &device,
            &config,
            channels,
            producer,
            Arc::clone(&dropped_samples),
            Arc::clone(&backend_warning_count),
        ),
        SampleFormat::U32 => build_stream::<u32>(
            &device,
            &config,
            channels,
            producer,
            Arc::clone(&dropped_samples),
            Arc::clone(&backend_warning_count),
        ),
        SampleFormat::U64 => build_stream::<u64>(
            &device,
            &config,
            channels,
            producer,
            Arc::clone(&dropped_samples),
            Arc::clone(&backend_warning_count),
        ),
        SampleFormat::F32 => build_stream::<f32>(
            &device,
            &config,
            channels,
            producer,
            Arc::clone(&dropped_samples),
            Arc::clone(&backend_warning_count),
        ),
        SampleFormat::F64 => build_stream::<f64>(
            &device,
            &config,
            channels,
            producer,
            Arc::clone(&dropped_samples),
            Arc::clone(&backend_warning_count),
        ),
        other => return Err(CaptureError::UnsupportedSampleFormat(other)),
    }?;

    stream.play().map_err(CaptureError::PlayStream)?;

    Ok(ActiveRecording {
        stream,
        captured,
        dropped_samples,
        backend_warning_count,
        sample_rate,
    })
}

/// Records from the default microphone for a fixed duration.
pub fn record_default(duration: Duration) -> Result<AudioClip, CaptureError> {
    let recording = start_default()?;
    thread::sleep(duration);
    recording.finish()
}

fn build_stream<T>(
    device: &cpal::Device,
    config: &StreamConfig,
    channels: usize,
    mut producer: HeapProd<f32>,
    dropped_samples: Arc<AtomicU64>,
    backend_warning_count: Arc<AtomicU64>,
) -> Result<cpal::Stream, CaptureError>
where
    T: cpal::SizedSample + Sample,
    f32: FromSample<T>,
{
    device
        .build_input_stream(
            *config,
            move |data: &[T], _| {
                push_mono_frames(&mut producer, data, channels, &dropped_samples);
            },
            move |_error| {
                backend_warning_count.fetch_add(1, Ordering::Relaxed);
            },
            None,
        )
        .map_err(CaptureError::BuildStream)
}

fn push_mono_frames<T>(
    producer: &mut HeapProd<f32>,
    data: &[T],
    channels: usize,
    dropped_samples: &AtomicU64,
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
    let dropped = frame_count.saturating_sub(pushed);
    if dropped != 0 {
        dropped_samples.fetch_add(dropped as u64, Ordering::Relaxed);
    }
}

/// Converts a complete mono clip with Rubato's band-limited FFT resampler.
///
/// This runs only after the capture stream has stopped, never on CPAL's callback.
pub fn resample(
    input: &[f32],
    source_rate: u32,
    target_rate: u32,
) -> Result<Vec<f32>, CaptureError> {
    if source_rate == 0 || target_rate == 0 {
        return Err(CaptureError::InvalidSampleRate {
            source_rate,
            target_rate,
        });
    }

    if input.is_empty() {
        return Ok(Vec::new());
    }

    if source_rate == target_rate {
        return Ok(input.to_vec());
    }

    let adapter = InterleavedSlice::new(input, 1, input.len())
        .map_err(|error| CaptureError::Resample(error.to_string()))?;
    let mut resampler = Fft::<f32>::new(
        source_rate as usize,
        target_rate as usize,
        RESAMPLER_CHUNK_FRAMES,
        1,
        FixedSync::Both,
    )
    .map_err(|error| CaptureError::Resample(error.to_string()))?;
    let output = resampler
        .process_all(&adapter, input.len(), None)
        .map_err(|error| CaptureError::Resample(error.to_string()))?;
    Ok(output.take_data())
}

pub fn write_wav(path: &Path, clip: &AudioClip) -> Result<(), CaptureError> {
    let spec = hound::WavSpec {
        channels: 1,
        sample_rate: clip.sample_rate,
        bits_per_sample: 16,
        sample_format: hound::SampleFormat::Int,
    };
    let mut writer = hound::WavWriter::create(path, spec).map_err(CaptureError::Wav)?;

    for sample in &clip.samples {
        let value = (sample.clamp(-1.0, 1.0) * i16::MAX as f32).round() as i16;
        writer.write_sample(value).map_err(CaptureError::Wav)?;
    }

    writer.finalize().map_err(CaptureError::Wav)
}

pub fn read_wav(path: &Path) -> Result<AudioClip, CaptureError> {
    let mut reader = hound::WavReader::open(path).map_err(CaptureError::Wav)?;
    let spec = reader.spec();
    if spec.channels == 0 {
        return Err(CaptureError::InvalidWav("zero channels"));
    }

    let channels = usize::from(spec.channels);
    let interleaved = match (spec.sample_format, spec.bits_per_sample) {
        (hound::SampleFormat::Int, 16) => reader
            .samples::<i16>()
            .map(|sample| sample.map(|value| value as f32 / i16::MAX as f32))
            .collect::<Result<Vec<_>, _>>()
            .map_err(CaptureError::Wav)?,
        (hound::SampleFormat::Float, 32) => reader
            .samples::<f32>()
            .collect::<Result<Vec<_>, _>>()
            .map_err(CaptureError::Wav)?,
        _ => return Err(CaptureError::UnsupportedWavFormat),
    };

    let mono = interleaved
        .chunks(channels)
        .map(|frame| frame.iter().sum::<f32>() / channels as f32)
        .collect::<Vec<_>>();
    let samples = resample(&mono, spec.sample_rate, WHISPER_SAMPLE_RATE)?;
    AudioClip::new(samples, WHISPER_SAMPLE_RATE).map_err(CaptureError::Audio)
}

#[derive(Debug, thiserror::Error)]
pub enum CaptureError {
    #[error("no default microphone is configured")]
    NoDefaultInputDevice,
    #[error("the selected microphone is no longer available: {0}")]
    InputDeviceNotFound(String),
    #[error("failed to enumerate input devices: {0}")]
    EnumerateDevices(cpal::Error),
    #[error("failed to read the default microphone configuration: {0}")]
    DefaultConfig(cpal::Error),
    #[error("unsupported microphone sample format: {0:?}")]
    UnsupportedSampleFormat(SampleFormat),
    #[error("failed to create microphone stream: {0}")]
    BuildStream(cpal::Error),
    #[error("failed to start microphone stream: {0}")]
    PlayStream(cpal::Error),
    #[error(
        "microphone stream failed before producing audio ({warning_count} backend notifications)"
    )]
    Stream { warning_count: u64 },
    #[error("recording capacity calculation overflowed")]
    CaptureCapacityOverflow,
    #[error("failed to allocate the bounded audio capture buffer")]
    CaptureBufferAllocation(#[source] TryReserveError),
    #[error(
        "recording exceeded the {max_seconds}-second limit and was rejected ({dropped_samples} samples omitted)"
    )]
    RecordingLimitReached {
        max_seconds: u64,
        dropped_samples: u64,
    },
    #[error("audio resampling failed: {0}")]
    Resample(String),
    #[error("sample rates must be nonzero (source={source_rate}, target={target_rate})")]
    InvalidSampleRate { source_rate: u32, target_rate: u32 },
    #[error("invalid WAV file: {0}")]
    InvalidWav(&'static str),
    #[error("WAV must use 16-bit integer or 32-bit float samples")]
    UnsupportedWavFormat,
    #[error("WAV operation failed: {0}")]
    Wav(hound::Error),
    #[error(transparent)]
    Audio(#[from] phorminx_core::AudioError),
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn bypasses_resampling_at_target_rate() {
        let input = [0.0, 1.0, 0.0, -1.0];
        let output = resample(&input, WHISPER_SAMPLE_RATE, WHISPER_SAMPLE_RATE).unwrap();

        assert_eq!(output, input);
    }

    #[test]
    fn resampling_preserves_duration() {
        for source_rate in [8_000, 44_100, 48_000, 96_000, 192_000] {
            let input = sine_wave(source_rate, 1_000.0);
            let output = resample(&input, source_rate, WHISPER_SAMPLE_RATE).unwrap();

            assert_eq!(output.len(), WHISPER_SAMPLE_RATE as usize);
            assert!(output.iter().all(|sample| sample.is_finite()));
            assert!((rms(&output) - std::f32::consts::FRAC_1_SQRT_2).abs() < 0.02);
        }
    }

    #[test]
    fn downsampling_rejects_energy_above_target_nyquist() {
        let input = sine_wave(48_000, 12_000.0);
        let output = resample(&input, 48_000, WHISPER_SAMPLE_RATE).unwrap();

        assert!(rms(&output) < 0.02);
    }

    #[test]
    fn resamples_short_and_odd_length_clips() {
        for input_len in [1, 159, 160, 319, 320, 1_023, 1_024, 1_025] {
            let input = vec![0.25; input_len];
            let output = resample(&input, 48_000, WHISPER_SAMPLE_RATE).unwrap();
            let expected_len = (input_len * WHISPER_SAMPLE_RATE as usize).div_ceil(48_000);

            assert_eq!(output.len(), expected_len, "input length {input_len}");
            assert!(output.iter().all(|sample| sample.is_finite()));
        }
    }

    #[test]
    fn rejects_zero_sample_rates_at_the_resampler_boundary() {
        assert!(matches!(
            resample(&[0.0], 0, WHISPER_SAMPLE_RATE),
            Err(CaptureError::InvalidSampleRate { .. })
        ));
        assert!(matches!(
            resample(&[0.0], WHISPER_SAMPLE_RATE, 0),
            Err(CaptureError::InvalidSampleRate { .. })
        ));
    }

    #[test]
    fn bounded_capture_counts_dropped_mono_frames() {
        let (mut producer, mut consumer) = HeapRb::<f32>::new(2).split();
        let dropped = AtomicU64::new(0);
        let stereo = [1.0_f32, 3.0, 2.0, 4.0, 10.0, 12.0];

        push_mono_frames(&mut producer, &stereo, 2, &dropped);

        assert_eq!(consumer.pop_iter().collect::<Vec<_>>(), vec![2.0, 3.0]);
        assert_eq!(dropped.load(Ordering::Relaxed), 1);
    }

    #[test]
    fn downmixes_more_than_two_channels() {
        let (mut producer, mut consumer) = HeapRb::<f32>::new(1).split();
        let dropped = AtomicU64::new(0);

        push_mono_frames(
            &mut producer,
            &[1.0_f32, 2.0, 3.0, 4.0, 5.0, 6.0],
            6,
            &dropped,
        );

        assert_eq!(consumer.try_pop(), Some(3.5));
        assert_eq!(dropped.load(Ordering::Relaxed), 0);
    }

    #[test]
    fn converts_unsigned_and_extended_pcm_to_normalized_float() {
        let u16_output = capture_test_frames(&[u16::MIN, 1 << 15, u16::MAX]);
        assert!(u16_output[0] <= -0.999);
        assert!(u16_output[1].abs() < 0.000_1);
        assert!(u16_output[2] >= 0.999);

        let i24_output = capture_test_frames(&[
            cpal::I24::new(-(1 << 23)).unwrap(),
            cpal::I24::new(0).unwrap(),
            cpal::I24::new((1 << 23) - 1).unwrap(),
        ]);
        assert!(i24_output[0] <= -0.999);
        assert_eq!(i24_output[1], 0.0);
        assert!(i24_output[2] >= 0.999);

        let u24_output = capture_test_frames(&[
            cpal::U24::new(0).unwrap(),
            cpal::U24::new(1 << 23).unwrap(),
            cpal::U24::new((1 << 24) - 1).unwrap(),
        ]);
        assert!(u24_output[0] <= -0.999);
        assert!(u24_output[1].abs() < 0.000_1);
        assert!(u24_output[2] >= 0.999);

        assert_eq!(capture_test_frames(&[-1.0_f64, 0.0, 1.0]), [-1.0, 0.0, 1.0]);
    }

    fn capture_test_frames<T>(input: &[T]) -> Vec<f32>
    where
        T: Sample,
        f32: FromSample<T>,
    {
        let (mut producer, mut consumer) = HeapRb::<f32>::new(input.len()).split();
        push_mono_frames(&mut producer, input, 1, &AtomicU64::new(0));
        consumer.pop_iter().collect()
    }

    fn sine_wave(sample_rate: u32, frequency: f32) -> Vec<f32> {
        (0..sample_rate)
            .map(|index| {
                (2.0 * std::f32::consts::PI * frequency * index as f32 / sample_rate as f32).sin()
            })
            .collect()
    }

    fn rms(samples: &[f32]) -> f32 {
        (samples.iter().map(|sample| sample * sample).sum::<f32>() / samples.len() as f32).sqrt()
    }
}
