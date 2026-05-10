use anyhow::{anyhow, Context, Result};
use cpal::traits::{DeviceTrait, HostTrait, StreamTrait};
use cpal::{Device, Sample, SampleFormat, Stream, StreamConfig};
use hound::{SampleFormat as WavSampleFormat, WavSpec, WavWriter};
use std::path::{Path, PathBuf};
use std::sync::mpsc::{sync_channel, Receiver, SyncSender, TrySendError};
use std::sync::Arc;
use std::thread::{self, JoinHandle};
use std::time::{Duration, SystemTime, UNIX_EPOCH};
use tokio::sync::mpsc;
use tokio_util::sync::CancellationToken;

use meetily_audio::resample::Resampler16k;
use meetily_audio::vad::{Utterance, Vad};

const TARGET_SAMPLE_RATE: u32 = 16_000;
const AUDIO_CHANNEL_CAPACITY: usize = 16_384;
/// Tokio mpsc capacity for streaming utterances (each is small metadata + ~few sec of f32).
const STREAM_UTTERANCE_CAPACITY: usize = 64;

/// Audio source tag for a streaming utterance.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum StreamSource {
    Mic,
    System,
}

impl StreamSource {
    pub fn as_str(self) -> &'static str {
        match self {
            StreamSource::Mic => "mic",
            StreamSource::System => "system",
        }
    }
}

/// A speech utterance captured from one audio source, ready for transcription.
#[derive(Debug, Clone)]
pub struct StreamingChunk {
    pub source: StreamSource,
    pub utterance: Utterance,
}

/// Handle returned by `record_streaming` — drop or call `stop()` to clean up.
pub struct StreamingHandle {
    mic_capture: Option<ActiveCapture>,
    system_capture: Option<ActiveCapture>,
    mic_pump: Option<thread::JoinHandle<Result<()>>>,
    system_pump: Option<thread::JoinHandle<Result<()>>>,
}

impl StreamingHandle {
    /// Stop cpal streams and signal pump threads to finish their pending raw
    /// audio buffer. Does NOT join pump threads — call `await_completion()`
    /// after the receiver has been fully drained, otherwise the pumps may
    /// deadlock on a full utterance channel.
    pub fn request_stop(&mut self) -> Result<()> {
        if let Some(cap) = self.mic_capture.take() {
            cap.stop()?;
        }
        if let Some(cap) = self.system_capture.take() {
            cap.stop()?;
        }
        Ok(())
    }

    /// Join pump threads. Must only be called once the utterance receiver is
    /// being drained (or has been dropped) so blocking_send calls inside the
    /// pumps can complete.
    pub fn await_completion(mut self) -> Result<()> {
        if let Some(t) = self.mic_pump.take() {
            t.join()
                .map_err(|_| anyhow!("mic pump thread panicked"))??;
        }
        if let Some(t) = self.system_pump.take() {
            t.join()
                .map_err(|_| anyhow!("system pump thread panicked"))??;
        }
        Ok(())
    }

    /// Convenience: request_stop + immediately await_completion. Only safe if
    /// you are certain the utterance receiver is being polled concurrently.
    pub fn stop(mut self) -> Result<()> {
        self.request_stop()?;
        self.await_completion()
    }
}

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
    raw_sender: Option<SyncSender<RawAudioMessage>>,
    /// CoreAudio backend uses a forwarding thread (cidre Stream -> sync mpsc).
    /// `play()` is a no-op for CoreAudio (the device is already streaming);
    /// `stop()` flips this flag and joins the thread.
    core_audio_stop: Option<Arc<std::sync::atomic::AtomicBool>>,
    core_audio_thread: Option<JoinHandle<Result<()>>>,
}

impl ActiveCapture {
    fn play(&self) -> Result<()> {
        if let Some(stream) = self.stream.as_ref() {
            stream.play().context("failed to play audio stream")?;
        }
        // CoreAudio devices are already running by the time the forwarding
        // thread is spawned -- nothing to play().
        Ok(())
    }

