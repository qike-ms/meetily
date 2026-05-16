# Meetily v4 research: macOS meeting capture, transcription, diarization

Date: 2026-05-12
Author: codex
Project: Meetily
Scope: research and design only; no code changes.
Requested destination was `/Users/qike/git/obsidian-vault/projects/meetily/RESEARCH-codex.md`, but this session is sandboxed to write only under `/Users/qike/git/meetily`, `/private/tmp`, and related temp roots. This copy is saved at `/Users/qike/git/meetily/RESEARCH-codex.md`.

## Executive summary

- The closest analog is Granola Desktop, not Zoom/Teams/Meet.
- Granola Desktop captures microphone plus system audio, labels the two rails as `Me` and `Them`, uses real-time cloud transcription, and explicitly does not do live desktop diarization.
- Zoom, Microsoft Teams, and Google Meet avoid most laptop-level diarization because they own the conference transport and can attach text/audio to participant identities before or during media processing.
- A local app that is not in the meeting protocol cannot generally get true per-participant RTP/WebRTC streams.
- For Meetily v4, use a signed/notarized macOS app with a stable bundle ID, explicit permission preflight, Core Audio process taps on macOS 14.2+ with ScreenCaptureKit fallback, and a unified source-aware ASR scheduler.
- Keep two raw capture rails (`mic`, `system`) as evidence.
- Throw away the current two-independent-Whisper-workers-plus-posthoc-text-dedup architecture.
- Replace post-ASR dedup with audio-domain segmentation/arbitration before ASR.
- The bug Qi hit is consistent with a packaging/TCC/session failure mode: an unbundled CLI launched from different parent contexts can receive different macOS privacy attribution and silently fail system audio capture.
- A bundled app with proper Info.plist usage strings and hard system-audio health checks should not enter a meeting with an all-zero tap.

## Primary sources

- Granola transcription docs: https://docs.granola.ai/help-center/taking-notes/transcription
- Granola privacy/security FAQ: https://docs.granola.ai/help-center/consent-security-privacy/security-privacy-data-faqs
- Granola troubleshooting: https://docs.granola.ai/help-center/troubleshooting/transcription-issues
- Apple Core Audio taps sample: https://developer.apple.com/documentation/coreaudio/capturing-system-audio-with-core-audio-taps
- Apple `AudioHardwareCreateProcessTap`: https://developer.apple.com/documentation/coreaudio/audiohardwarecreateprocesstap%28_%3A_%3A%29
- Apple ScreenCaptureKit: https://developer.apple.com/documentation/screencapturekit
- Apple ScreenCaptureKit sample: https://developer.apple.com/documentation/screencapturekit/capturing_screen_content_in_macos
- Zoom local recording docs: https://support.zoom.com/hc/en/article?id=zm_kb&sysparm_article=KB0076922
- Zoom transcription docs: https://support.zoom.com/hc/en/article?id=zm_kb&sysparm_article=KB0064927
- Zoom auto captions blog: https://www.zoom.com/en/blog/zoom-auto-generated-captions/
- Teams transcription support: https://support.microsoft.com/en-us/office/start-stop-and-download-live-transcripts-in-microsoft-teams-meetings-dc1a8f23-2e20-4684-885e-2152e06a4a8b
- Teams recording/transcription overview: https://learn.microsoft.com/en-us/microsoftteams/recording-transcription-overview
- Teams real-time media bots: https://learn.microsoft.com/en-us/microsoftteams/platform/bots/calls-and-meetings/real-time-media-concepts
- Teams call flows: https://learn.microsoft.com/en-us/microsoftteams/microsoft-teams-online-call-flows
- Teams intelligent speaker recognition: https://support.microsoft.com/en-us/office/use-microsoft-teams-intelligent-speakers-to-identify-in-room-participants-in-a-meeting-transcription-a075d6c0-30b3-44b9-b218-556a87fadc00
- Google Meet transcripts: https://support.google.com/meet/answer/12849897
- Google Meet transcript entries API: https://developers.google.com/workspace/meet/api/reference/rest/v2/conferenceRecords.transcripts.entries
- Google Meet Media API virtual streams: https://developers.google.com/meet/media-api/guides/virtual-streams
- Google Meet captions: https://support.google.com/meet/answer/15077804
- Google Meet noise cancellation: https://support.google.com/meet/answer/9919960
- Google Workspace echo cancellation: https://workspace.google.com/resources/echo-cancellation/
- whisper.cpp: https://github.com/ggml-org/whisper.cpp
- faster-whisper: https://github.com/SYSTRAN/faster-whisper
- WhisperStreaming: https://github.com/ufal/whisper_streaming
- pyannote speaker diarization: https://huggingface.co/pyannote/speaker-diarization
- pyannote model overview: https://huggingface.co/pyannote
- NVIDIA NeMo diarization docs: https://docs.nvidia.com/nemo-framework/user-guide/latest/nemotoolkit/asr/speaker_diarization/intro.html
- NVIDIA Sortformer model card: https://huggingface.co/nvidia/diar_sortformer_4spk-v1
- sherpa-onnx: https://github.com/k2-fsa/sherpa-onnx
- sherpa-onnx diarization docs: https://k2-fsa.github.io/sherpa/onnx/speaker-diarization/index.html
- Recall.ai meeting bot API: https://www.recall.ai/product/meeting-bot-api
- Recall.ai bot overview: https://docs.recall.ai/docs/bot-overview
- MeetingBaaS license: https://www.meetingbaas.com/license
- Vexa GitHub: https://github.com/Vexa-ai/vexa
- Vexa pricing/license page: https://vexa.ai/pricing
- RecordKit system audio docs: https://nonstrict.eu/recordkit/guides/system-audio-recording.html
- insidegui AudioCap: https://github.com/insidegui/AudioCap
- AudioCap Recorder docs: https://chrisns.github.io/audiocap-recorder/docs/QuickStart/
- Rust `screencapturekit` crate docs: https://docs.rs/screencapturekit
- `systemAudioDump`: https://github.com/sohzm/systemAudioDump

---

# Section A: Competitive research

## A.1 Comparison table

