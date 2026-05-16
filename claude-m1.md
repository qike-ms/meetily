# webnote M1 Design Review

_Reviewed by Claude Sonnet 4.6 — 2026-05-12_

---

## Architecture Critique

### Manifest / permissions

`tabCapture` does **not** need host_permissions — it's a regular permission. But the WebSocket connection to localhost does: `"host_permissions": ["ws://127.0.0.1:*/*"]`. Without that, the offscreen doc's WebSocket will be blocked by CSP in MV3.

You're also missing `"offscreen"` in the `permissions` array — required for `chrome.offscreen.*` API. Add `"storage"` too (popup needs to read live transcript across the SW sleep/wake boundary).

### background.ts — tabCapture in MV3

**Critical pattern shift from MV2.** In MV3 service workers, `chrome.tabCapture.capture()` is gone. The correct MV3 path:

1. Background calls `chrome.tabCapture.getMediaStreamId({tabId, consumerTabId})` → returns a `streamId` string.
2. Background sends `streamId` to the offscreen doc via `chrome.runtime.sendMessage`.
3. Offscreen doc calls:
   ```js
   navigator.mediaDevices.getUserMedia({
     audio: { mandatory: { chromeMediaSource: 'tab', chromeMediaSourceId: streamId } }
   })
   ```

This is the only path that works in MV3 service workers.
Ref: https://developer.chrome.com/docs/extensions/reference/api/tabCapture#method-getMediaStreamId

### offscreen.ts — reasons

`chrome.offscreen.createDocument()` requires a `reasons` array. For tab capture + mic, use `["USER_MEDIA"]`. If you play the captured audio back (for monitoring), add `"AUDIO_PLAYBACK"`. Chrome enforces that the document's actual usage matches the declared reasons, and they audit this for store review.
Ref: https://developer.chrome.com/docs/extensions/reference/api/offscreen#type-Reason

### Resampling in worklet

Simpler than a custom 48k→16k resampler: create the AudioContext with `sampleRate: 16000` directly.

```js
const ctx = new AudioContext({ sampleRate: 16000 });
```

Chromium handles the hardware→16k conversion internally via its high-quality resampler. Your worklet then receives 16kHz samples natively — the worklet becomes just a PCM extractor. This also handles non-48kHz hardware (e.g., 44.1kHz on some devices) correctly without special-casing.

### Auth token delivery

Token-in-URL-querystring is fine for local only. One gap: how does the extension know the token? For M1: backend prints it to stdout on startup; user pastes it into extension options page. Simple, zero extra endpoints. Don't automate it yet.

### Missing: WS reconnection

If backend restarts mid-meeting, the extension WS closes with no recovery. For M1 a "dead WS → error banner in popup → user clicks Restart" flow is acceptable. Note it as a known gap.

### Missing: GPU memory with two Whisper workers

Two whisper.cpp server processes both loading the model = 2× GPU VRAM. For `small` (~466MB), fine. For `large-v3` (~6GB), this will OOM a typical 8GB GPU. See Q3 below.

---

## Open Questions

### Q1 — Single WS or two?

**Recommendation: Option A (one WS, source field).**

The worker isolation argument for (B) is real but the cost is also real: two WS connections from the extension, two reconnection loops, double the connection state. For M1 the backend demux is trivial — a `source: "mic"|"system"` field is five lines.

One concrete case where (B) wins: independent flow control. But at 20ms frames and ~640 bytes/frame (~25 KB/s per stream), backpressure over a loopback socket is not a real concern.

Implement (A); redesign to (B) only if stream cross-contamination becomes a measured problem.

### Q2 — Audio framing

**Send Int16 PCM, 20ms frames, batch 5 frames (100ms per WS send).**

- Float32 → Int16 conversion in the worklet is `sample * 32767 | 0`, a few microseconds per frame. Do it there.
- Int16 is what whisper.cpp's `/inference` endpoint expects as WAV anyway — converting backend-side adds numpy but not simplicity.
- 5-frame batching = 100ms latency addition per WS send. Whisper needs 5–30s of audio before returning anything, so 100ms accumulated latency is irrelevant to end-to-end transcript lag.
- Binary WS frames, not base64-JSON. Minimal binary header: `[source_u8][sequence_u16][pcm_i16 × 1600]` = 3203 bytes per message.

### Q3 — Whisper invocation

**Keep Option A (long-running whisper.cpp HTTP server), but run one server, not two.**