    fn stop(mut self) -> Result<()> {
        // Drop the cpal stream first so no more samples are pushed.
        self.stream.take();
        // Signal the CoreAudio forwarder to exit if any.
        if let Some(stop_flag) = self.core_audio_stop.take() {
            stop_flag.store(true, std::sync::atomic::Ordering::Release);
        }
        if let Some(sender) = self.sender.take() {
            // Best-effort Finish; falling out of scope drops the sender too.
            let _ = sender.send(WriterMessage::Finish);
        }
        if let Some(sender) = self.raw_sender.take() {
            // For streaming pumps we don't try to send Finish (raw channel is
            // bounded; main task may not have started draining utterances
            // yet). Dropping the sender closes the channel so the pump's
            // recv() returns Err and it exits its loop. The Finish variant
            // is only used by the legacy WAV writer path which uses an
            // unbounded send pattern.
            drop(sender);
        }
        if let Some(writer_thread) = self.writer_thread.take() {
            writer_thread
                .join()
                .map_err(|_| anyhow!("audio writer thread panicked"))??;
        }
        if let Some(t) = self.core_audio_thread.take() {
            t.join()
                .map_err(|_| anyhow!("CoreAudio forwarder thread panicked"))??;
        }
        Ok(())
    }
}

enum WriterMessage {
    Sample(f32),
    Finish,
}

/// Raw audio frames produced by a streaming capture, before resampling.
/// Channel close (sender dropped) signals end of stream — no explicit
/// Finish variant is needed because the raw channel is bounded and we
/// don't want the producer to ever block trying to enqueue a sentinel.
struct RawAudioMessage(Vec<f32>);

/// Selects the system-audio capture backend.
///
/// `Cpal` is the cross-platform default — uses cpal default-output loopback,
/// which on macOS requires a virtual audio device (BlackHole or Multi-Output
/// Device).
///
/// `CoreAudio` (macOS only) uses Apple's native Core Audio Tap (macOS 14.2+),
/// avoiding the need for any third-party audio driver. Requires
/// `NSAudioCaptureUsageDescription` in the bundle's Info.plist for permission
/// prompting. The `--system <device>` argument is ignored — Core Audio Tap
/// captures the global default-output mix directly.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
#[non_exhaustive]
pub enum SystemBackend {
    #[default]
    Cpal,
    CoreAudio,
}