| Tool | Capture topology | macOS system-audio capture | Echo cancellation | Transcription | Diarization | Lesson for Meetily |
|---|---|---|---|---|---|---|
| Granola Desktop | Local two-source capture: microphone plus combined system output. | Public docs do not name the API. Docs mention system audio, Screen & System Audio permission, and macOS 14.2+ preference; Core Audio tap plus ScreenCaptureKit fallback is a strong inference, not confirmed. | Mostly sidesteps by using source labels; speaker echo can still leak into mic, and docs recommend headset where appropriate. | Cloud real-time transcription provider; docs say audio is not stored as a recording and temporary cache is deleted. | No live desktop diarization; `Me` and `Them` correspond to microphone and system audio. | Closest analog: source-derived `Me`/`Them` is production-proven; per-person live diarization is not. |
| Zoom | Native conferencing client with participant-associated media; recordings can output separate audio per participant. | Not needed for normal remote participant transcription because Zoom receives meeting media directly. | Built-in/proprietary AEC and audio processing. | Cloud captions/live transcription; cloud recording transcript; local recordings can save media files. | Mostly participant identity metadata; separate participant files are available. | If you own transport, diarization is metadata. Meetily does not. |
| Microsoft Teams | Protocol/service media with participants, active-speaker concepts, and media-bot frame access. | Not needed for Teams' own transcription; media flows through Teams/Microsoft 365. | Teams proprietary audio stack with AEC/noise suppression. | Cloud real-time transcript and post-meeting transcript. | Speaker names via participant identity; in-room recognition via voice profiles/Intelligent Speakers. | Service identity beats acoustic diarization. |
| Google Meet | WebRTC/SFU; transcript entries include participant references; Media API exposes virtual streams/CSRC source metadata. | Not needed for Meet's own transcripts/captions. | Google Meet proprietary/browser audio processing with AEC/noise cancellation/adaptive audio. | Cloud live captions; cloud transcripts saved to Drive; API exposes transcript entries. | Mostly service/protocol attribution. | Platform transcripts are best when available, but not universal/local/privacy-first. |

## A.2 Granola Desktop

### A.2.1 What Granola says it captures

- Granola says Desktop uses microphone and system audio for transcription.
- Source: https://docs.granola.ai/help-center/taking-notes/transcription
- Granola says there is no meeting bot.
- Source: https://docs.granola.ai/help-center/consent-security-privacy/security-privacy-data-faqs
- Granola says the app runs locally on the device and captures directly from microphone and system audio.
- Source: https://docs.granola.ai/help-center/consent-security-privacy/security-privacy-data-faqs
- Granola says it captures whatever audio inputs and outputs happen on the computer.
- Source: https://docs.granola.ai/help-center/taking-notes/transcription
- Granola warns that unrelated audio, such as music, can be included if it plays during a meeting.
- Source: https://docs.granola.ai/help-center/taking-notes/transcription
- Granola says it cannot isolate audio from individual applications.
- Source: https://docs.granola.ai/help-center/taking-notes/transcription
- Granola troubleshooting says the user's default audio devices must match the meeting app's devices.
- Source: https://docs.granola.ai/help-center/troubleshooting/transcription-issues
- Granola can transcribe Zoom, Meet, Teams, Slack, VoIP, in-person conversations, voice memos, and arbitrary computer audio.
- Source: https://docs.granola.ai/help-center/getting-started/setting-up-granola-for-the-first-time

### A.2.2 Capture topology

- Topology: local source split.
- Rail 1: microphone input.
- Rail 2: system output.
- It is not a single opaque mixed mono stream.
- It is not true per-participant stream capture.
- It is not a meeting-protocol bot.
- It is not extracting Zoom/Meet/Teams participant RTP.
- Evidence: Granola says Desktop transcript shows `Me` and `Them` corresponding to microphone input and system audio.
- Source: https://docs.granola.ai/help-center/taking-notes/transcription
- This is source-derived role labeling, not diarization.
- `Me` equals the local mic rail.
- `Them` equals the system-output rail.
- If two remote participants speak, both remain `Them`.
- If YouTube or Spotify plays, that audio can also land in `Them`.
- If laptop speakers leak into the mic, the mic rail contains echo.
- Granola's headset recommendation is consistent with this topology.
- Source: https://docs.granola.ai/help-center/taking-notes/transcription

### A.2.3 System-audio capture mechanism on macOS

- Granola public docs do not name Core Audio Tap, ScreenCaptureKit, or a virtual driver.
- Granola troubleshooting references macOS Microphone and Screen & System Audio recording permissions.
- Source: https://docs.granola.ai/help-center/troubleshooting/transcription-issues
- Granola says macOS 13+ is required and 14.2+ works best.
- Source: https://docs.granola.ai/help-center/troubleshooting/transcription-issues
- Apple Core Audio process taps are the modern native API for outgoing process/system audio on macOS 14.2+.
- Source: https://developer.apple.com/documentation/coreaudio/capturing-system-audio-with-core-audio-taps
- Apple's tap sample requires `NSAudioCaptureUsageDescription` and prompts for system audio recording permission.
- Source: https://developer.apple.com/documentation/coreaudio/capturing-system-audio-with-core-audio-taps
- Apple ScreenCaptureKit can capture system audio and microphone buffers.
- Source: https://developer.apple.com/documentation/screencapturekit/capturing_screen_content_in_macos
- RecordKit documents a practical backend split: Core Audio on 14.2+ and ScreenCaptureKit on older supported macOS versions.
- Source: https://nonstrict.eu/recordkit/guides/system-audio-recording.html
- Strong inference: Granola probably uses native system audio APIs, likely Core Audio taps where available and possibly ScreenCaptureKit fallback.
- This is inference only.
- I found no evidence Granola installs BlackHole/Soundflower-style drivers.
- I found no evidence Granola pulls WebRTC/RTP streams from meeting protocols.

### A.2.4 Echo cancellation

- Granola Desktop does not publicly claim AUVoiceProcessingIO, WebRTC AEC3, or an in-house echo model.
- Granola's public design mostly sidesteps far-end echo by treating system output as `Them` and microphone as `Me`.
- This does not eliminate acoustic leakage from laptop speakers into the mic.
- Granola recommends headset use where appropriate.
- Source: https://docs.granola.ai/help-center/taking-notes/transcription
- Granola troubleshooting warns about mixers/pass-through setups confusing input/output routing.
- Source: https://docs.granola.ai/help-center/troubleshooting/transcription-issues
- Lesson: source split is a useful echo workaround, but not a full AEC solution.
- Lesson: text dedup should not be the primary echo-control layer.
- Lesson: capture correctness and audio-domain gating must happen before ASR.

### A.2.5 Transcription