Run **one** whisper.cpp server instance. Backend maintains two async queues (mic, system), feeds them serially into the single server, with mic-stream priority. This avoids 2× VRAM. Serial processing adds at most one chunk's latency (5s) between streams — acceptable for transcription.

Avoid pywhispercpp: Python GIL + two simultaneous decodes = one blocks the other; less tested with GPU; harder to tune beam search and context window.

### Q4 — VAD

**Skip Silero VAD for M1, but add an RMS gate in the AudioWorklet.**

Without VAD, Whisper on the mic stream during silence reliably produces "Thank you.", "you", "Hmm.", ".". This will look broken to first-time users.

Lightweight alternative: RMS gate in the worklet before sending. If the RMS of a 100ms frame is below ~−40 dBFS, don't send that frame. This is ~10 lines of JS, catches silence and keyboard noise, and eliminates most hallucinations. Not as good as Silero but good enough for M1. Add Silero in M2 once you have real-meeting data to tune the threshold against.

### Q5 — Offscreen doc lifecycle

**The concern is partially wrong, but the right pattern still matters.**

Offscreen documents run in their own dedicated renderer process, not the service worker. The SW can sleep; the offscreen doc and its AudioWorklet + WebSocket continue running. The 25s keepalive ping from background to offscreen is **not needed** for the offscreen doc's survival.

What IS needed: the service worker waking up when the user clicks Stop. Use `chrome.runtime.connect` from the popup during active recording — this keeps the SW alive for popup ↔ background messaging only.

Verified lifecycle pattern:
1. Click Start → SW wakes, creates offscreen doc, gets tabCapture stream ID via `getMediaStreamId`, sends to offscreen doc via `sendMessage`.
2. SW may sleep. Offscreen doc runs indefinitely (active MediaStream + active WS keep it alive).
3. Click Stop → popup port wakes SW → SW sends "stop" to offscreen doc → offscreen doc closes streams + WS → SW calls `chrome.offscreen.closeDocument()`.

One gotcha: only **one offscreen document** per extension is allowed. Guard with `chrome.offscreen.hasDocument()` before creating.

Ref: https://developer.chrome.com/docs/extensions/reference/api/offscreen  
Ref: https://developer.chrome.com/docs/extensions/develop/migrate/to-service-workers#keep-sw-alive

### Q6 — M0.5 milestone

**Yes, add it.** MV3 offscreen + tabCapture has enough sharp edges to warrant isolation before backend complexity.

> M0.5 success criterion: extension icon click captures current tab audio + mic; popup shows live audio level meters for both streams. No backend, no WS, no transcript.

Things M0.5 de-risks:
- `getMediaStreamId` → offscreen `getUserMedia` with `chromeMediaSource: tab` actually works in your Chrome version
- AudioWorklet loads correctly from `chrome-extension://` URL (CSP can block `worker-src` — add `worker-src 'self'` to manifest)
- `AudioContext({sampleRate: 16000})` is accepted by the hardware
- Offscreen document lifecycle behavior on your OS + Chrome version
- Store review compatibility (Google rejects mismatched `reasons`)

Recommended milestones:
- **M0**: ✅ standalone HTML, getDisplayMedia + getUserMedia, AEC3 verified
- **M0.5**: MV3 extension shell — tab capture + mic → audio levels in popup. No backend.
- **M1**: WS → backend → whisper → transcript in popup. Meeting persisted to SQLite.
- **M2**: Silero VAD, LLM summarizer, multi-meeting UI, store submission prep.

---

## Summary Risk Table

| Risk | Severity | Notes |
|---|---|---|
| MV3 tabCapture API change (`capture()` gone in SW) | **High** | Use `getMediaStreamId` + offscreen `getUserMedia`. Breaks silently if missed. |
| Two Whisper instances → GPU OOM | **Medium** | Serialize to one server. Document large-model limitation. |
| Missing `offscreen` + WS host_permissions in manifest | **Medium** | WS blocked silently, hard to debug. |
| Whisper hallucinations on silent mic | **Low-medium** | RMS gate in worklet is a fast fix. |
| No WS reconnection | **Low** | Error banner; user restarts. Known gap for M1. |
| AudioContext 16kHz rejection on some hardware | **Low** | Verified by M0.5; fallback to native rate + worklet resample. |
| CSP blocking worklet script load | **Low** | Add `worker-src 'self'` to manifest CSP. |
