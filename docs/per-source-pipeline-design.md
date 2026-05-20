# Meetily Per-Source Audio Pipeline — Design Doc

**Status:** Draft v3.2 (post B1 design pass)
**Date:** 2026-05-09
**Context:** [[design]] (v1 architecture) · [[code/meetily]] · WI-41 streaming VAD (PR #50)
**Supersedes:** Tauri's pre-mixed transcription path. Extends v1 design with strict per-source rule.

**Revision history:**
- v1 (2026-05-08): initial draft.
- v2 (2026-05-08): codex round 1 corrections — fixed macOS backend (CoreAudio Tap, not SCK); expanded AEC integration concerns (frame alignment, drift, pre-processing order); split issue A into smaller migration steps; tempered industry research claims; expanded acceptance criteria; addressed backpressure and Whisper concurrency.
- v3 (2026-05-08): codex round 2 corrections — split B into B1 (CLI AEC) and B2 (Tauri AEC, depends on Tauri-Unmix); type-level no-mixing enforced via private fields + source-specific constructors; macOS Core Audio Tap requirements corrected (14.2+, `NSAudioCaptureUsageDescription`, no entitlement); aligner-level AEC backpressure replaces per-source drop; explicit Tauri schema/UI files added (`recording_saver.rs`, frontend types); `AudioSource` trait made object-safe; per-source VAD warm-up addressed.
- v3.2 (2026-05-09): B1 design pass corrections — switched AEC dep from `webrtc-audio-processing` (autotools/meson system installs required) to `sonora-aec3` (pure-Rust port of WebRTC AEC3, BSD-3, zero system deps; tradeoffs documented in §3 "Dependency choice"); block size corrected to 4 ms / 64 samples (sonora-aec3's `BlockProcessor` is the algorithm-native granularity, not the 10 ms WebRTC frame); custom aligner removed — sonora-aec3's `RenderDelayController` does delay estimation internally; drop policy revised — render-side drop on tee overflow with `RenderDelayController` re-estimation (~2–5 s reconvergence) accepted for v1, paired-frame coherent drop deferred to a follow-up issue.

---

## Summary

Meetily currently has two clients with divergent audio architectures:

- **Tauri desktop app** (`frontend/`) — pre-mixes mic + system audio into one stream, then transcribes once. Loses speaker attribution permanently.
- **CLI** (`meetily-client/`) — captures mic + system as independent streams, transcribes each separately, labels segments `[YOU]` / `[THEM]`. Better attribution, but suffers acoustic bleed (mic picks up speaker output → duplicate transcripts).

This doc proposes converging both clients on a **strict per-source pipeline**: capture, AEC, VAD, and transcription stay separate per source; only the *final transcript* interleaves segments from both sources by start timestamp. **Mixing in any transcription path is explicitly disallowed.**

This is a strict refinement of the v1 design (`design.md`), which already stated "source tagging, not diarization" (decision 8) and "interleave segments by timestamp, tag source" (Phase 2 WI-13). v1's intent matches this doc; Tauri's actual implementation diverged. This doc realigns both clients on v1's stated architecture and adds AEC + clean macOS capture improvements.

## Architectural principle (load-bearing)

**No mixing in the transcription path. Ever.** This is not a performance choice; it is a non-negotiable design rule. Mixing irreversibly destroys speaker attribution, and we will not accept that tradeoff to save CPU. Every reviewer and contributor must understand this rule supersedes optimization opportunities.

**Enforcement (not just a `grep`):** type-level separation between transcription and recording paths. Introduce two distinct frame types in the new `meetily-audio` crate:

- `TranscriptionFrame { source: SourceLabel, samples: Vec<f32>, timestamp_ms: u64 }` — single-source, *cannot be constructed* by mixing two streams (no `From<(MicFrame, SystemFrame)>` impl, no merge constructor).
- `RecordingMixFrame { samples: Vec<f32>, timestamp_ms: u64 }` — produced only by an explicit recording-path mixer; the transcription pipeline does not accept this type.

Architectural tests verify the trait bound: any function feeding the VAD or Whisper sink takes `TranscriptionFrame`, never `RecordingMixFrame`. `grep` is a fallback acceptance check, not the enforcement.

## Goals

1. **Per-source transcription end-to-end.** Mic and system audio are never mixed before Whisper.
2. **Speaker attribution preserved.** Every transcript segment carries a source label (`[YOU]` for mic, `[THEM]` for system).
3. **Interleaved output.** Final transcript is a single chronological stream, sorted by segment start timestamp, with source labels. Live display shows segments as they complete (out-of-order display is OK due to Whisper latency).
4. **Acoustic bleed substantially reduced.** Acoustic Echo Cancellation removes most speaker output from the mic signal before transcription. (Goal is *substantial reduction* — AEC is suppression, not elimination; see Acceptance Criteria.)
5. **Clean macOS capture.** Use the same macOS backend Tauri already uses (Core Audio Tap via cidre); eliminate the BlackHole + Multi-Output Device user setup for the CLI.
6. **Unified audio code.** Tauri and CLI share one `meetily-audio` crate.

## Non-goals (v1)

- **Multi-speaker diarization within the system stream.** A Zoom call with 3 remote participants will all label as `[THEM]` in v1. Per-speaker labels (`[THEM-1]`, `[THEM-2]`...) tracked as a future issue.
- **Recording WAV output.** Per discussion, recording is deprioritized for v1. Pipeline will write `mic.wav` and `system.wav` only if needed for debugging. **Mixed playback WAV is explicitly out of scope** and tracked as a separate, low-priority issue.
- **Linux / Windows native loopback parity.** macOS gets clean capture first via Core Audio Tap. Linux uses cpal/PulseAudio fallback (existing CLI path); Windows uses WASAPI loopback (existing CLI path). Native equivalents tracked separately.
- **Replacing Silero VAD.** WebRTC AEC3 ships with its own VAD, but evaluation against Silero is out of scope for v1.
- **Replacing existing mic pre-processing.** Tauri currently applies high-pass + optional RNNoise + EBU R128 normalization. AEC integration must address ordering (see AEC Integration); we do not commit to removing these in v1.

---

## Current state (verified against code 2026-05-08)

### CLI (`meetily-client/`)

```
mic device   ──► cpal input  ──► resample 48k→16k ──► Silero VAD ──► Whisper(mic)   ──► segments[mic]
                                                                                          │
system device──► cpal loopback ─► resample 48k→16k ─► Silero VAD ──► Whisper(system) ──► segments[system]
                                                                                          │
                                                                                          ▼
                                                                  merge by timestamp → interleaved transcript
```

- Per-source ✓. Labels preserved ✓.
- **No AEC** → mic picks up speaker output → duplicate transcripts (the user-visible bug Qi reported).
- Capture path uses cpal on the **default output device** (`meetily-client/src/audio/capture.rs:302-306, 377-381`) — the BlackHole + Multi-Output Device route on macOS.
- **System capture is conditional**: only starts if `--system` is provided. Without it, only mic is captured.

### Tauri (`frontend/src-tauri/`)

```
mic capture  ─┐
              ├─► ring buffer (50ms) ──► mixer.mix_window() ──► single mixed signal
system        ─┘                                                       │
(Core Audio                                                            ├──► VAD ──► Whisper ──► segments (no source label)
 Tap)                                                                  └──► recording WAV
```

- macOS system capture: **Core Audio Tap via cidre** (`frontend/src-tauri/src/audio/capture/core_audio.rs:91`, `with_mono_global_tap_excluding_processes`). The `ScreenCaptureKit` enum variant exists in `backend_config.rs:11-13` but the active default is `AudioCaptureBackend::CoreAudio` (`backend_config.rs:79`).
- Single transcription pass for performance; mixed chunks marked `DeviceType::Microphone` at `pipeline.rs:844-849`.
- Transcription worker (`audio/transcription/worker.rs`) emits source as hardcoded `"Audio"` — UI cannot distinguish mic from system.
- **Speaker attribution lost** at the mixer (`pipeline.rs:826`). Diverges from v1 design intent.
- Mic pre-processing: high-pass filter, optional RNNoise, EBU R128 normalization applied at capture before pipeline.

---

## Target state

### Unified per-source pipeline (both clients)

```
┌─ macOS ─────────────────────────────────────────────────────────────────────────────────────────┐
│  mic (cpal CoreAudio)         ──► resample/frame-align ──┐                                       │
│                                                          ├─► AEC3 ──► VAD(mic)    ──► Whisper(mic)    ──► segments[mic, ts]    │
│  system (Core Audio Tap)      ──► resample/frame-align ──┤        (mic = near-end, system = reference)               │       │
│                                                          └─►       ──► VAD(system) ──► Whisper(system) ──► segments[system, ts] │
└─────────────────────────────────────────────────────────────────────────────────────────────────┘
                                                                                          │
                                                  ┌───────────────────────────────────────┘
                                                  ▼
                            interleave by start_timestamp, label [YOU]/[THEM] → final transcript
```

Linux/Windows: same logical pipeline, different capture backends (existing cpal/PulseAudio/WASAPI from CLI).

### Recording (deprioritized for v1)

- v1: optionally `mic.wav` + `system.wav` for debugging. No mixed file.
- Future issue: combined-playback WAV for human review (mix mic + system *only* in the recording path; type system prevents this from feeding transcription).

---

## Component design

### 1. `meetily-audio` crate (new)

Workspace member at `meetily-audio/`. Owns capture + DSP code shared between Tauri and CLI.

**Public API surface (sketch — subject to change during implementation):**

```rust
/// Source label, baked into transcription frames at capture time.
#[derive(Copy, Clone, Debug, Eq, PartialEq)]
pub enum SourceLabel { Mic, System }

/// Single-source frame. Private fields prevent caller-side construction.
/// Only the per-source capture pipeline can build these via the
/// from_mic_capture / from_system_capture constructors below.
pub struct TranscriptionFrame {
    source: SourceLabel,
    samples: Vec<f32>,    // 16kHz mono after resample
    timestamp_ms: u64,    // monotonic from session start
}

impl TranscriptionFrame {
    /// Only the mic capture pipeline calls this. Crate-private constructor
    /// (`pub(crate)`) ensures external callers cannot bypass via the public
    /// surface; mixing source labels with arbitrary samples is impossible.
    pub(crate) fn from_mic_capture(samples: Vec<f32>, ts_ms: u64) -> Self {
        Self { source: SourceLabel::Mic, samples, timestamp_ms: ts_ms }
    }
    pub(crate) fn from_system_capture(samples: Vec<f32>, ts_ms: u64) -> Self {
        Self { source: SourceLabel::System, samples, timestamp_ms: ts_ms }
    }
    pub fn source(&self) -> SourceLabel { self.source }
    pub fn samples(&self) -> &[f32] { &self.samples }
    pub fn timestamp_ms(&self) -> u64 { self.timestamp_ms }
}

/// Recording-only frame. Constructed only by the recording-path mixer
/// (`pub(crate)` constructor). Transcription pipeline does not accept this type.
pub struct RecordingMixFrame { samples: Vec<f32>, timestamp_ms: u64 }

/// Object-safe audio source trait. Returns a boxed Stream so trait objects
/// (`Box<dyn AudioSource>`) compile. Native sample rate is exposed via
/// `sample_rate()` so consumers can wire the resampler.
pub trait AudioSource: Send {
    fn label(&self) -> SourceLabel;
    fn sample_rate(&self) -> u32;
    fn start(&mut self) -> Result<Pin<Box<dyn Stream<Item = AudioFrame> + Send>>>;
    fn stop(&mut self) -> Result<()>;
}

pub fn create_mic_source(device: Option<&str>) -> Result<Box<dyn AudioSource>>;
pub fn create_system_source() -> Result<Box<dyn AudioSource>>;  // CoreAudio Tap on macOS, fallback elsewhere

/// Aligned AEC processor. Owns the frame aligner; consumes both mic and
/// system streams; emits cleaned mic frames as `TranscriptionFrame { Mic }`.
/// Frame alignment + backpressure are the aligner's responsibility (see
/// "Aligner + backpressure" below), not the upstream channels.
///
/// Construction returns the receivers along with the pipeline so each
/// receiver has exactly one owner from the start (Tokio mpsc is
/// single-consumer; receivers cannot be cloned or fetched via `&self`).
pub struct AecPipeline { /* webrtc-audio-processing + aligner */ }
pub struct AecOutputs {
    pub cleaned_mic: mpsc::Receiver<TranscriptionFrame>,
    pub system: mpsc::Receiver<TranscriptionFrame>,
}
impl AecPipeline {
    /// Returns the pipeline plus single-owner receivers for cleaned mic
    /// and pass-through system frames.
    pub fn new(sample_rate: u32) -> Result<(Self, AecOutputs)>;
    /// Push raw mic and system frames (any size, any rate). Aligner buffers,
    /// resamples to 16kHz, frames to 10ms (160 samples), pairs by timestamp,
    /// runs AEC3, and emits cleaned frames via the receivers returned at
    /// construction.
    pub fn ingest_mic(&mut self, frame: AudioFrame);
    pub fn ingest_system(&mut self, frame: AudioFrame);
}
```

**Channel design:** bounded `tokio::sync::mpsc` between stages. Default capacity 100 frames. **Drop policies must NOT be applied independently between mic and system at any stage that feeds AEC** — that would desynchronize AEC near/far ends. The pre-AEC capture channels are sized large enough (1 second of audio at native rate) that the only drop point is the AEC aligner itself, which uses paired-frame coherent drop. Post-AEC channels (cleaned mic, system) feed independent Whisper workers and may use per-source drop-oldest there, since alignment is no longer required.

**Aligner + backpressure (the AEC stage):**

The aligner is the single place that pairs mic and system frames before AEC3. Its job:

1. Buffer incoming mic and system frames (each tagged with `timestamp_ms`).
2. Resample both to 16kHz; reframe to 10ms (160 samples per frame).
3. **Pair frames by `timestamp_ms`** with a sliding window. Allow ±100ms drift between mic and system clocks (matches `set_stream_delay_ms` initial hint).
4. **Coherent drop policy** — if mic outpaces system (system audio ended / muted), substitute silence for the far-end reference and pass the mic frame through. If system outpaces mic (mic muted / dropped), advance the system buffer without producing a paired output. Never drop one stream silently.
5. **Backpressure overflow** — if either internal buffer exceeds 1 second of audio (>100 frames), drop the oldest **paired window** and increment a metric. This preserves alignment by always dropping coherently.
6. **Warm-up** — emit nothing for the first ~200ms after both streams have started; this is buffer-fill / startup, not full AEC convergence (AEC3 takes 2–5 seconds to fully converge — first few seconds of mic transcript may have residual echo regardless). Pass mic through (un-AEC'd) only as a debug option (`--no-aec` flag).

This makes AEC backpressure *the aligner's problem*, not the upstream channels' problem. Source-independent drop is forbidden.

**Whisper sink:** the crate does **not** own Whisper. Tauri provides `WhisperEngine`; CLI provides `transcribe_chunk`. Both consume `TranscriptionFrame` via a `mpsc::Sender<TranscriptionFrame>` exposed by `PerSourcePipeline::transcribe_sink()`.

**Whisper concurrency:** documented in crate that consumers should run **two transcribe workers** (one per source), each with its own `Arc<WhisperContext>` *or* one shared context with a mutex if `whisper-rs` Metal context is not thread-safe. **Open question, see UX issue:** measure whether `Arc<WhisperContext>` parallelism is real or serialized on Metal.

### 2. macOS clean capture (issue: A1 — backend extraction)

**What to extract:**
- Tauri's CoreAudio Tap implementation in `frontend/src-tauri/src/audio/capture/core_audio.rs` (uses cidre's `with_mono_global_tap_excluding_processes`).
- Mic capture from `frontend/src-tauri/src/audio/capture/microphone.rs`.
- Existing pre-processing (high-pass, RNNoise, EBU R128) — **carry forward as-is**, but make AEC-vs-pre-processing order configurable per (4) below.

**What CLI gains:** clean digital loopback via Core Audio Tap. BlackHole + Multi-Output Device disappear from macOS user setup.

**Permissions (macOS):**
- Mic: `NSMicrophoneUsageDescription` Info.plist key + TCC mic grant.
- Core Audio Tap: requires **macOS 14.2+** AND `NSAudioCaptureUsageDescription` Info.plist key (per Apple's "Capturing system audio with Core Audio taps" sample). **No separate entitlement** is required for unsigned/dev builds; signing requirements may add one and will be revisited if/when shipping signed builds. Document in setup guide.

**Migration:** issue A1 ships only the extracted backend code in `meetily-audio`. Both Tauri and CLI continue to use their existing capture paths until A2.

### 3. WebRTC AEC3 integration (issue: B)

**Crate (v3.2 update):** [`sonora-aec3`](https://crates.io/crates/sonora-aec3) v0.1.0 — pure-Rust port of WebRTC's AEC3, BSD-3, by `dignifiedquire` (iroh / libp2p). Pinned exactly (`=0.1.0`) per PM-M1 review.

**Dependency choice (v3.2 deviation from v3):**

v3 named `tonarino/webrtc-audio-processing`. Build risk #6 ("native build may fail on a target") materialized **before any code was written**:
- `webrtc-audio-processing` 0.3.x (named in v3): default = `pkg-config` + a pre-installed system `webrtc-audio-processing` lib (not on macOS by default; not on most Linux distros either). Its `bundled` feature builds upstream from source but needs `glibtoolize`+`aclocal`+`automake`+`autoconf` — all four missing on a vanilla macOS dev box; equivalent autotools chain needed on Linux.
- `webrtc-audio-processing` 2.0.x (newer upstream API): `bundled` needs `meson` + `ninja` instead. Same fundamental issue: every contributor + CI has to install a system toolchain.
- `sonora-aec3` v0.1.0: probe-built clean in 14 s on M4 with **zero system deps** (just rustc + cargo). Same algorithm — sonora is a port of the same upstream WebRTC AEC3 code.

Trade-off accepted: lose battle-test confidence (years of WebRTC + Chrome usage on the C++ binding) for trivial install on every platform. Single-maintainer crate risk is real but mitigated by (a) recent active commit including a real AEC3 panic fix (April 2026), (b) maintainer credibility, (c) algorithm-in-active-upstream means a fork stays maintainable.

If `sonora-aec3` fails the acceptance bar (#1 or #2 in issue #56), fallback is to vendor `webrtc-audio-processing-sys` and document the brew/apt deps. **Don't pre-build the fallback.**

**Pipeline placement (per source, per frame, v3.2 corrected):**
```
raw mic capture ──► resample to 16kHz ──► AEC3 (mic + system as far-end ref, 4ms blocks) ──► VAD ──► Whisper
raw system capture ──► resample to 16kHz ──► (also feed to AEC3 as far-end via tee) ──► VAD ──► Whisper
```

The AEC reframes internally to its 4 ms / 64-sample blocks (`sonora_aec3::common::BLOCK_SIZE`). Callers pass arbitrary-length 16 kHz mono `&[f32]` slices into `AecPipeline::ingest_render` and `AecPipeline::process_capture`; the wrapper buffers + dispatches.

**Hard parts (v3.2 status):**

1. **Sample rate alignment.** AEC3 requires both near-end and far-end at the same rate. Resample both to 16 kHz before AEC. *(Unchanged from v3.)*
2. ~~**Frame size.** AEC3 processes 10ms frames (160 samples at 16kHz).~~ **Corrected v3.2:** sonora-aec3's `BlockProcessor` is the algorithm-native granularity at **4 ms / 64 samples**. The 10 ms / 160 samples figure was the upper-level WebRTC frame size; we operate one layer below. No reframing distinct from the AEC's internal accumulator is needed.
3. **Far-end delay.** AEC3 has built-in delay estimation (`RenderDelayController`). Optional `AecPipeline::set_delay_hint_ms(100)` for faster initial convergence; sonora-aec3 will refine continuously. *(Unchanged in spirit; method name updated.)*
4. **Drift handling.** Independent capture clocks (mic + system) drift. AEC3 has `clockdrift_detector`; verify behavior on real-world recordings. *(Unchanged from v3.)*
5. **Warm-up.** AEC3 needs ~2–5 s to converge. Documented in `AecPipeline` rustdoc + `AecMetrics`; first seconds of mic transcript may have residual echo. *(Unchanged from v3.)*
6. **Far-end buffering.** If system stream stops, AEC3 should still pass mic through cleanly. Test with intermittent system audio. *(Unchanged from v3.)*
7. **Pre-processing order.** Tauri currently does HPF + RNNoise + R128 on mic at capture. **AEC must run on raw or near-raw mic** — pre-processing distorts the near-end signal vs. far-end echo path. *(Unchanged from v3; relevant only at B2 / Tauri-Unmix.)*

**Aligner (v3.2 correction):** v3 §1 defined a custom `AecPipeline` that owned the aligner and used paired-frame coherent drop. **sonora-aec3's `RenderDelayController` does delay estimation internally**, so no custom aligner exists. The paired-drop guarantee from v3 §1 no longer maps cleanly:

- **Drop policy in B1 CLI integration:** the CLI tees the resampled system stream into the AEC via a bounded mpsc channel. On tee overflow, the **incoming render frame is dropped without dropping capture** (sync_channel `try_send` drops the newest frame on `Full`, not the oldest — preserving up to ~tee-capacity seconds of stale render reference). This desyncs near/far, and `RenderDelayController` re-estimates the delay (~2–5 s reconvergence).
- **Why accepted for v1:** steady state should have no drops; the drop path is the abnormal case. Sustained drop pressure manifests as audible AEC degradation (re-emerging echo), not crashes or wrong output.
- **Diagnosability:** `AecMetrics::render_drops` is a cumulative counter exposed for production debugging. Sustained non-zero values point to drop pressure as the cause of degradation.
- **Followup:** "B1-followup: paired-frame coherent drop via centralized AEC pump" filed as a separate issue. Low priority. Closes if (a) AEC degradation under real load is observed, or (b) we explicitly decide v1 design is good enough.

**Why sonora-aec3 (replaces v3 "Why AEC3"):**
- Same algorithm as Chromium's AEC3 (`getUserMedia({echoCancellation:true})`).
- Pure Rust → trivial cross-platform build (no autotools, no meson, no system deps).
- Active maintainer with recent panic-fix commits.
- macOS `AUVoiceProcessing` rejected: aggressive AGC + ducking until macOS 14, requires mic + speaker share one engine (incompatible with Core Audio Tap system capture).
- DTLN-AEC (neural) deferred — heavier; AEC3 first.


### 4. Tauri pipeline rework (issue: Tauri-Unmix — full migration, NOT just DSP)

**Removes from transcription path:**
- `ProfessionalAudioMixer` invocation at `pipeline.rs:826`.
- Mixed-frame send at `pipeline.rs:835-849` with `DeviceType::Microphone` lie.

**Replaces with:**
- Two parallel VAD chains (mic + system), each emitting `TranscriptionFrame`.
- Two parallel Whisper workers consuming from each chain.
- Interleave merger (mpsc fan-in, sort by timestamp) before UI emission.

**Dependencies / collateral changes (this is what makes it a full migration, not just DSP):**

- `audio/transcription/worker.rs` — currently emits source `"Audio"`. Change to `"mic"` / `"system"` based on `TranscriptionFrame.source()`.
- `audio/recording_state.rs` — currently tracks one transcript stream. Either fan-in or split into two streams.
- `audio/pipeline_manager.rs` (or equivalent) — manages one pipeline; needs to manage two parallel chains.
- `audio/recording_saver.rs` — `TranscriptSegment` struct currently has no `source` field. Add it; persist with each saved segment.
- Tauri events emitted to frontend — `transcript-update` event payload must include `source` field.
- Frontend transcript types: `frontend/src/types/index.ts` (or equivalent) — add `source: "mic" | "system"` to the transcript segment type.
- Frontend transcript rendering (`frontend/src/components/`) — must render with source labels (e.g., distinct colors / left-right alignment per v1 design).
- Backend API: `/api/meetings/{id}/transcript` already accepts `source` per segment (per v1 schema in `design.md`). Verify Tauri uploads use it (currently they may upload `"Audio"`).
- JSON import/export paths: any meeting export feature must include `source`.
- **Per-source VAD warm-up** — each VAD chain (mic, system) maintains independent state. On stop, both chains flush remaining audio independently per the existing CLI VAD force-cut logic. Document that mic and system VAD have separate warm-up windows after start.
- Recording WAV path: per "recording deprioritized," can remain using the mixed signal *via a separate `RecordingMixFrame` path*, OR write two separate WAVs, OR be removed entirely. Type system enforces the mixed signal cannot leak into transcription.

### 5. CLI shutdown UX (issue: UX — independent track)

Independent of the architectural changes. Adds progress counter + second-Ctrl+C abort during the post-recording transcribe drain. Tracked separately.

---

## Transcript interleaving

Both VAD chains emit segments tagged with `(source, start_timestamp_ms, end_timestamp_ms, text)`. A merger:

1. Receives segments from both sources via `mpsc::Receiver<TranscriptSegment>`.
2. **Live mode:** prints each segment as it arrives, prefixed by computed `[YOU]`/`[THEM]` and timestamp. Out-of-order display is OK; each line shows its true start time.
3. **Final mode (on stop):** sorts all collected segments by `start_timestamp_ms`, prints/uploads as a chronological transcript.

CLI already implements (3) at `meetily-client/src/main.rs:200-205`. Tauri needs equivalent merge + sort logic.

---

## Migration plan (revised — split A and B; Tauri-Unmix gates Tauri AEC)

Work item IDs to be assigned by GitHub Issues:

1. **A1 — Define shared types in `meetily-audio`.** `SourceLabel`, `TranscriptionFrame` (private fields + crate-private constructors), `RecordingMixFrame`, `AudioSource` trait (object-safe). No platform code yet. Both Tauri and CLI add the dep but don't consume it. **Acceptance:** `cargo check` passes for both clients; new crate has zero behavioral effect; CI compile-fail test confirms `TranscriptionFrame` cannot be constructed externally.
2. **A2 — Extract pure DSP into `meetily-audio`.** Resampler, VAD wrapper. Both clients keep their existing capture but route through the new DSP types. **Acceptance:** end-to-end transcription quality unchanged on both clients.
3. **A3 — Extract one capture backend into `meetily-audio`.** Start with macOS Core Audio Tap (Tauri's existing implementation). Behind a feature flag. CLI gains the option to use it via `--backend coreaudio`. **Acceptance:** CLI can record on macOS without BlackHole when `--backend coreaudio` is set; existing CLI behavior unchanged when flag absent.
4. **A4 — Make Core Audio Tap default for CLI on macOS.** Drop BlackHole from setup docs. **Acceptance:** README diff removes BlackHole + Multi-Output Device steps.
5. **B1 — Add AEC3 to CLI per-source pipeline.** New `AecPipeline` stage in `meetily-audio` between capture and VAD; wire into CLI only. CLI is already per-source, so AEC fits cleanly. **Depends on A3.** **Acceptance:** see acceptance criteria below; verified on CLI.
6. **Tauri-Unmix — Remove pre-mix from Tauri transcription.** Two parallel VAD+Whisper chains; interleave merger; preserve `[YOU]`/`[THEM]` labels in UI; update transcription worker, recording state, recording saver, frontend types, frontend rendering, API uploads. **Acceptance:** Tauri transcript JSON contains per-segment `source` field; UI shows per-source labels; AEC stage NOT yet present in Tauri (still vulnerable to acoustic bleed but no longer mixing).
7. **B2 — Add AEC3 to Tauri per-source pipeline.** Wire `AecPipeline` into Tauri after Tauri-Unmix has landed. **Depends on Tauri-Unmix and B1.** **Acceptance:** Tauri matches CLI on the bleed-reduction acceptance criterion.
8. **#1 — User-visible bug** (CLI duplicate transcripts) closes when CLI passes the dedup acceptance criterion (covered by A3 + B1).
9. **UX — Streaming shutdown drain** — independent, can land in parallel.

Order: A1 → A2 → A3 → A4 → B1 → Tauri-Unmix → B2. UX in parallel anytime.

**Why this order:** B1 depends on A3 (CLI needs the new backend extracted to wire AEC). Tauri-Unmix MUST land before B2 — wiring AEC into Tauri while Tauri still pre-mixes would leave it shipping mixed-with-AEC transcription, which is architecturally wrong (AEC output goes through the mixer, defeating per-source labels). Each step ships independently and `cargo check` passes for both clients after each.

---

## Acceptance criteria (rolled up)

### Architectural (gating)

- **No mixing in any transcription path.** Verified by:
  1. Type system: `TranscriptionFrame` has private fields and only crate-private constructors (`from_mic_capture`, `from_system_capture`); external code cannot construct a `TranscriptionFrame` from arbitrary mixed samples. **Compile-fail test in CI** confirms this (a test that tries to construct `TranscriptionFrame { source: Mic, samples: vec![], timestamp_ms: 0 }` from outside the crate must fail to compile).
  2. `RecordingMixFrame` is a distinct type with no impl converting it to `TranscriptionFrame`.
  3. Code grep as a fallback check.
- **Per-source labels preserved end-to-end** in API responses, JSON dumps, UI events, and frontend rendering.
- **Final transcript interleaved by timestamp.** Output verified chronological for a recording where mic and system have alternating speech (test fixture).

### Functional (measurable)

- **Acoustic bleed reduction** (replaces "zero duplicates"):
  - Baseline measurement: record 30s of system audio + speak into mic; count duplicate segment pairs (`[YOU]` segment matching `[THEM]` segment within ±500ms with ≥0.85 token overlap).
  - Post-AEC target: ≥80% reduction in duplicate pairs vs baseline.
  - Synthetic test: ERLE (Echo Return Loss Enhancement) ≥ 20dB measured on a controlled test file (system audio = white noise; mic input = system audio attenuated by -10dB to simulate acoustic path).
  - Real-speaker acceptance: human reviewer confirms transcript quality on a 2-minute recording; subjective rating "no significant duplicates" on a yes/no scale.
- **macOS user setup simplified.** README diff: lines removed for BlackHole + Multi-Output Device steps. New macOS users can record without third-party audio drivers.
- **Both clients depend on `meetily-audio` crate.** `Cargo.toml` of `meetily-client` and `frontend/src-tauri` both declare the dependency.

### Non-regression

- Existing transcription quality (without AEC for headphone users) does not degrade. Measured by comparing transcript token-error-rate against a 5-minute reference recording before/after pipeline changes.
- CPU usage on M-series Macs increases by < 5% for the AEC stage at 16kHz, both streams active.

---

## Open questions

1. **Whisper context sharing across two parallel chains.** Today CLI shares one `Arc<WhisperContext>`. Whether `whisper-rs` serializes on Metal context is a known unknown — relevant for both old and new pipeline, but more visible with parallel chains. Investigation tracked in UX issue. **Resolution path:** measure single vs. concurrent transcribe timing; if serialized, decide between (a) two separate contexts, or (b) one context with explicit serialization.
2. **AEC3 cross-platform build.** `webrtc-audio-processing` requires cmake/meson/ninja or a system package. Verify it builds on M-series macOS (developer machines), Ubuntu (CI / M1 server), and Windows (some CLI users). Risk: native build fails on a target → fall back to optional AEC bypass.
3. **Latency budget.** AEC3 adds 10–30ms processing latency. Acceptable for transcription pipelines (Whisper is the dominant latency). Validate empirically per platform.
4. **AEC bypass for headphone users.** Probably worth a `--no-aec` CLI flag and a Tauri toggle. Tracked as future issue. Default: AEC on.
5. **Drop policy under high load.** Bounded channels with drop-oldest may lose audio under sustained pressure. Decide: silent drop (warning log only) vs. surface to user via UI? v1: silent drop with metric.

---

## Industry research (toned down per codex feedback)

Publicly documented approaches in adjacent products:

- **Granola** (Mac AI meeting note-taker): documents source-separated transcription (`[Me]` / `[Them]` labels) and warns against mic-as-output configurations that prevent capturing participants' audio separately. [Granola docs](https://docs.granola.ai/help-center/taking-notes/transcription). Specific capture backend not publicly disclosed.
- **Hyprnote** (open-source Granola alternative): public architecture documentation indicates Core Audio Tap on macOS. [Hyprnote audio architecture](https://deepwiki.com/fastrepl/hyprnote/4.1-audio-input-and-output-system).
- **Recall.ai**: documents that native macOS APIs (SCK, Electron, AVFoundation) do not include built-in echo cancellation; their SDK addresses this. [Recall.ai SCK guide](https://www.recall.ai/blog/macos-screencapture-api). Specific AEC implementation not disclosed.
- **WebRTC AEC3**: open-source reference AEC, used in Chromium for `getUserMedia({echoCancellation:true})`. [Chrome blog: macOS native echo cancellation](https://developer.chrome.com/blog/macos-native-echo-cancellation).
- **DTLN-AEC** (neural AEC, deferred): [breizhn/DTLN-aec paper](https://arxiv.org/pdf/2010.14337.pdf). Placed 3rd at ICASSP-2021 AEC Challenge.

The pattern across these products is: per-source capture, OS-native or carefully chosen system-audio backend, AEC for acoustic bleed. Specific implementation choices vary and are often not disclosed.

---

## References

- v1 architecture doc: [[design]] in this folder.
- Existing Meetily code (verified 2026-05-08):
  - Tauri pre-mix: `frontend/src-tauri/src/audio/pipeline.rs:145-188, 823-849` (mixer + transcription send).
  - Tauri macOS capture (CoreAudio Tap, default): `frontend/src-tauri/src/audio/capture/core_audio.rs:91`.
  - Tauri backend selection: `frontend/src-tauri/src/audio/capture/backend_config.rs:79` (default: CoreAudio).
  - Tauri transcription worker (hardcoded `"Audio"` source): `frontend/src-tauri/src/audio/transcription/worker.rs`.
  - CLI per-source streaming: `meetily-client/src/main.rs:135-217`.
  - CLI BlackHole capture: `meetily-client/src/audio/capture.rs:302-306, 377-381`.
  - CLI conditional system capture: `meetily-client/src/main.rs` Record arm — system stream only if `--system` provided.