- Granola says it passes microphone and system audio directly to a transcription provider.
- Source: https://docs.granola.ai/help-center/taking-notes/transcription
- Granola Desktop uses real-time transcription.
- Source: https://docs.granola.ai/help-center/taking-notes/transcription
- Granola says it does not record or save meeting audio as an accessible artifact.
- Source: https://docs.granola.ai/help-center/taking-notes/transcription
- Granola privacy FAQ says temporary audio cache is deleted after transcription.
- Source: https://docs.granola.ai/help-center/consent-security-privacy/security-privacy-data-faqs
- This is cloud ASR, not local Whisper.
- Granola is a capture-topology reference, not a local runtime reference.

### A.2.6 Diarization

- Granola explicitly says Desktop does not support live diarization today.
- Source: https://docs.granola.ai/help-center/taking-notes/transcription
- Granola says only iPhone can recognize different speakers in face-to-face meetings.
- Source: https://docs.granola.ai/help-center/taking-notes/transcription
- Desktop labels are `Me` and `Them` from source rails.
- This is the most important competitive finding.
- The closest production analog to Meetily does not solve full live desktop diarization.
- It ships source-derived roles and defers per-person diarization.

### A.2.7 Lesson for Meetily

- Granola validates local mic plus system capture for a no-bot desktop app.
- Granola validates `Me`/`Them` as a useful v1/v4 abstraction.
- Granola also shows that a single-laptop tool cannot isolate participants without protocol access or diarization.
- Meetily should not promise per-participant names from a system-output mix.
- Meetily should make source labels robust first.
- Meetily should keep raw rails for optional later diarization.
- Meetily should not rely on terminal-launched CLIs for production macOS audio permissions.
- Meetily should fail loudly when system audio returns all-zero samples while user expects playback capture.

## A.3 Zoom

### A.3.1 Capture topology

- Zoom is the meeting client/service.
- Zoom participates in the protocol and knows participants.
- Zoom local recording can save a separate audio file for each participant.
- Source: https://support.zoom.com/hc/en/article?id=zm_kb&sysparm_article=KB0076922
- Current Zoom cloud recording settings also document separate participant audio-only files.
- Source: https://support.zoom.com/hc/en/article?ampDeviceId=e2281521-3d7b-49d9-b35b-833ffd6c7c80&ampSessionId=undefined&id=zm_kb&sysparm_article=KB0064676
- Therefore Zoom has participant-associated audio before or during recording.
- The practical topology is participant-associated streams, not laptop speaker capture.

### A.3.2 System-audio capture mechanism on macOS

- For normal meeting transcription and recording, Zoom does not need to capture macOS system output.
- Zoom already receives remote audio as meeting media.
- I did not find authoritative public docs tying Zoom transcription to Core Audio Tap, ScreenCaptureKit, or virtual drivers on macOS.
- System audio capture may exist for `share computer audio` or local routing features, but it is not the core path for participant transcription.
- For Meetily this means Zoom's clean speaker labels are not evidence that a local recorder can get those labels.

### A.3.3 Echo cancellation

- Zoom has built-in echo cancellation/audio processing.
- Source: https://support.zoom.com/hc/en/article?id=zm_kb&sysparm_article=KB0061720
- Zoom SDK/App SDK surfaces echo-cancellation-related controls.
- Source: https://appssdk.zoom.us/types/ZoomSdkTypes.SetAudioSettingsOptions.html
- Zoom Developer Forum confirms Web SDK AEC exists.
- Source: https://devforum.zoom.us/t/acoustic-echo-cancellation/12583
- Exact algorithms are proprietary.

### A.3.4 Transcription

- Zoom provides auto-generated captions/live transcription.
- Source: https://www.zoom.com/en/blog/zoom-auto-generated-captions/
- Zoom cloud recording transcription produces transcript files such as VTT after processing.
- Source: https://support.zoom.com/hc/en/article?id=zm_kb&sysparm_article=KB0064927
- This is service/cloud ASR, not local Whisper.

### A.3.5 Diarization

- Zoom can identify speakers because participants are known to the platform.
- Separate local recording files are named by participant.
- Source: https://support.zoom.com/hc/en/article?id=zm_kb&sysparm_article=KB0076922
- This is not pyannote-style acoustic diarization.
- It is participant metadata carried by the meeting pipeline.

### A.3.6 Lesson for Meetily

- Zoom proves the best diarization is upstream identity, not downstream embeddings.
- Meetily cannot copy this without becoming a bot/platform client/API integration.
- For privacy-first local transcription, do not make Zoom-like per-person diarization a v4 requirement.
- If Meetily later offers platform-specific modes, Zoom APIs/bot paths could add true names.

## A.4 Microsoft Teams

### A.4.1 Capture topology

- Teams media uses ICE, RTP/SRTP, relays, and Microsoft 365 endpoints.
- Source: https://learn.microsoft.com/en-us/microsoftteams/microsoft-teams-online-call-flows
- Teams real-time media bots can access raw media frames.
- Source: https://learn.microsoft.com/en-us/microsoftteams/platform/bots/calls-and-meetings/real-time-media-concepts
- The bot docs discuss active speakers and media frames.
- Source: https://learn.microsoft.com/en-us/microsoftteams/platform/bots/calls-and-meetings/real-time-media-concepts
- Teams therefore has participant/active-speaker metadata at the service/media layer.
- Normal Teams transcription is service-side and participant-aware.

### A.4.2 System-audio capture mechanism on macOS

- For Teams' own meeting transcription, macOS system-output capture is not the mechanism.
- Teams receives meeting media through its client/service stack.
- I found no evidence Teams live transcription depends on ScreenCaptureKit, Core Audio Tap, or virtual audio drivers.

### A.4.3 Echo cancellation

- Teams includes audio processing, echo cancellation, and noise suppression features.
- Source: https://techcommunity.microsoft.com/blog/microsoftteamsblog/microsoft-teams-leads-in-audio-quality-echo-cancellation-and-noise-suppression-p/4416095
- Microsoft has separate Speech/Microsoft Audio Stack AEC docs, but those are Windows Speech features and not necessarily Teams-on-macOS internals.
- Source: https://learn.microsoft.com/en-us/azure/ai-services/speech-service/audio-processing-model-based-echo-cancellation
- Treat Teams AEC as proprietary product audio processing.

### A.4.4 Transcription