/// Start a streaming dual-source capture: mic (always) and optionally system.
/// Each captured speech utterance is pushed to the returned mpsc receiver.
///
/// The handle must be stopped when the user is done — drop is not enough
/// because cpal streams hold OS resources and writer threads must drain.
/// `system_backend` selects the system-audio capture mechanism (see
/// [`SystemBackend`]). `enable_aec` enables WebRTC AEC3 between the
/// resampler and VAD on the mic path; system audio is tee'd into the AEC
/// as the far-end reference. Requires the `aec` Cargo feature; ignored
/// otherwise.
pub fn record_streaming(
    mic_device: Option<String>,
    system_device: Option<String>,
    system_backend: SystemBackend,
    enable_aec: bool,
) -> Result<(StreamingHandle, mpsc::Receiver<StreamingChunk>)> {
    let (utt_tx, utt_rx) = mpsc::channel::<StreamingChunk>(STREAM_UTTERANCE_CAPACITY);

    let (mic_capture, mic_raw_rx, mic_rate, mic_channels) =
        start_streaming_capture(mic_device.as_deref(), true)
            .context("failed to start streaming mic capture")?;
    mic_capture.play().context("failed to play mic stream")?;

    // Build the AEC roles. With AEC enabled and a system source available,
    // we set up a render-tee channel. Without either, both pumps run with
    // AecRole::None.
    let want_system = !matches!((system_device.as_deref(), system_backend), (None, SystemBackend::Cpal));
    #[cfg(feature = "aec")]
    let (mic_role, system_role) = if enable_aec && want_system {
        // Bounded render tee. Capacity sized to ~2 s of 30 ms frames at
        // 16 kHz mono = 64 frames. Per the sonora-aec3 wrapper rustdoc,
        // overflow → drop the *newest* render frame (sync_channel::try_send
        // semantics) + bump AecMetrics::render_drops; sonora's
        // RenderDelayController re-estimates on the next aligned audio.
        let (render_tx, render_rx) = sync_channel::<Vec<f32>>(64);
        let render_drops = Arc::new(std::sync::atomic::AtomicU64::new(0));
        let aec = meetily_audio::aec::AecPipeline::new(16_000)
            .context("failed to initialize AecPipeline")?;
        (
            AecRole::Mic { aec, render_rx, render_drops: render_drops.clone() },
            AecRole::System { render_tx, render_drops },
        )
    } else {
        (AecRole::None, AecRole::None)
    };
    #[cfg(not(feature = "aec"))]
    let (mic_role, system_role) = {
        let _ = enable_aec;
        let _ = want_system;
        (AecRole::None, AecRole::None)
    };

    let mic_pump = spawn_pump_thread(
        StreamSource::Mic,
        mic_raw_rx,
        mic_rate,
        mic_channels,
        utt_tx.clone(),
        mic_role,
    )?;

    let (system_capture, system_pump) = match (system_device.as_deref(), system_backend) {
        (None, SystemBackend::Cpal) => {
            // No system audio requested. Drop the cloned sender to allow rx
            // to close once mic pump exits.
            drop(utt_tx);
            (None, None)
        }
        (_, SystemBackend::Cpal) => {
            let (cap, raw_rx, rate, channels) =
                start_streaming_capture(system_device.as_deref(), false)
                    .context("failed to start streaming system capture")?;
            cap.play().context("failed to play system stream")?;
            let pump = spawn_pump_thread(
                StreamSource::System,
                raw_rx,
                rate,
                channels,
                utt_tx,
                system_role,
            )?;
            (Some(cap), Some(pump))
        }
        (_, SystemBackend::CoreAudio) => {
            let (cap, raw_rx, rate, channels) = start_streaming_capture_coreaudio()
                .context("failed to start CoreAudio system capture")?;
            cap.play().context("failed to play CoreAudio system stream")?;
            let pump = spawn_pump_thread(
                StreamSource::System,
                raw_rx,
                rate,
                channels,
                utt_tx,
                system_role,
            )?;
            (Some(cap), Some(pump))
        }
    };

    Ok((
        StreamingHandle {
            mic_capture: Some(mic_capture),
            system_capture,
            mic_pump: Some(mic_pump),
            system_pump,
        },
        utt_rx,
    ))
}

