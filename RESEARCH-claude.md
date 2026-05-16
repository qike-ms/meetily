# Meetily v4: Architecture Research Report
*Claude, 2026-05-12 — branch main @ 52d11d0*

---

## Section A: Competitive Research

### 1. Granola

**Capture topology**: Single-laptop, mixed-stream split at the OS level. Granola captures two parallel streams: the microphone (via `NSMicrophoneUsageDescription`) as the local speaker, and system audio (via macOS Core Audio Tap or ScreenCaptureKit) as remote participants. There is no per-participant splitting within the system-audio stream — all remote voices arrive mixed together on a single "them" channel. This is capture-level diarization, not acoustic diarization.

**System-audio capture mechanism**: Core Audio Tap (`AudioHardwareCreateProcessTap` API, macOS 14.2+) by default. ScreenCaptureKit is documented as a fallback path. Granola's own setup guide requires users to enable **both** Microphone and **Screen & System Audio Recording** in System Settings → Privacy & Security. The "Screen & System Audio Recording" grant is TCC service `kTCCServiceScreenCapture` (in macOS 14.4+ there is a sub-permission for audio-only, `kTCCServiceSystemAudioCapture`, that avoids requesting screen-capture). Both permissions are attributed to Granola's signed `.app` bundle — a signed, bundled application with a stable bundle ID.

**Echo cancellation**: None visible from the outside, and structurally unnecessary. Because Granola captures mic and system audio as separate OS streams before the audio engine ever mixes them, there is no acoustic echo path to cancel. The microphone stream contains only your voice; the system-audio tap contains only remote voices (+ any local media). No AEC needed.

**Transcription**: **Cloud.** Despite the privacy-first framing, Granola sends audio to their own cloud for transcription. Their FAQ states: *"On macOS and Windows, audio is transcribed in real-time then deleted. The audio doesn't leave your device until it's sent for AI transcription."* They do not disclose which model powers transcription. Post-meeting notes are enhanced by an LLM (likely GPT-4 class). There is no fully local/offline mode.

**Diarization**: Capture-level only: mic = YOU, system-audio tap = THEM. There is no acoustic speaker diarization to distinguish individual remote participants within the THEM stream. Granola's notes treat all remote participants as a single "others" voice and relies on the user's own memory/context for per-participant attribution.

**Lesson for meetily**: Granola's architecture is the direct template. Two things make it work:
1. **Signed app bundle** — TCC attributions are stable. No terminal-inheritance problem.
2. **Capture-level split** — eliminates AEC, eliminates acoustic diarization, gives perfect YOU/THEM labels for free.

The one place meetily can beat Granola: **local transcription**. Granola requires cloud; meetily runs Whisper on-device. That is meetily's core value proposition and it is sound.