- Teams live transcription appears in real time and includes speaker name and timestamp.
- Source: https://support.microsoft.com/en-us/office/start-stop-and-download-live-transcripts-in-microsoft-teams-meetings-dc1a8f23-2e20-4684-885e-2152e06a4a8b
- Teams recording and transcription are governed by policies.
- Source: https://learn.microsoft.com/en-us/microsoftteams/recording-transcription-overview
- Teams transcripts can be downloaded after the meeting depending on permissions and policies.
- Source: https://support.microsoft.com/en-us/office/start-stop-and-download-live-transcripts-in-microsoft-teams-meetings-dc1a8f23-2e20-4684-885e-2152e06a4a8b
- This is cloud/service transcription.

### A.4.5 Diarization

- Teams transcription includes speaker names for remote participants.
- Source: https://support.microsoft.com/en-us/office/start-stop-and-download-live-transcripts-in-microsoft-teams-meetings-dc1a8f23-2e20-4684-885e-2152e06a4a8b
- Teams Rooms can identify in-room participants using Intelligent Speakers and voice profiles.
- Source: https://support.microsoft.com/en-us/office/use-microsoft-teams-intelligent-speakers-to-identify-in-room-participants-in-a-meeting-transcription-a075d6c0-30b3-44b9-b218-556a87fadc00
- Without speaker recognition, room audio may be attributed to the room.
- Source: same Teams Intelligent Speakers doc.
- This is metadata plus proprietary speaker recognition, not generic local diarization.

### A.4.6 Lesson for Meetily

- Teams validates the hierarchy: protocol identity beats acoustic diarization.
- Teams also shows in-room attribution needs voice profiles or special hardware.
- Meetily should treat per-person local diarization as optional batch enhancement.
- For local privacy, do not depend on Graph/Teams cloud.

## A.5 Google Meet

### A.5.1 Capture topology

- Google Meet is WebRTC-based.
- Source: https://developers.google.com/meet/media-api/guides/concepts
- Meet Media API docs describe SFU virtual streams.
- Source: https://developers.google.com/meet/media-api/guides/virtual-streams
- The docs say CSRC identifies the true source of RTP packets.
- Source: https://developers.google.com/meet/media-api/guides/virtual-streams
- Meet transcript entries include a `participant` field referring to the speaker.
- Source: https://developers.google.com/workspace/meet/api/reference/rest/v2/conferenceRecords.transcripts.entries

### A.5.2 System-audio capture mechanism on macOS

- For Meet captions/transcripts, macOS system-output capture is not the mechanism.
- Meet attaches transcripts to participants because it owns the conference/session layer.
- ScreenCaptureKit/Core Audio Tap are relevant to third-party recorders outside Meet, not Meet itself.

### A.5.3 Echo cancellation

- Google Meet offers noise cancellation and echo-related audio improvements.
- Source: https://support.google.com/meet/answer/9919960
- Google Workspace has a Meet echo-cancellation explainer.
- Source: https://workspace.google.com/resources/echo-cancellation/
- Meet's AEC is proprietary product behavior built on browser/device audio infrastructure.
- It helps the meeting audio, but does not remove remote audio leaking into a separate local mic recording by a third-party app.

### A.5.4 Transcription

- Meet live captions are available during meetings.
- Source: https://support.google.com/meet/answer/15077804
- Meet transcripts can be saved to the organizer's Google Drive.
- Source: https://support.google.com/meet/answer/12849897
- Meet transcript entries are exposed through Google Meet API resources.
- Source: https://developers.google.com/workspace/meet/api/reference/rest/v2/conferenceRecords.transcripts.entries
- This is cloud/service transcription.

### A.5.5 Diarization

- Meet transcript entries contain participant references.
- Source: https://developers.google.com/workspace/meet/api/reference/rest/v2/conferenceRecords.transcripts.entries
- Meet Media API CSRC/source metadata connects RTP packets to source participants.
- Source: https://developers.google.com/meet/media-api/guides/virtual-streams
- Meet's speaker attribution is therefore primarily service/protocol attribution.

### A.5.6 Lesson for Meetily

- Meet shows the best speaker-name mode is platform integration or bot/media API.
- Google's Media API is not a universal local desktop solution.
- Browser caption scraping can be platform-specific and brittle.
- Build platform-independent local audio first.

## A.6 Cross-tool conclusions

- Production conferencing apps either own the media protocol or run local desktop capture.
- Zoom/Teams/Meet get participant identity before mixing.
- Granola cannot, so it uses mic/system role labels.
- Meetily is in the Granola category.
- `Me`/`Them` is a production-proven compromise.
- Per-participant local diarization is a separate optional feature.
- A single mixed stream makes capture simpler but makes attribution much harder.
- True per-participant streams require bot/API/protocol integration.
- A local privacy-first app should avoid bot infrastructure by default.
- A local privacy-first app should avoid cloud transcription by default.
- Local source-separated capture plus local ASR is the right v4 foundation.
- Current Meetily's post-ASR text dedup is not the right foundation.
- Silent zero system taps must be capture failures, not silence.
- Permission identity must be app-bundle-stable.

---

# Section B: OSS landscape

## B.1 Summary table

| Project | License | Language/runtime | Solves | Fit for Meetily v4 |
|---|---:|---|---|---|
| whisper.cpp | MIT | C/C++; Rust via whisper-rs; Metal/Core ML on Apple Silicon | Local Whisper inference | Keep, but wrap in streaming policy and unified scheduler. |
| faster-whisper | MIT | Python; CTranslate2; CUDA/CPU strongest | Faster Whisper inference | Benchmark/reference; less ideal inside native Mac Rust app. |
| whisper_streaming | MIT | Python | Real-time policy for Whisper-like models | Borrow local-agreement ideas; repo itself says newer SimulStreaming supersedes it. |
| pyannote-audio | MIT code/models vary/gated | Python/PyTorch | Offline diarization | Optional batch pass, not live v4 core. |
| NVIDIA NeMo / Sortformer | NeMo code; public model CC-BY-NC-4.0 | Python/PyTorch/NVIDIA | Streaming/offline diarization | Research-only due license/GPU footprint. |
| sherpa-onnx | Apache-2.0 | C++/ONNX Runtime; Rust/Swift/etc. | Local ASR/VAD/diarization/speaker ID | Strong candidate for VAD/diarization experiments; benchmark ASR. |
| sherpa-ncnn | Apache-2.0 | C/ncnn | Lightweight local ASR | Interesting embedded path; not obvious default for Mac meetings. |
| Recall.ai | Closed SaaS/API | Cloud bot/SDK | Meeting bot capture/per-participant streams | Useful contrast; not OSS/local. |
| MeetingBaaS | BSL for core | Bot platform | Self-hostable meeting bots | Not permissive v4 primitive; future bot reference. |
| Vexa | Apache-2.0 | TS/Python/C++/browser automation | Self-hosted meeting bot/transcription | Good future bot-mode reference, not local capture. |
| AudioCap | BSD-2-Clause | Swift/Core Audio taps | macOS process/system audio sample | High-value capture reference. |
| RecordKit | License unclear from docs | Swift/Electron SDK | Recording abstraction | Design reference; license review before use. |
| Rust `screencapturekit` | MIT/Apache | Rust/Apple frameworks | Screen/system audio bindings | Strong fallback candidate. |
| systemAudioDump | MIT | Swift/ScreenCaptureKit | System-audio-to-PCM demo | Useful sanity reference, not product permission model. |

