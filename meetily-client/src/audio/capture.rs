use anyhow::{anyhow, Context, Result};
use cpal::traits::{DeviceTrait, HostTrait, StreamTrait};
use cpal::{Device, Sample, SampleFormat, Stream, StreamConfig};
use hound::{SampleFormat as WavSampleFormat, WavSpec, WavWriter};
use std::path::{Path, PathBuf};
use std::sync::mpsc::{sync_channel, Receiver, SyncSender, TrySendError};
use std::thread::{self, JoinHandle};
use std::time::{Duration, SystemTime, UNIX_EPOCH};
use tokio_util::sync::CancellationToken;

const TARGET_SAMPLE_RATE: u32 = 16_000;
const AUDIO_CHANNEL_CAPACITY: usize = 16_384;

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
                start_capture_stream(system_device.as_deref(), false, &system_path)
                    .context("failed to start system capture")?,
            )
        } else {
            write_empty_wav(&system_path)?;
            None
        };

        mic_stream
            .play()
            .context("failed to play microphone stream")?;
        if let Some(stream) = &system_stream {
            stream.play().context("failed to play system stream")?;
        }

        while !stop_signal.is_cancelled() {
            std::thread::sleep(Duration::from_millis(100));
        }

        if let Some(stream) = system_stream {
            stream.stop().context("failed to stop system capture")?;
        }
        mic_stream
            .stop()
            .context("failed to stop microphone capture")?;

        Ok((mic_path, system_path))
    })
    .await
    .context("recording task failed")?
}

struct ActiveCapture {
    stream: Option<Stream>,
    sender: Option<SyncSender<WriterMessage>>,
    writer_thread: Option<JoinHandle<Result<()>>>,
}

impl ActiveCapture {
    fn play(&self) -> Result<()> {
        self.stream
            .as_ref()
            .context("audio stream already stopped")?
            .play()
            .context("failed to play audio stream")
    }

    fn stop(mut self) -> Result<()> {
        self.stream.take();
        if let Some(sender) = self.sender.take() {
            let _ = sender.send(WriterMessage::Finish);
        }
        if let Some(writer_thread) = self.writer_thread.take() {
            writer_thread
                .join()
                .map_err(|_| anyhow!("audio writer thread panicked"))??;
        }
        Ok(())
    }
}

enum WriterMessage {
    Sample(f32),
    Finish,
}

fn start_capture_stream(
    device_name: Option<&str>,
    input: bool,
    path: &Path,
) -> Result<ActiveCapture> {
    let host = cpal::default_host();
    let device = select_device(&host, device_name, input)?;
    let supported_config = if input {
        device
            .default_input_config()
            .context("failed to get default input config")?
    } else {
        device
            .default_output_config()
            .context("failed to get default output config")?
    };
    let sample_format = supported_config.sample_format();
    let config: StreamConfig = supported_config.into();
    let input_rate = config.sample_rate.0;
    let (sender, receiver) = sync_channel(AUDIO_CHANNEL_CAPACITY);
    let writer_path = path.to_path_buf();
    let writer_thread = thread::spawn(move || write_wav_samples(receiver, writer_path, input_rate));

    let err_fn = |err| log::error!("audio stream error on capture stream: {err}");

    let stream = match sample_format {
        SampleFormat::F32 => build_stream::<f32>(&device, &config, sender.clone(), err_fn),
        SampleFormat::I16 => build_stream::<i16>(&device, &config, sender.clone(), err_fn),
        SampleFormat::U16 => build_stream::<u16>(&device, &config, sender.clone(), err_fn),
        other => Err(anyhow!("unsupported sample format: {other:?}")),
    }?;

    Ok(ActiveCapture {
        stream: Some(stream),
        sender: Some(sender),
        writer_thread: Some(writer_thread),
    })
}

