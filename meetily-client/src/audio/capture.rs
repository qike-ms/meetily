use anyhow::{anyhow, Context, Result};
use cpal::traits::{DeviceTrait, HostTrait, StreamTrait};
use cpal::{Device, Sample, SampleFormat, Stream, StreamConfig};
use hound::{SampleFormat as WavSampleFormat, WavSpec, WavWriter};
use std::fs::File;
use std::io::BufWriter;
use std::path::{Path, PathBuf};
use std::sync::{Arc, Mutex};
use std::time::{Duration, SystemTime, UNIX_EPOCH};
use tokio_util::sync::CancellationToken;

const TARGET_SAMPLE_RATE: u32 = 16_000;

pub async fn record_dual_stream(
    mic_device: Option<String>,
    system_device: Option<String>,
    stop_signal: CancellationToken,
) -> Result<(PathBuf, PathBuf)> {
    tokio::task::spawn_blocking(move || {
        let mic_path = temp_wav_path("mic");
        let system_path = temp_wav_path("system");

        let mic_stream = start_capture_stream(mic_device.as_deref(), true, &mic_path)
            .context("failed to start microphone capture")?;
        let system_stream = if system_device.is_some() {
            Some(
                start_capture_stream(system_device.as_deref(), true, &system_path)
                    .context("failed to start system capture")?,
            )
        } else {
            write_empty_wav(&system_path)?;
            None
        };

        mic_stream.play().context("failed to play microphone stream")?;
        if let Some(stream) = &system_stream {
            stream.play().context("failed to play system stream")?;
        }

        while !stop_signal.is_cancelled() {
            std::thread::sleep(Duration::from_millis(100));
        }

        drop(system_stream);
        drop(mic_stream);

        Ok((mic_path, system_path))
    })
    .await
    .context("recording task failed")?
}

fn start_capture_stream(device_name: Option<&str>, input: bool, path: &Path) -> Result<Stream> {
    let host = cpal::default_host();
    let device = select_device(&host, device_name, input)?;
    let supported_config = device
        .default_input_config()
        .context("failed to get default input config")?;
    let sample_format = supported_config.sample_format();
    let config: StreamConfig = supported_config.into();

    let writer = Arc::new(Mutex::new(Some(WavWriter::create(path, wav_spec())?)));
    let err_fn = |err| log::error!("audio stream error: {err}");

    match sample_format {
        SampleFormat::F32 => build_stream::<f32>(&device, &config, writer, err_fn),
        SampleFormat::I16 => build_stream::<i16>(&device, &config, writer, err_fn),
        SampleFormat::U16 => build_stream::<u16>(&device, &config, writer, err_fn),
        other => Err(anyhow!("unsupported sample format: {other:?}")),
    }
}

fn build_stream<T>(
    device: &Device,
    config: &StreamConfig,
    writer: Arc<Mutex<Option<WavWriter<BufWriter<File>>>>>,
    err_fn: impl FnMut(cpal::StreamError) + Send + 'static,
) -> Result<Stream>
where
    T: cpal::Sample + cpal::SizedSample,
    f32: cpal::FromSample<T>,
{
    let channels = config.channels.max(1) as usize;
    let input_rate = config.sample_rate.0.max(1);
    let mut resampler = Downsampler::new(input_rate, TARGET_SAMPLE_RATE);

    let stream = device.build_input_stream(
        config,
        move |data: &[T], _| {
            if let Ok(mut guard) = writer.lock() {
                let Some(writer) = guard.as_mut() else {
                    return;
                };

                for frame in data.chunks(channels) {
                    let mono = frame
                        .iter()
                        .map(|sample| f32::from_sample(*sample))
                        .sum::<f32>()
                        / channels as f32;

                    for sample in resampler.push(mono) {
                        if let Err(err) = writer.write_sample(sample.clamp(-1.0, 1.0)) {
                            log::error!("failed to write WAV sample: {err}");
                            break;
                        }
                    }
                }
            }
        },
        err_fn,
        None,
    )?;

    Ok(stream)
}

fn select_device(host: &cpal::Host, device_name: Option<&str>, input: bool) -> Result<Device> {
    if let Some(name) = device_name {
        let devices = if input {
            host.input_devices()?
        } else {
            host.output_devices()?
        };

        for device in devices {
            if device.name().ok().as_deref() == Some(name) {
                return Ok(device);
            }
        }

        return Err(anyhow!("audio device not found: {name}"));
    }

    host.default_input_device()
        .ok_or_else(|| anyhow!("no default input device found"))
}

fn temp_wav_path(source: &str) -> PathBuf {
    let millis = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .unwrap_or_default()
        .as_millis();
    std::env::temp_dir().join(format!("meetily-{source}-{millis}.wav"))
}

fn wav_spec() -> WavSpec {
    WavSpec {
        channels: 1,
        sample_rate: TARGET_SAMPLE_RATE,
        bits_per_sample: 32,
        sample_format: WavSampleFormat::Float,
    }
}

fn write_empty_wav(path: &Path) -> Result<()> {
    WavWriter::create(path, wav_spec())?.finalize()?;
    Ok(())
}

struct Downsampler {
    input_rate: u32,
    output_rate: u32,
    position: u32,
}

impl Downsampler {
    fn new(input_rate: u32, output_rate: u32) -> Self {
        Self {
            input_rate,
            output_rate,
            position: 0,
        }
    }

    fn push(&mut self, sample: f32) -> Vec<f32> {
        if self.input_rate == self.output_rate {
            return vec![sample];
        }

        self.position += self.output_rate;
        let mut out = Vec::new();
        while self.position >= self.input_rate {
            out.push(sample);
            self.position -= self.input_rate;
        }
        out
    }
}