## B.2 whisper.cpp

- URL: https://github.com/ggml-org/whisper.cpp
- License: MIT.
- Language: C/C++.
- Meetily already uses it through whisper-rs.
- Strength: local offline ASR.
- Strength: Apple Silicon support through Metal/Core ML/Accelerate paths.
- Strength: simpler deployment than Python/PyTorch.
- Strength: quantized models reduce memory.
- Weakness: Whisper is not natively streaming.
- Weakness: naive chunking causes repeated text and hallucinations.
- Weakness: two independent large-model workers are expensive and can create inconsistent context.
- Weakness: word timestamps and VAD require careful tuning.
- Verdict: keep as default local ASR runtime.
- v4 change: one ASR scheduler consumes source-tagged speech windows.
- v4 change: centralize segmentation, prompts, context, and backpressure.
- v4 change: avoid treating each source as a separate meeting transcript.

## B.3 faster-whisper

- URL: https://github.com/SYSTRAN/faster-whisper
- License: MIT.
- Runtime: CTranslate2.
- Strength: faster/lower-memory than OpenAI Whisper in common benchmarks.
- Strength: VAD filter integration with Silero VAD.
- Strength: rich community ecosystem.
- Weakness: best acceleration is CUDA, not Apple Metal.
- Weakness: Python sidecar increases native app complexity.
- Weakness: not diarization by itself.
- Fit: benchmark/reference backend.
- Recommendation: benchmark against whisper.cpp on target Macs before switching.

## B.4 whisper_streaming

- URL: https://github.com/ufal/whisper_streaming
- License: MIT.
- Language: Python.
- Problem: Whisper is not designed for real-time transcription.
- Approach: local-agreement policy with adaptive latency.
- The README says the project is becoming outdated and replaced by SimulStreaming.
- Strength: directly relevant to Meetily's streaming gap.
- Weakness: Python and backend churn.
- Fit: borrow algorithmic ideas, not necessarily code.

## B.5 pyannote-audio

- URLs: https://huggingface.co/pyannote and https://huggingface.co/pyannote/speaker-diarization
- Code/model status: pyannote.audio is open; model cards vary and can require Hugging Face condition acceptance.
- Runtime: Python/PyTorch.
- Problem solved: speaker diarization, segmentation, VAD, overlap handling.
- Strength: de facto diarization baseline.
- Strength: strong benchmark reporting.
- Weakness: large Python/PyTorch dependency.
- Weakness: model access tokens/terms complicate bundled app distribution.
- Weakness: labels are anonymous clusters, not names.
- Weakness: echo-heavy laptop recordings remain hard.
- Fit: optional offline post-meeting diarization.
- Not v4 live core.

## B.6 NVIDIA NeMo / Sortformer

- Docs: https://docs.nvidia.com/nemo-framework/user-guide/latest/nemotoolkit/asr/speaker_diarization/intro.html
- Model card: https://huggingface.co/nvidia/diar_sortformer_4spk-v1
- NeMo docs describe end-to-end Sortformer and streaming Sortformer variants.
- Public Sortformer model card license is CC-BY-NC-4.0.
- Runtime: Python/PyTorch, NVIDIA ecosystem.
- Strength: modern real-time diarization research direction.
- Weakness: non-commercial model license blocks product embedding.
- Weakness: GPU/PyTorch footprint is poor for a local Mac app.
- Fit: research-only for v4.

## B.7 sherpa-onnx / sherpa-ncnn

- sherpa-onnx URL: https://github.com/k2-fsa/sherpa-onnx
- License: Apache-2.0.
- Runtime: C++/ONNX Runtime.
- Bindings: C, C++, Python, JavaScript, Java, C#, Kotlin, Swift, Go, Dart, Rust, Pascal.
- Supported tasks include ASR, TTS, VAD, diarization, speaker ID, speaker verification, punctuation, source separation, and more.
- Diarization docs: https://k2-fsa.github.io/sherpa/onnx/speaker-diarization/index.html
- Strength: permissive license.
- Strength: offline operation.
- Strength: Rust/Swift-friendly.
- Weakness: ASR quality must be benchmarked against Whisper for English meetings.
- Weakness: ONNX Runtime packaging size matters.
- Fit: strong candidate for local VAD and diarization experiments.
- sherpa-ncnn is Apache-2.0 and more embedded/mobile-oriented.
- Fit for ASR replacement: only after benchmarking.

## B.8 Recall.ai

- URL: https://www.recall.ai/product/meeting-bot-api
- Bot docs: https://docs.recall.ai/docs/bot-overview
- License/status: closed commercial SaaS/API.
- Problem solved: meeting bot infrastructure across Zoom, Meet, Teams, Webex, Slack, and others.
- Recall says bots join as meeting participants.
- Recall advertises real-time transcripts with speaker names and separate participant audio/video streams.
- Strength: proves bot mode can get true speaker names and participant streams.
- Weakness: not OSS.
- Weakness: not local privacy-first.
- Weakness: bot appears in meeting and changes consent/UX.
- Fit: not v4 local primitive.
- Future: benchmark/contrast for a cloud/bot edition.

## B.9 MeetingBaaS

- URL: https://www.meetingbaas.com/license
- License: Business Source License for core bot/server technology.
- The license allows personal/internal use but restricts commercial service/product use until change date.
- Problem solved: self-hostable meeting bot platform for Zoom, Meet, Teams.
- Strength: more inspectable than closed SaaS.
- Weakness: not standard permissive OSS for product embedding.
- Weakness: bot architecture differs from local desktop capture.
- Fit: not v4 primitive.