fn build_stream<T>(
    device: &Device,
    config: &StreamConfig,
    sender: SyncSender<WriterMessage>,
    err_fn: impl FnMut(cpal::StreamError) + Send + 'static,
) -> Result<Stream>
where
    T: cpal::Sample + cpal::SizedSample,
    f32: cpal::FromSample<T>,
{
    let channels = config.channels.max(1) as usize;

    let stream = device.build_input_stream(
        config,
        move |data: &[T], _| {
            for frame in data.chunks(channels) {
                let mono = frame
                    .iter()
                    .map(|sample| f32::from_sample(*sample))
                    .sum::<f32>()
                    / channels as f32;

                match sender.try_send(WriterMessage::Sample(mono)) {
                    Ok(()) => {}
                    Err(TrySendError::Full(_)) => {}
                    Err(TrySendError::Disconnected(_)) => return,
                }
            }
        },
        err_fn,
        None,
    )?;

    Ok(stream)
}

fn write_wav_samples(
    receiver: Receiver<WriterMessage>,
    path: PathBuf,
    input_rate: u32,
) -> Result<()> {
    let mut writer = WavWriter::create(&path, wav_spec())
        .with_context(|| format!("failed to create WAV {}", path.display()))?;
    let mut downsampler = Downsampler::new(input_rate.max(1), TARGET_SAMPLE_RATE);
    let mut output = Vec::with_capacity(8);

    while let Ok(message) = receiver.recv() {
        match message {
            WriterMessage::Sample(sample) => {
                downsampler.push(sample, &mut output);
                for sample in output.iter().copied() {
                    writer
                        .write_sample(sample.clamp(-1.0, 1.0))
                        .with_context(|| {
                            format!("failed to write WAV sample to {}", path.display())
                        })?;
                }
            }
            WriterMessage::Finish => break,
        }
    }

    writer
        .finalize()
        .with_context(|| format!("failed to finalize WAV {}", path.display()))?;
    Ok(())
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

    if input {
        host.default_input_device()
            .ok_or_else(|| anyhow!("no default input device found"))
    } else {
        host.default_output_device()
            .ok_or_else(|| anyhow!("no default output device found"))
    }
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
    position: u64,
    mode: DownsampleMode,
}

enum DownsampleMode {
    Passthrough,
    Average {
        ratio: u32,
        sum: f32,
        count: u32,
    },
    Fir {
        taps: Vec<f32>,
        history: Vec<f32>,
        next_history: usize,
        filled: usize,
    },
}

impl Downsampler {
    fn new(input_rate: u32, output_rate: u32) -> Self {
        let mode = if input_rate <= output_rate {
            DownsampleMode::Passthrough
        } else if input_rate == output_rate * 3 {
            DownsampleMode::Average {
                ratio: 3,
                sum: 0.0,
                count: 0,
            }
        } else {
            let tap_count = 15;
            DownsampleMode::Fir {
                taps: vec![1.0 / tap_count as f32; tap_count],
                history: vec![0.0; tap_count],
                next_history: 0,
                filled: 0,
            }
        };

        Self {
            input_rate,
            output_rate,
            position: 0,
            mode,
        }
    }

    fn push(&mut self, sample: f32, out: &mut Vec<f32>) {
        out.clear();
        match &mut self.mode {
            DownsampleMode::Passthrough => out.push(sample),
            DownsampleMode::Average { ratio, sum, count } => {
                *sum += sample;
                *count += 1;
                if *count == *ratio {
                    out.push(*sum / *ratio as f32);
                    *sum = 0.0;
                    *count = 0;
                }
            }
            DownsampleMode::Fir {
                taps,
                history,
                next_history,
                filled,
            } => {
                history[*next_history] = sample;
                *next_history = (*next_history + 1) % history.len();
                *filled = (*filled + 1).min(history.len());

                if *filled < history.len() {
                    return;
                }

                let mut filtered = 0.0;
                for (tap_index, tap) in taps.iter().enumerate() {
                    let history_index =
                        (*next_history + history.len() - 1 - tap_index) % history.len();
                    filtered += history[history_index] * tap;
                }

                self.position += self.output_rate as u64;
                while self.position >= self.input_rate as u64 {
                    out.push(filtered);
                    self.position -= self.input_rate as u64;
                }
            }
        }
    }
}