/// Start a CoreAudio Tap (macOS 14.2+) system capture and forward its samples
/// into a `RawAudioMessage` channel matching the cpal-streaming-capture shape.
///
/// The `coreaudio` Cargo feature on `meetily-audio` must be enabled (it is by
/// default for macOS builds of this crate via the `coreaudio` feature on
/// `meetily-client`). Returns an error on non-macOS platforms.
#[cfg(all(target_os = "macos", feature = "coreaudio"))]
fn start_streaming_capture_coreaudio()
    -> Result<(ActiveCapture, Receiver<RawAudioMessage>, u32, u16)> {
    use futures_util::StreamExt;

    let capture = meetily_audio::capture::core_audio::CoreAudioCapture::new()
        .context("failed to initialize CoreAudio capture")?;
    let mut stream = capture
        .stream()
        .context("failed to start CoreAudio stream")?;

    let input_rate = stream.sample_rate().max(1);
    // CoreAudioCapture uses a mono global tap — single channel of f32.
    let input_channels: u16 = 1;

    let (raw_tx, raw_rx) = sync_channel::<RawAudioMessage>(AUDIO_CHANNEL_CAPACITY);
    let stop_flag = Arc::new(std::sync::atomic::AtomicBool::new(false));
    let stop_flag_thread = stop_flag.clone();

    // The CoreAudio Stream is async (futures::Stream). Run it on a small
    // single-thread tokio runtime inside a dedicated OS thread so we can
    // forward batched samples synchronously into the bounded sync_channel
    // that the existing pump thread already drains.
    let raw_tx_for_thread = raw_tx.clone();
    let thread = thread::Builder::new()
        .name("meetily-coreaudio-fwd".to_string())
        .spawn(move || -> Result<()> {
            let rt = tokio::runtime::Builder::new_current_thread()
                .enable_all()
                .build()
                .context("failed to build CoreAudio forwarder runtime")?;

            // Batch single-sample stream output into ~10ms chunks (480 @ 48k)
            // to amortize per-message overhead before handing to the pump.
            // Note: AUDIO_CHANNEL_CAPACITY counts MESSAGES, not samples — at
            // 480 samples/batch and ~100 batches/sec the channel can buffer
            // ~16k batches ≈ 160s of audio before drop, far exceeding any
            // realistic pump-side stall. Drop-on-Full is therefore a hard
            // safety net, not a routine event.
            const BATCH: usize = 480;
            let mut buf: Vec<f32> = Vec::with_capacity(BATCH);

            rt.block_on(async move {
                while !stop_flag_thread.load(std::sync::atomic::Ordering::Acquire) {
                    match tokio::time::timeout(
                        std::time::Duration::from_millis(50),
                        stream.next(),
                    )
                    .await
                    {
                        Ok(Some(sample)) => {
                            buf.push(sample);
                            if buf.len() >= BATCH {
                                let chunk = std::mem::replace(
                                    &mut buf,
                                    Vec::with_capacity(BATCH),
                                );
                                if let Err(e) = raw_tx_for_thread
                                    .try_send(RawAudioMessage(chunk))
                                {
                                    match e {
                                        TrySendError::Full(_) => {
                                            log::warn!("CoreAudio fwd: pump backpressure, dropping batch");
                                        }
                                        TrySendError::Disconnected(_) => {
                                            log::debug!("CoreAudio fwd: pump dropped, exiting");
                                            return Ok::<(), anyhow::Error>(());
                                        }
                                    }
                                }
                            }
                        }
                        Ok(None) => {
                            log::debug!("CoreAudio stream ended");
                            break;
                        }
                        Err(_) => continue, // timeout: re-check stop flag
                    }
                }
                // Flush trailing partial batch on shutdown.
                if !buf.is_empty() {
                    let _ = raw_tx_for_thread.try_send(RawAudioMessage(buf));
                }
                Ok::<(), anyhow::Error>(())
            })?;
            Ok(())
        })
        .context("failed to spawn CoreAudio forwarder thread")?;

    Ok((
        ActiveCapture {
            stream: None,
            sender: None,
            writer_thread: None,
            raw_sender: Some(raw_tx),
            core_audio_stop: Some(stop_flag),
            core_audio_thread: Some(thread),
        },
        raw_rx,
        input_rate,
        input_channels,
    ))
}

#[cfg(not(all(target_os = "macos", feature = "coreaudio")))]
fn start_streaming_capture_coreaudio()
    -> Result<(ActiveCapture, Receiver<RawAudioMessage>, u32, u16)> {
    Err(anyhow!(
        "CoreAudio backend is only available on macOS with the `coreaudio` feature enabled"
    ))
}

/// Per-pump AEC wiring.
///
/// `Mic` owns the [`AecPipeline`] and consumes render-side resampled chunks
/// produced by the system pump. `System` is the source side: after
/// resampling each 30 ms frame is also pushed to `render_tx` so the mic
/// pump can `ingest_render` them. On render-tee overflow the system pump
/// bumps `render_drops` and drops the incoming (newest) write — see
/// [`AecPipeline`] rustdoc §"Drop semantics" for the design rationale.
#[cfg(feature = "aec")]
enum AecRole {
    None,
    Mic {
        aec: meetily_audio::aec::AecPipeline,
        render_rx: Receiver<Vec<f32>>,
        render_drops: Arc<std::sync::atomic::AtomicU64>,
    },
    System {
        render_tx: SyncSender<Vec<f32>>,
        render_drops: Arc<std::sync::atomic::AtomicU64>,
    },
}

#[cfg(not(feature = "aec"))]
enum AecRole {
    None,
}