## B.10 Vexa

- URL: https://github.com/Vexa-ai/vexa
- License: Apache-2.0.
- Source: https://vexa.ai/pricing
- Languages: TypeScript, Python, C++, shell.
- Problem solved: self-hostable meeting bot/transcription API for Google Meet, Teams, Zoom.
- Strength: permissive license.
- Strength: self-hostable data-sovereignty path.
- Weakness: bots join meetings.
- Weakness: platform automation is a big maintenance surface.
- Weakness: not a macOS local capture library.
- Fit: possible future `meetily-bot`, not v4 local core.

## B.11 macOS capture primitives

### Core Audio process taps

- Official sample: https://developer.apple.com/documentation/coreaudio/capturing-system-audio-with-core-audio-taps
- Function docs: https://developer.apple.com/documentation/coreaudio/audiohardwarecreateprocesstap%28_%3A_%3A%29
- API captures outgoing audio from processes/groups.
- Taps are used via HAL aggregate devices.
- Apple sample requires macOS 14.2+.
- Apple sample requires `NSAudioCaptureUsageDescription`.
- Apple says the first start prompts for system audio recording permission.
- Fit: primary v4 system audio backend on macOS 14.2+.

### ScreenCaptureKit

- Docs: https://developer.apple.com/documentation/screencapturekit
- Sample: https://developer.apple.com/documentation/screencapturekit/capturing_screen_content_in_macos
- `SCStreamConfiguration` includes audio capture options.
- Sample outputs `.audio` and `.microphone` sample buffers.
- Permission is Screen Recording and may require restart after grant.
- Fit: fallback for macOS 13 through 14.1 and possibly app/window filtering.

### AudioCap

- URL: https://github.com/insidegui/AudioCap
- License: BSD-2-Clause.
- Language: Swift.
- Problem solved: Core Audio tap sample for macOS system/app audio capture.
- README says `NSAudioCaptureUsageDescription` must be entered manually.
- README says public permission check/request API is lacking; sample has private TCC option that should not be shipped casually.
- Fit: high-value reference for tap setup and TCC pitfalls.

### RecordKit

- URL: https://nonstrict.eu/recordkit/guides/system-audio-recording.html
- License: not determined from docs; evaluate before dependency use.
- RecordKit docs say default backend uses Core Audio on macOS 14.2+ and ScreenCaptureKit on older macOS.
- RecordKit docs say missing `NSAudioCaptureUsageDescription` makes Core Audio system-audio recordings silent.
- Fit: design reference and validation of backend choice.

### AudioCap Recorder

- URL: https://chrisns.github.io/audiocap-recorder/docs/QuickStart/
- Language: Swift.
- Captures audio from specific processes using ScreenCaptureKit.
- Requires Screen Recording permission.
- Fit: reference for process filtering; verify maturity/license before use.

### Rust `screencapturekit`

- URL: https://docs.rs/screencapturekit
- License: MIT/Apache per lib.rs listing: https://lib.rs/crates/screencapturekit
- Provides Rust bindings for Apple's ScreenCaptureKit.
- Supports system audio and microphone capture on supported macOS versions.
- Fit: strong Rust fallback candidate.

### systemAudioDump

- URL: https://github.com/sohzm/systemAudioDump
- License: MIT.
- Language: Swift.
- Captures system audio and writes raw PCM to stdout using ScreenCaptureKit.
- Fit: useful test utility/reference, but CLI permission attribution is exactly what Meetily should avoid in production.

---

# Section C: Recommendation

## C.1 Concrete v4 architecture

- Build Meetily v4 as a bundled, signed, notarized macOS desktop app first.
- Treat the CLI as a developer/debug tool only.
- Use a stable Bundle ID for production capture.
- Add correct Info.plist privacy strings.
- Include `NSMicrophoneUsageDescription`.
- Include `NSAudioCaptureUsageDescription`.
- Include Screen Recording/System Audio guidance for ScreenCaptureKit fallback.
- Never rely on the parent terminal app for production permissions.

## C.2 Capture strategy

- Recommended strategy: source-separated capture, unified transcript engine.
- Capture raw rail 1: `mic`.
- Capture raw rail 2: `system`.
- Do not run them as two independent meeting transcribers.
- Do not merge transcripts after the fact using token containment and overlap heuristics.
- Do audio-domain segmentation first.
- Do source arbitration before ASR.
- Send source-tagged speech windows to one ASR scheduler.
- Emit `Me` for accepted mic speech.
- Emit `Them` for accepted system speech.
- Preserve rail metadata, energy, correlation, and backend diagnostics.
- Do not promise per-person labels for remote participants in v4.

## C.3 Why not single mixed stream?

- Single mixed stream is easy to record but hard to attribute.
- It forces diarization to separate local user from remote speakers acoustically.
- It makes laptop-speaker echo look like local speech.
- Whisper does not solve diarization.
- pyannote/NeMo can add clusters, not reliable names in real time.
- Granola Desktop did not choose this as its visible model; it uses source-derived `Me`/`Them`.
- Therefore single mixed should be an export format, not the internal truth.

## C.4 Why not true per-participant streams?

- True participant streams require meeting protocol integration.
- Zoom/Teams/Meet have this because they are the meeting service/client.
- Recall.ai and Vexa get it by joining as bots or using platform automation.
- A local invisible app cannot generally access participant RTP/WebRTC streams from arbitrary meeting apps.
- Reverse-engineering meeting media is brittle and likely unacceptable.
- Therefore true per-participant capture is future bot/platform mode, not v4 local core.

## C.5 Why not hardware-AEC as core?

- Hardware/software AEC can help but should not be the central bet.
- AEC quality varies by device, OS path, app, and routing.
- Prior AUVoiceProcessingIO/sonora-aec plans should not be the v4 foundation.
- Use echo-aware source arbitration instead.
- If system rail has strong remote speech and mic rail has correlated lower-energy speech, classify mic content as echo leakage.
- If mic rail has strong near-end speech not correlated with system rail, classify it as `Me`.
- If both rails have strong uncorrelated speech, allow overlap with two labels.
- This is not text dedup.
- This happens before ASR.

## C.6 macOS system-audio mechanism