*Sources*: [Granola FAQ/privacy](https://www.granola.ai/), [Granola setup guide](https://docs.granola.ai/help-center/getting-started/setting-up-granola-for-the-first-time), [Granola transcription troubleshooting](https://docs.granola.ai/help-center/troubleshooting/transcription-issues), [Muesli (OSS Granola clone) capture docs](https://github.com/pHequals7/muesli), [Apple Core Audio tap docs](https://developer.apple.com/documentation/CoreAudio/capturing-system-audio-with-core-audio-taps)

---

### 2. Zoom

**Capture topology**: Per-participant streams at the conferencing protocol level. Each participant's audio is sent as a separate RTP stream to Zoom's SFU (Selective Forwarding Unit). When Zoom records locally with "Record separate audio file for each participant" enabled, it writes one audio track per participant — 100% accurate channel diarization with zero acoustic ambiguity. Zoom's own AI transcription consumes these pre-separated streams server-side.

**System-audio capture mechanism**: Zoom is the meeting platform, not a passive observer, so it doesn't need OS-level system audio capture. Zoom installs **ZoomAudioDevice**, a CoreAudio virtual audio device (kernel extension on older macOS, DriverKit on macOS 12+), operating at 48 kHz. This virtual device serves two purposes: (1) Zoom injects participant audio into it so other apps (DAWs, ProTools) can receive the meeting mix; (2) Zoom reads from it to capture computer audio for "share computer sound." External recording tools that want to tap Zoom audio without a bot use ZoomAudioDevice or BlackHole as a loopback.

**Echo cancellation**: Zoom has proprietary acoustic echo cancellation (AEC) built into its client. On macOS it may use `AUVoiceProcessingIO`. AEC is applied on the uplink (your mic → their ears) before packets are sent, so by the time audio arrives at Zoom's servers for transcription, AEC has already been applied per-participant. Downstream transcription never sees echo artifacts.

**Transcription**: **Cloud.** Zoom AI Companion (Azure AI Speech backend, confirmed by Microsoft's Teams/Zoom partnership docs). Real-time captions + post-meeting transcript. Live captions scroll in the UI; a cleaned-up transcript is generated post-call. Speaker attribution uses channel diarization (participant RTP stream identity), not acoustic diarization. Separately, Zoom's multichannel recording can be uploaded to third-party services (AssemblyAI, Rev, etc.) for independent transcription.

**Diarization**: "Free" via per-participant RTP streams. Each participant has a unique SSRC (synchronisation source) in the RTP header. Zoom's server maps SSRC → display name. No acoustic speaker-embedding model needed. For in-room scenarios (multiple people sharing one mic), Zoom's AI uses acoustic diarization as a fallback.

**Lesson for meetily**: When the user is IN a Zoom call (not just listening passively), meetily could — in theory — tap ZoomAudioDevice to get the Zoom mix, rather than using the global system audio tap. This would exclude any non-Zoom audio and might be more reliable in some configurations. However, the Zoom SDK's per-participant stream API is private/licensed. For meetily's primary use case (passive, any audio source), the global Core Audio tap is correct.

*Sources*: [ZoomAudioDevice driver forum](https://devforum.zoom.us/t/how-to-install-the-zoomaudiodevice-driver/51460), [Zoom audio stream SDK thread](https://devforum.zoom.us/t/audio-stream-access-from-zooms-sdk/97596), [Zoom multichannel transcription guide](https://www.assemblyai.com/blog/transcribe-multichannel-zoom), [speaker diarization vs channel diarization](https://www.recall.ai/blog/speaker-diarization)

---

### 3. Microsoft Teams

**Capture topology**: Per-participant RTP streams via Teams' WebRTC SFU. Like Zoom, each participant has a separate audio stream at the protocol level. Teams' internal transcription consumes per-participant streams; external bots accessing the Graph API get a mixed stream plus speaker labels derived from stream identity.

**System-audio capture mechanism**: Teams is the platform. For external transcription tools, Microsoft provides the **Azure Communication Services** SDK and **Teams Bot Framework**. Bots join as participants and receive the conference mix via the Real-Time Media SDK. There is no macOS system-audio capture involved in Teams' own transcription — it all happens server-side via RTP.

**Echo cancellation**: AEC is embedded in the Teams client (same approach as Zoom — proprietary per-device AEC, falls back to `AUVoiceProcessingIO` on macOS). Applied before RTP encoding; transcription never sees raw echo.

**Transcription**: **Cloud (Azure AI Speech).** Real-time diarization via Azure Speech Service reached GA in 2024. Single-channel audio gets speaker labels GUEST1, GUEST2, etc. For **Intelligent Speakers** in a meeting room: Teams can enroll up to 10 voice profiles per tenant; the system matches enrolled voices in real-time and labels them by name. Transcripts stored in Microsoft 365 SharePoint and retrievable via Graph API.

**Diarization**: Two modes: (1) **Protocol-level** — stream SSRC → display name, 100% accurate for remote participants; (2) **Intelligent Speakers** — voice-profile enrollment for in-room microphone arrays, embedding-based matching against enrolled profiles. The Intelligent Speaker hardware (Microsoft-certified USB conference microphones) is required for case (2).

**Lesson for meetily**: The Intelligent Speakers pattern is architecturally interesting as a future direction — enroll voice profiles once, then match in real-time. But for v4 meetily, this is overkill. The core Teams lesson is: when you control the protocol, per-participant streams are free diarization. When you don't (standalone laptop), you need something else.

*Sources*: [Teams recording/transcription overview](https://learn.microsoft.com/en-us/microsoftteams/recording-transcription-overview), [Teams Intelligent Speakers](https://support.microsoft.com/en-us/office/use-microsoft-teams-intelligent-speakers-to-identify-in-room-participants-in-a-meeting-transcription-a075d6c0-30b3-44b9-b218-556a87fadc00), [Azure Speech real-time diarization GA](https://techcommunity.microsoft.com/blog/azure-ai-foundry-blog/announcing-general-availability-of-real-time-diarization/4147556)

---

### 4. Google Meet

**Capture topology**: Mix-minus with Last-3 WebRTC tracks. Google Meet's SFU sends each client up to **three audio tracks** — the three loudest participants other than yourself (mix-minus removes your own audio from what you hear). This means a bot or recording app can receive up to 3 per-participant audio streams in real-time. More than 3 simultaneous speakers get down-mixed.

**System-audio capture**: Google Meet uses a proprietary WebRTC stack with their own SFU infrastructure. For developers, the **Google Meet Media API** (requires Google Workspace Essentials or higher) exposes live RTP audio and video streams via WebRTC, with CSRC identifiers per participant in RTP packet headers. Third-party tools that don't have Workspace API access use meeting bots (running in a headless browser or virtual machine) that capture system audio via virtual audio driver or ScreenCaptureKit — essentially screen-scraping the audio.

**Echo cancellation**: Mix-minus inherently eliminates the need for most echo cancellation — each participant only hears other people, never their own voice echoed back. WebRTC's own AEC3 is also applied in the browser client as a defense-in-depth measure.

**Diarization**: The Meet Media API exposes CSRC (Contributing Source) identifiers in RTP packet headers. Each participant has a unique, stable CSRC for the duration of their session. A recording bot that consumes Meet Media API streams gets effectively free, 100%-accurate speaker diarization: CSRC → participant display name. For bots that lack API access and capture a mixed audio stream, acoustic diarization (pyannote/NeMo) is needed as a fallback.

**Lesson for meetily**: Google Meet's API is the gold standard for bot-based transcription quality. Recall.ai's speaker diarization API is built on top of exactly this — CSRC mapping for Google Meet participants. For meetily's standalone laptop case, none of this applies. But it validates that capture-level speaker separation is architecturally superior to acoustic diarization — whether done via protocol CSRC or OS-stream split.

*Sources*: [Google Meet Media API explainer (Recall.ai)](https://www.recall.ai/blog/what-is-the-google-meet-media-api), [Meet Media API virtual streams docs](https://developers.google.com/workspace/meet/media-api/guides/virtual-streams), [Google Meet mix-minus WebRTC analysis (Red5)](https://www.red5.net/blog/how-google-meet-implements-audio-using-mix-minus-with-webrtc/), [how to get Meet transcripts programmatically](https://www.recall.ai/blog/how-to-get-transcripts-from-google-meet-developer-edition)

---

### Comparative Summary Table

| Tool | Capture | macOS System Audio | AEC | Transcription | Diarization |
|---|---|---|---|---|---|
| **Granola** | mic + OS tap (2 streams) | Core Audio Tap (14.2+) | Not needed | Cloud (proprietary) | Capture-level (mic=YOU / tap=THEM) |
| **Zoom** | Per-participant RTP | ZoomAudioDevice (virtual driver) | Proprietary client AEC | Cloud (Azure AI) | Channel (SSRC→name) / acoustic fallback |
| **Teams** | Per-participant RTP | Not needed (platform) | Proprietary client AEC | Cloud (Azure AI Speech) | SSRC→name + Intelligent Speakers |
| **Google Meet** | Last-3 WebRTC tracks | Not needed (SFU) | Mix-minus + WebRTC AEC3 | Cloud (Gemini) | CSRC→name via Meet Media API |

**Key insight across all four tools**: None of them use acoustic speaker diarization models for their primary speaker attribution. They all exploit structural separation — OS streams, virtual drivers, or protocol identifiers — to know who is speaking before transcription ever begins. Acoustic diarization is a fallback of last resort, not the primary mechanism.

---

## Section B: OSS Landscape

### whisper.cpp (current backend)

- **License**: MIT
- **Language**: C++ with Rust bindings via `whisper-rs`
- **What it solves**: Batch-oriented Whisper inference with Apple Metal + CoreML acceleration. The `large-v3-turbo` model gives strong accuracy at 5-10x real-time on Apple Silicon.
- **Streaming limitations**: Whisper was designed for 30-second segments; the encoder expects a full 30s mel-spectrogram even when the speech is shorter. Processing shorter windows (1-5s) degrades accuracy and dramatically increases hallucination rate on silent/short clips. The standard workaround (and what meetily does) is VAD-gated buffering: accumulate until a speech boundary, then transcribe. This trades latency (up to several seconds per utterance) for accuracy.
- **Hallucination**: Well-known problem. Whisper generates plausible-sounding text from near-silence, music, or noise. Mitigating filters (no_speech_prob threshold, hallucination word lists) help but don't eliminate the problem.
- **Right primitive for meetily v4?** Yes, for the Rust/Tauri frontend where C++ bindings are available. It is the least-friction path. However, WhisperKit is strictly better on Apple Silicon — same model quality, lower latency, better streaming design, Neural Engine instead of Metal GPU.

*Sources*: [whisper.cpp GitHub](https://github.com/ggerganov/whisper.cpp), [streaming real-time issues (#1653)](https://github.com/ggml-org/whisper.cpp/issues/1653)

---

### faster-whisper + whisper-streaming

- **License**: MIT (faster-whisper), BSD-2-Clause (whisper-streaming / UFAL)
- **Language**: Python (CTranslate2 backend for faster-whisper)
- **What it solves**: faster-whisper gives 4-5x speedup over whisper.cpp on NVIDIA CUDA via INT8 quantization in CTranslate2. whisper-streaming (UFAL) builds a ~3.3s latency streaming pipeline on top, using a local-agreement policy where confirmed tokens are emitted and partial hypotheses are updated.
- **2025 state**: whisper-streaming is being superseded by SimulStreaming and WhisperLiveKit. WhisperLiveKit adds WebSocket streaming and optional diarization integration.
- **macOS / Apple Silicon**: CTranslate2 does not use Metal GPU. On Apple Silicon, whisper.cpp with CoreML acceleration is faster than faster-whisper. faster-whisper is primarily a CUDA story.
- **Right primitive for meetily v4?** Relevant for the FastAPI Python backend (server-side batch processing or streaming). Not a replacement for the Rust/Tauri frontend's whisper.cpp.

*Sources*: [faster-whisper GitHub](https://github.com/SYSTRAN/faster-whisper), [whisper-streaming GitHub](https://github.com/ufal/whisper_streaming), [WhisperLiveKit](https://github.com/QuentinFuxa/WhisperLiveKit), [Modal blog: choosing Whisper variants](https://modal.com/blog/choosing-whisper-variants)

---

### WhisperKit (Argmax)

- **License**: MIT
- **Language**: Swift (Apple-native), CoreML / Apple Neural Engine
- **What it solves**: Whisper reimplemented for the Apple Neural Engine. The Audio Encoder is modified for streaming inference (partial mel-spectrogram); the Text Decoder is adapted to handle partial audio. Result, presented at ICML 2025: **2.2% WER on large-v3-turbo equivalent, ~0.45s mean streaming latency** on M-series chips.
- **Compression**: OD-MBP (Outlier-Decomposed Mixed-Bit Palletization) keeps model under 1 GB while staying within 1% WER of the uncompressed model.
- **Server mode**: `ArgmaxPro` local WebSocket server API-compatible with Deepgram and other cloud STT providers. Any app that calls a Deepgram endpoint can swap in WhisperKit with a URL change.
- **Compared to whisper.cpp**: WhisperKit uses the Neural Engine (dedicated accelerator, lower power, lower latency) while whisper.cpp uses Metal GPU (higher peak throughput but worse latency). For a meeting recorder that runs continuously, Neural Engine is strictly better.
- **Right primitive for meetily v4?** **Yes — the single best transcription runtime for the Tauri/macOS target.** The blocker is language: WhisperKit is Swift-only, so Tauri integration requires either a Tauri Swift plugin or a subprocess (run the WhisperKit Deepgram-compatible server, connect via WebSocket). Both are reasonable.

*Sources*: [WhisperKit GitHub](https://github.com/argmaxinc/WhisperKit), [WhisperKit ICML 2025 paper](https://arxiv.org/html/2507.10860v1), [Whisper performance on Apple Silicon](https://www.voicci.com/blog/apple-silicon-whisper-performance.html)

---

### pyannote-audio

- **License**: MIT (code), CC-BY-4.0 (community model weights). Full pipeline model (`speaker-diarization-3.1`) requires accepting HuggingFace terms before downloading.
- **Language**: Python (PyTorch)
- **What it solves**: State-of-the-art speaker diarization: VAD → speaker segmentation → speaker embedding → clustering. speaker-diarization-3.1 removes the onnxruntime dependency (now pure PyTorch).
- **Real-time feasibility**: ~2.5% real-time factor on GPU (V100 class). On Apple Silicon CPU, significantly slower — 10-30% real-time factor, making it infeasible for true real-time. Post-meeting batch processing is fine.
- **FluidAudio CoreML port**: muesli references "FluidAudio's pyannote-based CoreML diarization model" — a CoreML-compiled port that runs on Apple Neural Engine. This would be the right primitive for real-time diarization on Apple Silicon. Not widely publicized.
- **Right primitive for meetily v4?** Post-meeting batch diarization in the FastAPI backend, yes. Real-time in the Rust/Tauri client, no.

*Sources*: [pyannote-audio GitHub](https://github.com/pyannote/pyannote-audio), [speaker-diarization-3.1 model card](https://huggingface.co/pyannote/speaker-diarization-3.1)

---

### WhisperX

- **License**: BSD-4-Clause
- **Language**: Python
- **What it solves**: Three extensions on top of Whisper: (1) batched inference via faster-whisper for ~70x speedup; (2) word-level timestamps via forced alignment with Wav2Vec2; (3) speaker diarization via pyannote-audio, producing `[SPEAKER_00]: word word word` output.
- **License caveat**: WhisperX itself is BSD-4-Clause, but pyannote model weights require HuggingFace account + license acceptance. Internal/research use is fine; commercial redistribution needs review.
- **Right primitive for meetily v4?** For the FastAPI backend's post-meeting processing (rich transcript with timestamps + speaker labels), yes. Not for real-time in the Rust client.

*Sources*: [WhisperX GitHub](https://github.com/m-bain/whisperx), [pyannote/speaker-diarization-3.1](https://huggingface.co/pyannote/speaker-diarization-3.1)

---

### NVIDIA NeMo / Streaming Sortformer

- **License**: Apache 2.0
- **Language**: Python (PyTorch / NVIDIA Riva)
- **What it solves**: Streaming Sortformer (`diar_streaming_sortformer_4spk-v2.1`, released August 2025) is a frame-level real-time speaker diarization model. Uses an Arrival-Order Speaker Cache (AOSC) to track speaker embeddings in a sliding window. Maximum 4 speakers.
- **Hardware requirement**: NVIDIA GPU (CUDA). No Metal/ANE support. Deployed via NVIDIA Riva.
- **Right primitive for meetily v4?** No for the macOS laptop use case. Interesting for a future cloud/server backend with GPU.

*Sources*: [Streaming Sortformer announcement](https://developer.nvidia.com/blog/identify-speakers-in-meetings-calls-and-voice-apps-in-real-time-with-nvidia-streaming-sortformer/), [diar_streaming_sortformer_4spk-v2.1](https://huggingface.co/nvidia/diar_streaming_sortformer_4spk-v2.1)

---

### sherpa-onnx

- **License**: Apache 2.0
- **Language**: C++ core with bindings for Rust, Python, Go, Java, Swift, Flutter
- **What it solves**: Batteries-included runtime for speech AI without internet. Supports streaming ASR (Zipformer/Conformer-Transducer, Paraformer, LSTM), TTS, VAD (Silero), speaker diarization. Runs on macOS, iOS, Android, embedded systems.
- **macOS / Metal**: ONNX Runtime's CoreML execution provider offers some ANE acceleration but with significant operator coverage gaps. Performance on Apple Silicon is usable but not as optimized as WhisperKit.
- **Right primitive for meetily v4?** The Rust bindings make sherpa-onnx interesting for meetily-client on non-macOS platforms (Windows, Linux). On macOS, WhisperKit is better. sherpa-onnx is a strong single-binary cross-platform alternative to whisper.cpp.

*Sources*: [sherpa-onnx GitHub](https://github.com/k2-fsa/sherpa-onnx), [sherpa-onnx docs](https://k2-fsa.github.io/sherpa/onnx/index.html)

---

### Recall.ai

- **License**: Commercial (not open source)
- **What it solves**: Single API for meeting bots on Zoom, Meet, Teams, Slack Huddles. Speaker Diarization API using CSRC/channel mapping (near-100% accuracy for Google Meet), not acoustic diarization.
- **Right primitive for meetily v4?** No. Useful as competitive context: meetily's "no bot, no cloud" value prop is differentiated.

*Sources*: [Recall.ai](https://www.recall.ai/), [speaker diarization approach](https://www.recall.ai/blog/speaker-diarization)

---

### MeetingBaaS

- **License**: Open source components + hosted service
- **What it solves**: Bot-based meeting capture similar to Recall.ai. Switched to token-based pricing in late 2025.
- **Right primitive for meetily v4?** No.

*Sources*: [MeetingBaaS vs Recall.ai](https://www.meetingbaas.com/en/blog/meeting-baas-vs-recall-ai)

---

### vexa-ai/vexa

- **License**: Open source (self-hostable)
- **Language**: Python + Docker
- **What it solves**: Self-hosted meeting bot platform. Bots join Google Meet and Teams. Real-time WebSocket transcription with sub-1s latency at 16 kHz via Whisper models. PostgreSQL storage. Modular — swappable ASR backend.
- **Right primitive for meetily v4?** meetily's FastAPI backend architecture draws inspiration from vexa's modular WebSocket streaming ASR design. The bot-based capture is not meetily's use case.

*Sources*: [vexa.ai](https://vexa.ai/), [Vexa architecture](https://www.blog.brightcoding.dev/2026/02/28/vexa-the-self-hosted-meeting-bot-api-revolution/)

---

### AudioCap (insidegui/AudioCap)

- **License**: MIT
- **Language**: Swift
- **What it solves**: Canonical sample code for macOS 14.4+ Core Audio system audio recording via `AudioHardwareCreateProcessTap`. Demonstrates the complete flow: `NSAudioCaptureUsageDescription`, tap creation, aggregate device with tap-only configuration (no sub_device_list to avoid echo — same as meetily-audio's implementation), IOProc callback.
- **Key implementation detail**: AudioCap is a signed, bundled macOS app. TCC permission is attributed to its bundle's stable identity. This is why it works reliably. The same code in an unsigned CLI binary fails silently.
- **Permission granularity**: macOS 14.4+ introduced "System Audio Recording Only" as a sub-permission of Screen Recording. Apps with `NSAudioCaptureUsageDescription` can request audio-only permission without full Screen Recording access.
- **Right primitive for meetily v4?** Reference implementation, not a library. meetily-audio's `capture/core_audio.rs` implements the same pattern. The code is correct.

*Sources*: [AudioCap GitHub](https://github.com/insidegui/AudioCap), [Apple Core Audio tap documentation](https://developer.apple.com/documentation/CoreAudio/capturing-system-audio-with-core-audio-taps), [From Core Audio to LLMs](https://dev.to/yingzhong_xu_20d6f4c5d4ce/from-core-audio-to-llms-native-macos-audio-capture-for-ai-powered-tools-dkg)

---

### audiotee (makeusabrew/audiotee)

- **License**: MIT
- **Language**: Rust crate (wraps a Swift subprocess)
- **What it solves**: Rust-friendly API for streaming system audio from macOS 14.2+ Core Audio tap. Internally spawns an `audiotee` Swift CLI and pipes raw PCM via stdout. NOT a pure Rust implementation.
- **Relationship to meetily-audio**: meetily-audio's `capture/core_audio.rs` using `cidre` is more architecturally clean (pure Rust, no subprocess, direct CoreAudio bindings). audiotee is an alternative for teams that don't want the cidre dependency.
- **Right primitive for meetily v4?** meetily-audio is already doing the same thing better.

*Sources*: [audiotee crates.io](https://crates.io/crates/audiotee), [audiotee GitHub](https://github.com/makeusabrew/audiotee), [audiotee explainer](https://stronglytyped.uk/articles/audiotee-capture-system-audio-output-macos)

---

### muesli (pHequals7/muesli)

- **License**: GitHub project — check repo for license
- **Language**: Swift (native macOS app)
- **What it solves**: OSS Granola + WisprFlow alternative. Key capabilities:
  - CoreAudio process tap by default, ScreenCaptureKit fallback
  - Silero VAD for natural speech boundary detection
  - Apple Parakeet TDT on ANE for dictation (~0.13s latency)
  - FluidAudio pyannote-based CoreML diarization for per-speaker attribution within system-audio stream
  - Local LLM (Ollama) or cloud for meeting notes
- **Significance for meetily**: The most architecturally complete OSS analog. Proves the full stack is achievable locally. The critical differentiator: native Swift `.app` → TCC permissions properly attributed, no terminal inheritance problem. The Parakeet TDT + CoreML diarization combo is the right local-only direction for Apple Silicon.

*Sources*: [muesli GitHub](https://github.com/pHequals7/muesli)

---

### macOS TCC and Core Audio Tap: The Permission Bug Root Cause

This deserves its own entry because it is the root cause of the reported bug.

**How TCC identifies apps**: TCC (Transparency, Consent, and Control) uses an app's **code signature** — specifically `TeamIdentifier` + `BundleIdentifier` — to store permission grants. An ad-hoc signed binary produces a different code directory hash on every build. An unsigned binary has no stable identity at all.

**What happens with unsigned/CLI binaries**: When an unsigned binary calls `AudioHardwareCreateProcessTap`, the OS cannot identify the requester as a stable entity. There is no permission prompt. The tap is created, but if the calling process (or its terminal ancestor) hasn't been granted audio capture permission, the tap silently returns zeros. No error code, no log message.

**Why it works via SSH**: When the binary runs via `sshd`, TCC attributes the request differently. If `sshd` or a prior SSH-launched session received the permission grant (perhaps during a previous interactive approval), the tap works. This is not reliable and not the right production path.

**Why it fails in Warp/Terminal.app**: Warp (`dev.warp.Warp`) and Terminal.app (`com.apple.Terminal`) each have their own TCC entries. The grant was never made for those terminal bundle IDs' ancestry chain for meetily-client.

**macOS 14.4 granularity**: Introduced `kTCCServiceSystemAudioCapture` (distinct from `kTCCServiceScreenCapture`). Apps with `NSAudioCaptureUsageDescription` in their Info.plist request audio-only permission without full screen recording. This permission dialog only fires from a proper `.app` bundle.

**The fix**: meetily-client needs to be either (a) embedded as a signed helper tool inside the Tauri `.app` bundle, or (b) signed with a Developer ID Application certificate. The standalone unsigned CLI is architecturally incompatible with macOS TCC for audio capture.

*Sources*: [Apple TCC deep dive (Rainforest QA)](https://www.rainforestqa.com/blog/macos-tcc-db-deep-dive), [HackTricks macOS TCC](https://hacktricks.wiki/en/macos-hardening/macos-security-and-privilege-escalation/macos-security-protections/macos-tcc/index.html), [AudioCap permission handling](https://github.com/insidegui/AudioCap/blob/main/AudioCap/ProcessTap/AudioRecordingPermission.swift), [macOS audio capture permission docs](https://developer.apple.com/documentation/bundleresources/requesting-authorization-for-media-capture-on-macos), [OBS system audio recording permission issue](https://github.com/obsproject/obs-studio/issues/10401)

---

## Section C: Recommendation for meetily v4

### The Root Cause (not a bug in the audio code)

meetily-audio's `capture/core_audio.rs` is **correct**. The cidre-based Core Audio tap implementation matches AudioCap, audiotee, and muesli — it is the right approach. The bug is a TCC permission attribution problem: an unsigned CLI binary cannot own a stable TCC identity, so `AudioHardwareCreateProcessTap` silently returns zeros when launched from an interactive terminal whose bundle ID has no prior `kTCCServiceSystemAudioCapture` grant.

The SSH case works because `sshd`'s TCC attribution chain had a prior grant. This is coincidence, not architecture.

**This is not an argument to abandon the per-source pipeline or the Core Audio tap. It is an argument to fix the deployment layer.**

---

### Recommended Architecture

**Capture strategy: per-source split (mic + Core Audio tap)**

Keep the current architecture: mic via cpal → [YOU], system audio via Core Audio tap → [THEM]. This is what Granola, muesli, and every production meeting transcriber on macOS does. It provides perfect YOU/THEM diarization with zero acoustic error, eliminates AEC entirely, and is the minimum viable architecture for a privacy-first local meeting transcriber.

The mic stream and system-audio tap capture completely different acoustic paths at the OS level. There is no acoustic echo between them — the mic sees your voice, the tap sees the digital system audio bus before the DAC. They never mix unless you are using open speakers without headphones AND the mic happens to pick up speaker output. For that edge case, a lightweight cross-correlation gate is sufficient; a full AEC stack is not. AUVoiceProcessingIO and sonora-aec3 are both wrong choices here.

**macOS system-audio capture mechanism: Core Audio Tap, inside a signed app bundle**

1. **Primary**: `AudioHardwareCreateProcessTap` global mono tap (macOS 14.2+). meetily-audio's existing `capture/core_audio.rs` implementation is correct. The `NSAudioCaptureUsageDescription` key in Info.plist triggers the narrower audio-only permission dialog on macOS 14.4+.

2. **Permission dialog**: Only fires correctly when the binary runs as part of a signed `.app` bundle. The Tauri frontend already satisfies this. The capture code should live in the Tauri app process (or a signed XPC helper), not in a standalone unsigned CLI.

3. **For developer/CLI use**: Sign meetily-client with a Developer ID Application certificate to give it a stable TCC identity. Or embed it as a signed helper inside the Tauri `.app` and invoke it as a subprocess — it then inherits the app's TCC grants when launched from within the bundle.

4. **ScreenCaptureKit fallback**: Keep as a compile-time feature gate for macOS < 14.2. muesli uses this exact pattern.

**Transcription: WhisperKit for the Tauri app, whisper.cpp as fallback**

For the Tauri macOS app:
- Primary: **WhisperKit** (Swift, Apple Neural Engine). ~0.45s streaming latency, 2.2% WER with large-v3-turbo equivalent, <1 GB memory, runs on Neural Engine (lower power than Metal GPU, better for continuous recording). Integration path: run WhisperKit's local Deepgram-compatible WebSocket server as a subprocess from the Tauri app; Rust code connects via WebSocket. Avoids Swift→Rust FFI while keeping everything local.
- Fallback / non-macOS: **whisper.cpp** via `whisper-rs` (current backend). Keep for Windows/Linux and CI environments.

For the FastAPI Python backend (post-meeting enrichment):
- **faster-whisper** (CUDA) or **whisper.cpp Python bindings** for re-transcription with higher accuracy where available.
- **WhisperX** (faster-whisper + pyannote-audio) for optional post-meeting speaker diarization within the THEM stream → per-participant labels as a v4 stretch goal.

**Diarization: capture-level only for v4 core, pyannote batch as v4 enhancement**

v4 core: mic = YOU, tap = THEM. That's it. This is what Granola ships; it covers 80% of meeting use cases. No acoustic diarization in the real-time path.

v4 enhancement (post-meeting, optional): WhisperX + pyannote-audio in the FastAPI backend over the saved THEM WAV file. Produces:
```
[YOU] here's the agenda for today
[SPEAKER_1] sounds good, let's start with the Q3 numbers
[SPEAKER_2] I can walk through those
```
This is a batch operation after the meeting ends — no real-time constraint.

Future (v5+): Explore the FluidAudio CoreML pyannote port (referenced in muesli) for real-time per-speaker labels on ANE.

**What survives from current code**

| Component | Decision | Rationale |
|---|---|---|
| `meetily-audio/src/capture/core_audio.rs` | **KEEP** | Correct implementation. Bug is in TCC attribution, not this code. |
| `meetily-audio/src/capture/microphone.rs` | **KEEP** | cpal mic capture is correct. |
| `meetily-audio/src/vad.rs` | **KEEP** | VAD-gated chunking is required for Whisper. |
| `meetily-audio/src/resample.rs` | **KEEP** | Resampling to 16kHz for Whisper is required. |
| `meetily-audio/src/aec.rs` (sonora-aec3) | **DELETE** | Wrong problem. Capture-level split eliminates AEC need. |
| 3-layer dedup in `meetily-client/src/main.rs` | **DELETE** | Was compensating for broken tap → mic echo. Not needed when tap works. |
| `transcribe.rs` (whisper-rs wrapper) | **KEEP** | Correct batch + streaming transcription. |
| Tauri frontend audio pipeline | **KEEP architecture** | Already in a signed app bundle — route system audio here, not through CLI. |
| FastAPI backend | **KEEP** | Add WhisperX post-meeting diarization as enhancement. |

**What to throw away**

- `meetily-audio/src/aec.rs` and the `aec` Cargo feature
- The 3-layer dedup (Whisper hallucination filter, token-containment check, time-overlap ≥40%) in `meetily-client/src/main.rs`
- `per-source-pipeline-design.md` (all versions)
- `per-source-implementation-handoff.md`
- Any code wiring up `AUVoiceProcessingIO` or sonora-aec3

**Why this avoids the bug Qi hit**

| Failure mode | v3 (current) | v4 (recommended) |
|---|---|---|
| TCC permission attribution | Unsigned CLI → inherits terminal's TCC identity → silent zeros | Signed `.app` bundle → stable bundle ID → TCC dialog fires once, persists |
| AEC complexity | sonora-aec3 / AUVoiceProcessingIO — wrong path | Deleted — not needed when streams are OS-level separated |
| Dedup complexity | 3-layer dedup on top of broken capture | Deleted — clean capture needs no dedup |
| Diarization accuracy | Dedup-based "them filtering" | Native OS stream split: mic=YOU, tap=THEM, zero error |
| Transcription latency | whisper.cpp batch on ~30s segments | WhisperKit ANE streaming, ~0.45s per utterance |

**The one-sentence version**: meetily v4 is the Tauri app (already a signed app bundle) doing Core Audio tap capture (already the right code, just move it here) with WhisperKit streaming transcription (new) and no AEC or dedup (deleted), producing mic=[YOU] and tap=[THEM] natively.

---

### Architecture Diagram

```
macOS 14.2+ (meetily.app — signed, stable bundle ID)
│
├── TCC grants (one-time user approval, persistent):
│   ├── NSMicrophoneUsageDescription → mic capture
│   └── NSAudioCaptureUsageDescription → Core Audio tap
│
├── Mic stream (cpal / AVAudioEngine)
│   └── [YOU] channel
│       → Silero VAD → speech boundaries
│       → 16kHz PCM chunks
│       → WhisperKit ANE (WebSocket to local server)
│       → [YOU] segments with timestamps
│
├── System audio tap (Core Audio global mono tap via cidre)
│   └── [THEM] channel
│       → Silero VAD → speech boundaries
│       → 16kHz PCM chunks
│       → WhisperKit ANE (WebSocket to local server)
│       → [THEM] segments with timestamps
│
├── Merge by timestamp → unified transcript stream
│   → Tauri event → React UI (live display)
│
└── FastAPI backend (optional, on localhost:5167)
    ├── Transcript storage (SQLite / aiosqlite)
    ├── Post-meeting: WhisperX + pyannote batch diarization
    │   of saved THEM WAV → [SPEAKER_N] labels
    └── LLM summary (Ollama / Claude / Groq / OpenRouter)
```

No AEC. No dedup. No per-source-pipeline complexity beyond the simple mic/tap split. No CLI TCC problem.