fn spawn_pump_thread(
    source: StreamSource,
    raw_rx: Receiver<RawAudioMessage>,
    input_rate: u32,
    input_channels: u16,
    utt_tx: mpsc::Sender<StreamingChunk>,
    aec_role: AecRole,
) -> Result<thread::JoinHandle<Result<()>>> {
    let mut resampler = Resampler16k::new(input_rate, input_channels)
        .with_context(|| format!("failed to init resampler for {}", source.as_str()))?;
    let mut vad = Vad::new()
        .with_context(|| format!("failed to init VAD for {}", source.as_str()))?;

    // Post-AEC accumulator: AEC operates in 4 ms / 64-sample blocks while
    // VAD wants exactly VAD_FRAME_SAMPLES (480 = 30 ms) per call. AEC
    // output length per call is not a multiple of 480, so we buffer here
    // and drain in 480-sample windows. Without this accumulator, ~50% of
    // mic audio is lost on the AEC path (codex round 1 blocker fix).
    let mut vad_acc: Vec<f32> = Vec::with_capacity(meetily_audio::vad::VAD_FRAME_SAMPLES * 4);

    let handle = thread::Builder::new()
        .name(format!("meetily-pump-{}", source.as_str()))
        .spawn(move || -> Result<()> {
            #[cfg(feature = "aec")]
            let mut aec_role = aec_role;
            #[cfg(not(feature = "aec"))]
            let _ = aec_role;

            // Drain raw audio until the producer (cpal stream) is dropped.
            while let Ok(RawAudioMessage(samples)) = raw_rx.recv() {
                let frames = resampler.push(&samples).with_context(|| {
                    format!("resampler failed for {}", source.as_str())
                })?;
                for frame in frames {
                    // AEC wiring (feature-gated):
                    //  - Mic: drain any pending render-tee chunks into the
                    //    AecPipeline, propagate any system-side drops to the
                    //    AEC's metric counter, then run the mic frame through
                    //    AEC before VAD.
                    //  - System: tee the resampled frame to the mic pump.
                    //  - None: passthrough.
                    let processed_frame: Vec<f32>;
                    #[cfg(feature = "aec")]
                    {
                        match &mut aec_role {
                            AecRole::Mic { aec, render_rx, render_drops } => {
                                while let Ok(render_chunk) = render_rx.try_recv() {
                                    aec.ingest_render(&render_chunk);
                                }
                                let drops = render_drops.swap(
                                    0,
                                    std::sync::atomic::Ordering::Relaxed,
                                );
                                for _ in 0..drops {
                                    aec.record_render_drop();
                                }
                                let out = aec.process_capture(&frame);
                                // AEC emits 4 ms blocks; the VAD frame size
                                // is 30 ms (480 samples). The aec output for
                                // a 480-sample input is 480 samples (since
                                // 480 = 7×64 + 32, so the AEC may withhold
                                // up to 63 samples — we still pass whatever
                                // is ready; VAD's internal buffering already
                                // tolerates frame-size mismatches). If `out`
                                // is empty (warm-up), skip VAD this round.
                                if out.is_empty() {
                                    continue;
                                }
                                processed_frame = out;
                            }
                            AecRole::System { render_tx, render_drops } => {
                                if let Err(e) = render_tx.try_send(frame.clone()) {
                                    if matches!(e, TrySendError::Full(_)) {
                                        render_drops.fetch_add(
                                            1,
                                            std::sync::atomic::Ordering::Relaxed,
                                        );
                                    }
                                    // Disconnected = mic pump exited; we
                                    // continue running our own VAD/Whisper path
                                    // until the cpal channel closes.
                                }
                                processed_frame = frame;
                            }
                            AecRole::None => {
                                processed_frame = frame;
                            }
                        }
                    }
                    #[cfg(not(feature = "aec"))]
                    {
                        processed_frame = frame;
                    }

                    // Some VADs don't accept arbitrary block sizes; meetily's
                    // wrapper expects exactly VAD_FRAME_SAMPLES per call. The
                    // 30 ms-aligned frames from Resampler16k always satisfy
                    // that on the passthrough / system path. On the mic+AEC
                    // path AEC output length per call is not a multiple of
                    // 480, so we accumulate and drain.
                    vad_acc.extend_from_slice(&processed_frame);
                    while vad_acc.len() >= meetily_audio::vad::VAD_FRAME_SAMPLES {
                        let vad_frame: Vec<f32> = vad_acc
                            .drain(..meetily_audio::vad::VAD_FRAME_SAMPLES)
                            .collect();
                        let utts = vad
                            .process_frame(&vad_frame)
                            .with_context(|| format!("vad failed for {}", source.as_str()))?;
                        for utterance in utts {
                            let chunk = StreamingChunk { source, utterance };
                            if utt_tx.blocking_send(chunk).is_err() {
                                log::debug!(
                                    "{} pump: receiver dropped, exiting",
                                    source.as_str()
                                );
                                return Ok(());
                            }
                        }
                    }
                }
            }

            // Stream ended -- pad any remaining vad_acc residue with zeros
            // up to a full VAD frame so we don't lose the last few hundred
            // samples on shutdown, then run the standard 1 s silence flush
            // so the VAD emits SpeechEnd for any in-progress utterance.
            if !vad_acc.is_empty() {
                vad_acc.resize(meetily_audio::vad::VAD_FRAME_SAMPLES, 0.0);
                let utts = vad
                    .process_frame(&vad_acc)
                    .with_context(|| format!("vad final partial flush failed for {}", source.as_str()))?;
                for utterance in utts {
                    let chunk = StreamingChunk { source, utterance };
                    if utt_tx.blocking_send(chunk).is_err() {
                        return Ok(());
                    }
                }
                vad_acc.clear();
            }
            let silence_frame = vec![0.0f32; meetily_audio::vad::VAD_FRAME_SAMPLES];
            let flush_frames = (meetily_audio::vad::VAD_SAMPLE_RATE / meetily_audio::vad::VAD_FRAME_SAMPLES) + 1;
            for _ in 0..flush_frames {
                let utts = vad
                    .process_frame(&silence_frame)
                    .with_context(|| format!("vad flush failed for {}", source.as_str()))?;
                for utterance in utts {
                    let chunk = StreamingChunk { source, utterance };
                    if utt_tx.blocking_send(chunk).is_err() {
                        return Ok(());
                    }
                }
            }
            Ok(())
        })
        .context("failed to spawn pump thread")?;
    Ok(handle)
}