- Primary on macOS 14.2+: Core Audio process/system taps.
- Source: https://developer.apple.com/documentation/coreaudio/capturing-system-audio-with-core-audio-taps
- Reason: official API for outgoing process/system audio.
- Reason: audio-specific permission via `NSAudioCaptureUsageDescription`.
- Reason: RecordKit independently recommends Core Audio on 14.2+.
- Source: https://nonstrict.eu/recordkit/guides/system-audio-recording.html
- Fallback on macOS 13.0-14.1: ScreenCaptureKit audio capture.
- Source: https://developer.apple.com/documentation/screencapturekit/capturing_screen_content_in_macos
- Optional fallback: virtual driver import/routing for advanced users, not default.
- Avoid bare CLI capture as production UX.

## C.7 Permission and health model

- First launch runs a capture setup wizard.
- Show live microphone meter.
- Show live system-audio meter.
- Ask user to play a test sound or open demo video.
- Confirm non-zero system samples.
- Confirm non-zero mic samples.
- Store permission state and backend used.
- If system tap returns all zeros while playback is expected, show hard error.
- Do not continue into a meeting.
- Log OSStatus from tap creation and aggregate device setup.
- Log backend: `core_audio_tap`, `screen_capture_kit`, or `none`.
- Log sample rate and channel count.
- Log selected output device class, not sensitive full names in exported logs.
- Provide `Copy diagnostics`.
- Provide reset instructions for System Settings privacy panes.

## C.8 Transcription model/runtime

- Keep whisper.cpp/whisper-rs as default runtime.
- Use Metal acceleration on Apple Silicon.
- Default model should be chosen by benchmark; likely `large-v3-turbo` or a quantized variant.
- Use a single ASR scheduler.
- Feed short source-tagged speech windows, not fixed giant 60-second windows.
- Use a WhisperStreaming-style local-agreement policy for partials.
- Source: https://github.com/ufal/whisper_streaming
- Keep faster-whisper as benchmark/reference.
- Source: https://github.com/SYSTRAN/faster-whisper
- Consider sherpa-onnx for VAD and diarization experiments.
- Source: https://github.com/k2-fsa/sherpa-onnx

## C.9 Diarization approach

- V4 live labels: only `Me` and `Them`.
- `Me` comes from accepted mic windows.
- `Them` comes from accepted system windows.
- No pyannote live dependency.
- No NeMo Sortformer product dependency due CC-BY-NC public model license and footprint.
- Store enough metadata to support later offline diarization.
- Optional post-meeting mode can run pyannote if user accepts model terms.
- Optional post-meeting mode can test sherpa-onnx diarization if quality is adequate.
- UI must state that `Them` means system/remote audio, not a named person.

## C.10 What survives from current code

- Keep cpal mic capture concepts if clean.
- Keep resampling utilities if tested.
- Keep VAD only if it runs before ASR and exposes diagnostics.
- Keep whisper-rs/whisper.cpp integration.
- Keep backend storage if it supports source-tagged segments.
- Keep Tauri shell as product entry point.
- Keep CLI only for diagnostics/regression tests.
- Keep macOS tap knowledge but rewrite permission/health lifecycle.

## C.11 What to throw away

- Throw away two independent Whisper workers as the core model.
- Throw away text-layer dedup as the primary echo solution.
- Throw away token containment/time-overlap heuristics as correctness-critical code.
- Throw away treating a zero tap as silence.
- Throw away sonora-aec3 as v4 foundation.
- Throw away AUVoiceProcessingIO as v4 foundation.
- Throw away terminal-launched production recording UX.
- Throw away design docs centered on post-ASR dedup.

## C.12 Why this avoids Qi's bug

- Qi's bug: interactive terminal launch produced zero `THEM` segments because system tap silently returned zeros.
- Same binary launched from another parent session worked.
- That strongly implicates permission attribution/session context rather than ASR correctness.
- Apple, RecordKit, and AudioCap evidence shows system audio capture depends on Info.plist usage strings and TCC permissions.
- Source: https://developer.apple.com/documentation/coreaudio/capturing-system-audio-with-core-audio-taps
- Source: https://nonstrict.eu/recordkit/guides/system-audio-recording.html
- Source: https://github.com/insidegui/AudioCap
- A signed app bundle with stable Bundle ID gets a stable TCC identity.
- A terminal-launched unbundled CLI can be mediated by Terminal.app, Warp, sshd, or other parent context.
- The v4 wizard verifies actual non-zero system samples before meeting capture.
- Therefore the app cannot produce a 60-minute `YOU` echo transcript while the `system` rail is dead.

## C.13 Concrete milestones

- Milestone 1: bundled capture harness.
- Acceptance: app shows live mic and system meters from normal app launch.
- Milestone 2: Core Audio tap backend.
- Acceptance: macOS 14.2+ captures YouTube/system output with audio permission and fails loudly if not.
- Milestone 3: ScreenCaptureKit fallback backend.
- Acceptance: macOS 13/14.1 path works after Screen Recording permission and restart flow.
- Milestone 4: audio-domain arbitration.
- Acceptance: laptop-speaker YouTube test does not create a giant `Me` utterance when `system` is active.
- Milestone 5: unified ASR scheduler.
- Acceptance: one coherent transcript with `Me`/`Them` source labels and stable timestamps.
- Milestone 6: optional diarization spike.
- Acceptance: pyannote or sherpa-onnx can annotate clusters offline, but product does not depend on it.

## C.14 Final recommendation

Meetily v4 should be a Granola-style local desktop recorder, but local-first and fail-loud: capture microphone and system audio as separate raw rails inside a signed macOS app, use Core Audio process taps on macOS 14.2+ with ScreenCaptureKit fallback, perform audio-domain source arbitration before ASR, feed accepted source-tagged speech windows into a single whisper.cpp scheduler, and label live transcript output only as `Me` and `Them`. This preserves the production-proven part of the approach while discarding the brittle pieces that failed Qi: terminal-dependent TCC, silent zero taps, independent source transcripts, and text dedup as echo cancellation.

---

# Appendix: evidence ledger

## Granola

