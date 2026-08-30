//! Microphone capture and WAV utilities for the Phase 0 benchmark.

use std::path::Path;
use std::sync::{Arc, Mutex};
use std::thread;
use std::time::Duration;

use cpal::traits::{DeviceTrait, HostTrait, StreamTrait};
use cpal::{SampleFormat, StreamConfig};
use phorminx_core::AudioClip;

pub const WHISPER_SAMPLE_RATE: u32 = 16_000;

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

/// Records from the default microphone for a fixed duration.
///
/// This intentionally uses a mutex-backed buffer for the feasibility spike.
/// The interactive product will replace it with a bounded SPSC ring buffer.
pub fn record_default(duration: Duration) -> Result<AudioClip, CaptureError> {
    let host = cpal::default_host();
    let device = host
        .default_input_device()
        .ok_or(CaptureError::NoDefaultInputDevice)?;
    let supported = device
        .default_input_config()
        .map_err(CaptureError::DefaultConfig)?;
    let sample_format = supported.sample_format();
    let config: StreamConfig = supported.into();
    let channels = usize::from(config.channels);
    let sample_rate = config.sample_rate;
    let captured = Arc::new(Mutex::new(Vec::<f32>::new()));
    let errors = Arc::new(Mutex::new(Vec::<String>::new()));

    let stream = match sample_format {
        SampleFormat::F32 => build_stream::<f32, _>(
            &device,
            &config,
            channels,
            Arc::clone(&captured),
            Arc::clone(&errors),
            |sample| sample,
        ),
        SampleFormat::I16 => build_stream::<i16, _>(
            &device,
            &config,
            channels,
            Arc::clone(&captured),
            Arc::clone(&errors),
            |sample| sample as f32 / i16::MAX as f32,
        ),
        SampleFormat::U16 => build_stream::<u16, _>(
            &device,
            &config,
            channels,
            Arc::clone(&captured),
            Arc::clone(&errors),
            |sample| (sample as f32 / u16::MAX as f32) * 2.0 - 1.0,
        ),
        other => return Err(CaptureError::UnsupportedSampleFormat(other)),
    }?;

    stream.play().map_err(CaptureError::PlayStream)?;
    thread::sleep(duration);
    drop(stream);

    if let Some(error) = errors.lock().expect("audio error mutex poisoned").first() {
        return Err(CaptureError::Stream(error.clone()));
    }

    let native_samples = Arc::try_unwrap(captured)
        .map_err(|_| CaptureError::BufferStillShared)?
        .into_inner()
        .map_err(|_| CaptureError::BufferPoisoned)?;

    let samples = resample_linear(&native_samples, sample_rate, WHISPER_SAMPLE_RATE);
    AudioClip::new(samples, WHISPER_SAMPLE_RATE).map_err(CaptureError::Audio)
}

fn build_stream<T, F>(
    device: &cpal::Device,
    config: &StreamConfig,
    channels: usize,
    captured: Arc<Mutex<Vec<f32>>>,
    errors: Arc<Mutex<Vec<String>>>,
    convert: F,
) -> Result<cpal::Stream, CaptureError>
where
    T: cpal::SizedSample,
    F: Fn(T) -> f32 + Send + Copy + 'static,
{
    device
        .build_input_stream(
            *config,
            move |data: &[T], _| {
                let mut output = captured.lock().expect("audio buffer mutex poisoned");
                output.extend(data.chunks(channels).map(|frame| {
                    frame.iter().copied().map(convert).sum::<f32>() / channels as f32
                }));
            },
            move |error| {
                errors
                    .lock()
                    .expect("audio error mutex poisoned")
                    .push(error.to_string());
            },
            None,
        )
        .map_err(CaptureError::BuildStream)
}

/// Simple Phase 0 resampler. Replace with a band-limited implementation before
/// using benchmark results as an accuracy release gate.
pub fn resample_linear(input: &[f32], source_rate: u32, target_rate: u32) -> Vec<f32> {
    if input.is_empty() || source_rate == 0 || target_rate == 0 {
        return Vec::new();
    }

    if source_rate == target_rate {
        return input.to_vec();
    }

    let output_len = ((input.len() as u64 * target_rate as u64) / source_rate as u64) as usize;
    let source_per_output = source_rate as f64 / target_rate as f64;

    (0..output_len)
        .map(|index| {
            let source_position = index as f64 * source_per_output;
            let left = source_position.floor() as usize;
            let right = (left + 1).min(input.len() - 1);
            let fraction = (source_position - left as f64) as f32;
            input[left] * (1.0 - fraction) + input[right] * fraction
        })
        .collect()
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
    let samples = resample_linear(&mono, spec.sample_rate, WHISPER_SAMPLE_RATE);
    AudioClip::new(samples, WHISPER_SAMPLE_RATE).map_err(CaptureError::Audio)
}

#[derive(Debug, thiserror::Error)]
pub enum CaptureError {
    #[error("no default microphone is configured")]
    NoDefaultInputDevice,
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
    #[error("microphone stream failed: {0}")]
    Stream(String),
    #[error("audio buffer was still shared after the stream stopped")]
    BufferStillShared,
    #[error("audio buffer lock was poisoned")]
    BufferPoisoned,
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
    fn resamples_known_ramp() {
        let output = resample_linear(&[0.0, 1.0, 0.0, -1.0], 4, 8);

        assert_eq!(output.len(), 8);
        assert_eq!(output[0], 0.0);
        assert_eq!(output[2], 1.0);
    }
}