fn start_streaming_capture(
    device_name: Option<&str>,
    input: bool,
) -> Result<(ActiveCapture, Receiver<RawAudioMessage>, u32, u16)> {
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
    let input_channels = config.channels.max(1);

    let (raw_tx, raw_rx) = sync_channel::<RawAudioMessage>(AUDIO_CHANNEL_CAPACITY);
    let err_fn = |err| log::error!("audio stream error on streaming capture: {err}");

    let stream = match sample_format {
        SampleFormat::F32 => build_streaming_stream::<f32>(&device, &config, raw_tx.clone(), err_fn),
        SampleFormat::I16 => build_streaming_stream::<i16>(&device, &config, raw_tx.clone(), err_fn),
        SampleFormat::U16 => build_streaming_stream::<u16>(&device, &config, raw_tx.clone(), err_fn),
        other => Err(anyhow!("unsupported sample format: {other:?}")),
    }?;

    Ok((
        ActiveCapture {
            stream: Some(stream),
            sender: None,
            writer_thread: None,
            raw_sender: Some(raw_tx),
            core_audio_stop: None,
            core_audio_thread: None,
        },
        raw_rx,
        input_rate,
        input_channels,
    ))
}

fn build_streaming_stream<T>(
    device: &Device,
    config: &StreamConfig,
    sender: SyncSender<RawAudioMessage>,
    err_fn: impl FnMut(cpal::StreamError) + Send + 'static,
) -> Result<Stream>
where
    T: cpal::Sample + cpal::SizedSample,
    f32: cpal::FromSample<T>,
{
    let stream = device.build_input_stream(
        config,
        move |data: &[T], _| {
            let mut floats = Vec::with_capacity(data.len());
            for sample in data.iter() {
                floats.push(f32::from_sample(*sample));
            }
            match sender.try_send(RawAudioMessage(floats)) {
                Ok(()) => {}
                Err(TrySendError::Full(_)) => {
                    log::warn!("streaming raw channel full, dropping {} samples", data.len());
                }
                Err(TrySendError::Disconnected(_)) => {}
            }
        },
        err_fn,
        None,
    )?;
    Ok(stream)
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
        raw_sender: None,
        core_audio_stop: None,
        core_audio_thread: None,
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