- Evidence: no bot joins the meeting.
- Source: https://docs.granola.ai/help-center/consent-security-privacy/security-privacy-data-faqs
- Evidence: Desktop captures mic and system audio.
- Source: https://docs.granola.ai/help-center/taking-notes/transcription
- Evidence: Desktop uses real-time transcription.
- Source: https://docs.granola.ai/help-center/taking-notes/transcription
- Evidence: audio is passed to a transcription provider.
- Source: https://docs.granola.ai/help-center/taking-notes/transcription
- Evidence: audio cache is temporary/deleted per privacy docs.
- Source: https://docs.granola.ai/help-center/consent-security-privacy/security-privacy-data-faqs
- Evidence: Desktop has no live diarization.
- Source: https://docs.granola.ai/help-center/taking-notes/transcription
- Evidence: Desktop labels map to microphone and system audio.
- Source: https://docs.granola.ai/help-center/taking-notes/transcription
- Evidence: Granola cannot isolate individual applications.
- Source: https://docs.granola.ai/help-center/taking-notes/transcription
- Evidence: macOS users need Microphone and Screen & System Audio permissions.
- Source: https://docs.granola.ai/help-center/troubleshooting/transcription-issues
- Evidence: macOS 14.2+ works best.
- Source: https://docs.granola.ai/help-center/troubleshooting/transcription-issues

## Apple/macOS

- Evidence: Core Audio taps capture outgoing process/group audio.
- Source: https://developer.apple.com/documentation/coreaudio/capturing-system-audio-with-core-audio-taps
- Evidence: taps are created via `AudioHardwareCreateProcessTap`.
- Source: https://developer.apple.com/documentation/coreaudio/audiohardwarecreateprocesstap%28_%3A_%3A%29
- Evidence: Apple's tap sample uses aggregate devices.
- Source: https://developer.apple.com/documentation/coreaudio/capturing-system-audio-with-core-audio-taps
- Evidence: Apple's tap sample requires macOS 14.2+.
- Source: https://developer.apple.com/documentation/coreaudio/capturing-system-audio-with-core-audio-taps
- Evidence: Apple's tap sample requires `NSAudioCaptureUsageDescription`.
- Source: https://developer.apple.com/documentation/coreaudio/capturing-system-audio-with-core-audio-taps
- Evidence: ScreenCaptureKit streams audio/video sample buffers.
- Source: https://developer.apple.com/documentation/screencapturekit
- Evidence: ScreenCaptureKit sample has `capturesAudio`, `captureMicrophone`, `.audio`, and `.microphone` outputs.
- Source: https://developer.apple.com/documentation/screencapturekit/capturing_screen_content_in_macos
- Evidence: RecordKit says missing `NSAudioCaptureUsageDescription` makes Core Audio recordings silent.
- Source: https://nonstrict.eu/recordkit/guides/system-audio-recording.html
- Evidence: AudioCap documents TCC/API gaps around permission request/checking.
- Source: https://github.com/insidegui/AudioCap

## Zoom/Teams/Meet

- Evidence: Zoom local recording can save separate participant audio files.
- Source: https://support.zoom.com/hc/en/article?id=zm_kb&sysparm_article=KB0076922
- Evidence: Zoom cloud recording can generate transcripts.
- Source: https://support.zoom.com/hc/en/article?id=zm_kb&sysparm_article=KB0064927
- Evidence: Zoom has auto-generated captions/live transcription.
- Source: https://www.zoom.com/en/blog/zoom-auto-generated-captions/
- Evidence: Teams live transcription includes speaker names and timestamps.
- Source: https://support.microsoft.com/en-us/office/start-stop-and-download-live-transcripts-in-microsoft-teams-meetings-dc1a8f23-2e20-4684-885e-2152e06a4a8b
- Evidence: Teams media bots access raw media frames.
- Source: https://learn.microsoft.com/en-us/microsoftteams/platform/bots/calls-and-meetings/real-time-media-concepts
- Evidence: Teams media flows use ICE/SRTP/RTP and Microsoft relays.
- Source: https://learn.microsoft.com/en-us/microsoftteams/microsoft-teams-online-call-flows
- Evidence: Teams Rooms can use voice profiles/Intelligent Speakers for in-room identity.
- Source: https://support.microsoft.com/en-us/office/use-microsoft-teams-intelligent-speakers-to-identify-in-room-participants-in-a-meeting-transcription-a075d6c0-30b3-44b9-b218-556a87fadc00
- Evidence: Google Meet transcripts save to Drive.
- Source: https://support.google.com/meet/answer/12849897
- Evidence: Google Meet transcript entries include participant references.
- Source: https://developers.google.com/workspace/meet/api/reference/rest/v2/conferenceRecords.transcripts.entries
- Evidence: Google Meet Media API uses WebRTC concepts and virtual streams.
- Source: https://developers.google.com/meet/media-api/guides/virtual-streams

## OSS

- Evidence: whisper.cpp is MIT and Apple Silicon optimized.
- Source: https://github.com/ggml-org/whisper.cpp
- Evidence: faster-whisper is MIT and CTranslate2-based.
- Source: https://github.com/SYSTRAN/faster-whisper
- Evidence: faster-whisper supports VAD filtering.
- Source: https://github.com/SYSTRAN/faster-whisper
- Evidence: whisper_streaming is MIT and uses local agreement.
- Source: https://github.com/ufal/whisper_streaming
- Evidence: pyannote diarization models have model cards and gated access details.
- Source: https://huggingface.co/pyannote/speaker-diarization
- Evidence: pyannote distinguishes open/community and premium Precision-2 models.
- Source: https://huggingface.co/pyannote
- Evidence: NeMo docs describe Sortformer.
- Source: https://docs.nvidia.com/nemo-framework/user-guide/latest/nemotoolkit/asr/speaker_diarization/intro.html
- Evidence: public NVIDIA Sortformer model card is CC-BY-NC-4.0.
- Source: https://huggingface.co/nvidia/diar_sortformer_4spk-v1
- Evidence: sherpa-onnx is Apache-2.0 and supports local speech functions.
- Source: https://github.com/k2-fsa/sherpa-onnx
- Evidence: sherpa-onnx has speaker diarization docs.
- Source: https://k2-fsa.github.io/sherpa/onnx/speaker-diarization/index.html
- Evidence: Recall.ai bots join meetings and expose real-time data.
- Source: https://docs.recall.ai/docs/bot-overview
- Evidence: Recall.ai advertises speaker names and separate participant streams.
- Source: https://www.recall.ai/product/meeting-bot-api
- Evidence: MeetingBaaS core license is BSL.
- Source: https://www.meetingbaas.com/license
- Evidence: Vexa is Apache-2.0 and self-hostable.
- Source: https://github.com/Vexa-ai/vexa
- Evidence: Vexa supports Meet/Teams/Zoom bot transcription.
- Source: https://github.com/Vexa-ai/vexa
