# Meetily Per-Source Pipeline — Verification Protocol

**Status:** Draft v1
**Date:** 2026-05-09
**Companion to:** [[per-source-pipeline-design]] v3.2 · [[per-source-implementation-handoff]]
**Run after:** Qi merges PR #61 (A1) → #62 (A2) → #63 (A3) → #64 (A4) → #66 (B1) → #68 (UX) → #70 (Tauri-Unmix). Run from `main` after each merge OR all at once after the full stack is in.

---

## TL;DR

Three end-to-end tests on M4 macOS (14.2+), each a single recording session against a running meetily server. Acceptance criteria deferred from PRs #63, #64, #66, #70 are exercised here. **One pass = entire stack green for shipping.**

| Test | Verifies | PRs unblocked |
|------|----------|---------------|
| 1. CoreAudio capture (no BlackHole) | A3 + A4 acceptance | #63, #64 |
| 2. AEC dup-reduction + ERLE | B1 acceptance | #66 |
| 3. Tauri-Unmix end-to-end source flow | Tauri-Unmix #57 | #70 |

UX (#68) verification is split out at the bottom — it needs a long recording with intentional speech bursts.

---

## Common preconditions

```bash
# 1. Stack merged into main
cd ~/git/meetily
git checkout main && git pull
git log --oneline -10   # expect to see wi-A1, wi-A2, wi-A3, wi-A4, wi-B1, wi-UX, wi-Tauri-Unmix

# 2. Whisper model present (already downloaded)
ls -la ~/.local/share/meetily/models/ggml-large-v3-turbo.bin

# 3. Backend running
cd ~/git/meetily
ssh -fN -o ServerAliveInterval=30 -L 5167:localhost:5167 m1 || true
curl -sf http://localhost:5167/health   # expect 200

# 4. CLI release build (compiles all default features: coreaudio + aec)
cargo build -p meetily-client --release
./target/release/meetily-client devices    # confirm mic + system devices listed
```

If any precondition fails, fix that first; do not proceed with verification.

---

## Test 1 — CoreAudio capture (A3 + A4 acceptance)

**Goal:** record 30 seconds with mic + system audio simultaneously, **without BlackHole or Multi-Output Device installed**, on macOS 14.2+. Confirm both streams produce real transcripts.

### Audio source

Pick something that produces unambiguous English speech for ~30 seconds:
- TED talk YouTube clip (any of the popular short ones).
- Apple Podcasts: pick a 30s segment with clear single-speaker speech.
- Pre-recorded fixture (preferred for repeatability): a 30 s WAV/MP3 of clean speech played back via QuickTime.

Hit **play** then immediately speak into the mic for ~10 s of the 30 s window: "Testing one two three. This is QI on M4 verifying the per-source pipeline. The mic should be tagged YOU and the system audio should be tagged THEM."

### Run

```bash
# A4: --backend defaults to coreaudio on macOS, no flag needed.
./target/release/meetily-client record \
  --server http://localhost:5167 \
  --title "verify-A3-A4-coreaudio-$(date +%Y%m%d-%H%M%S)" \
  --model large-v3-turbo
```

Wait for `>>> Recording started <<<`. Start audio playback. Speak. After ~30s, Ctrl+C once.

### Pass criteria

- [ ] **No BlackHole path used.** Verify by inspecting `/Applications/` and Audio MIDI Setup — neither BlackHole nor a Multi-Output Device should be required. The recording should still capture.
- [ ] **Mic transcript present.** Final transcript section contains at least one segment tagged `[YOU]` with text matching what was spoken.
- [ ] **System transcript present.** At least one segment tagged `[THEM]` with text matching the played audio.
- [ ] **Both flow to backend.** `curl -sf http://localhost:5167/api/meetings | jq '.[0].segments | map(.source) | group_by(.) | map({source: .[0], count: length})'` shows both `mic` and `system` counts > 0.

### Fail diagnostics

- "No system devices listed" → check `meetily-client devices`; if Core Audio Tap is broken on this machine, the cidre / NSAudioCaptureUsageDescription path failed silently. Try `--backend cpal` for comparison; if that works, A3/A4 CoreAudio backend is broken.
- "All segments tagged `mic`" → A3 backend selected wrong source for system. Inspect logs: `RUST_LOG=meetily_client::audio::capture=debug` and re-run.
- "Server reports source as `Audio` (legacy)" → CLI is sending the old payload shape. Check WI-41 was not rolled back during merges.

---

## Test 2 — AEC dup-reduction + ERLE (B1 acceptance)

**Goal:** AEC3 reduces "system audio bleeding into mic and being transcribed twice" by ≥80% vs no-AEC baseline, and synthetic-test ERLE ≥ 20 dB.

### Fixture

Same audio playback as Test 1 — but this time **don't speak into the mic**. The mic should pick up only the acoustic echo of the system audio playing through the speakers. Without AEC, the mic transcribes a duplicate of the system stream → high duplicate-pair count. With AEC, the duplicates should drop dramatically.

### Baseline run (no AEC)

```bash
./target/release/meetily-client record \
  --server http://localhost:5167 \
  --title "verify-B1-baseline-noaec-$(date +%Y%m%d-%H%M%S)" \
  --model large-v3-turbo \
  --no-aec
```

Play 30s of audio. Don't speak. Ctrl+C. Note the meeting_id printed at the end.

### AEC run

```bash
./target/release/meetily-client record \
  --server http://localhost:5167 \
  --title "verify-B1-aec-$(date +%Y%m%d-%H%M%S)" \
  --model large-v3-turbo
# (default: AEC enabled)
```

Same playback, same no-mic-speech. Ctrl+C. Note the meeting_id.

### Pass criteria

- [ ] **Dup-pair count drops ≥80%.** Compute duplicate pairs in each transcript: a `[YOU]` segment matches a `[THEM]` segment within ±500ms (use `audio_start_time` ± 0.5) AND ≥0.85 token-overlap (Jaccard on lowercased word tokens).

  ```bash
  # Save the helper:
  cat > /tmp/dup-count.py <<'PY'
  import sys, json, requests
  meeting_id = sys.argv[1]
  segs = requests.get(f"http://localhost:5167/api/meetings/{meeting_id}").json()["segments"]
  mic = [s for s in segs if s["source"] == "mic"]
  sys_ = [s for s in segs if s["source"] == "system"]
  def tok(s): return set(s.lower().split())
  def jacc(a, b):
      A, B = tok(a), tok(b)
      return len(A & B) / max(len(A | B), 1)
  pairs = 0
  for m in mic:
      for s in sys_:
          if abs((m.get("audio_start_time") or 0) - (s.get("audio_start_time") or 0)) <= 0.5:
              if jacc(m["text"], s["text"]) >= 0.85:
                  pairs += 1
                  break
  print(f"meeting {meeting_id}: {pairs} dup pairs ({len(mic)} mic, {len(sys_)} sys segs)")
  PY
  python3 /tmp/dup-count.py <baseline_meeting_id>
  python3 /tmp/dup-count.py <aec_meeting_id>
  # Expected: aec_count <= 0.2 * baseline_count
  ```

- [ ] **AecMetrics::erle_db ≥ 20** at end of run. Extract from CLI logs (B1 should log this on shutdown; if not, instrument and re-run).
- [ ] **render_drops == 0** under steady-state. Non-zero indicates tee backpressure — investigate before declaring victory.
- [ ] **No regression with `--no-aec`.** Headphone users (no acoustic echo) should not see degraded mic quality. Run a brief `--no-aec` recording with intentional speech; transcript quality should match Test 1's mic transcript.

### Fail diagnostics

- "Dup pair count NOT down 80%" → AEC may not be converging in 30s. Try a 90s recording. If still high, check `set_delay_hint_ms(100)` is firing. If still high, sonora-aec3 acceptance is uncertain — check followup-issue path (vendored webrtc-audio-processing).
- "ERLE = 0 throughout" → audibility detector never engaged. System audio may be too quiet; turn up volume, retry.
- "render_drops > 0" → tee backpressure. Check pump thread isn't stalled. If reproducible, this validates issue #65 is real-world.

---

## Test 3 — Tauri-Unmix end-to-end (#57 acceptance)

**Goal:** record a meeting in the Tauri desktop app and verify the per-source `source` field appears at every persistence boundary.

### Run

1. Launch Tauri app: `cd ~/git/meetily/frontend && ./clean_run.sh debug` (or production build).
2. Click record. Let it run for ~30s with mic + system audio (same fixture as Test 1).
3. Stop. Note the meeting title or ID shown in the UI.

### Pass criteria

#### Boundary 1: in-memory `RecordingManager`

- [ ] DevTools / Tauri logs show `transcript-update` events firing with `source: "mic"` or `source: "system"` (NOT `"Audio"`).
  ```bash
  # Tauri logs path varies; find the active log:
  ls -lt ~/Library/Logs/com.meetily.app/ 2>/dev/null | head -3
  tail -100 ~/Library/Logs/com.meetily.app/<latest>.log | grep transcript-update
  ```

#### Boundary 2: SQLite

- [ ] Per-segment `speaker` column populated.
  ```bash
  sqlite3 ~/Library/Application\ Support/com.meetily.app/meetily.db \
    "SELECT speaker, count(*) FROM transcripts WHERE meeting_id = '<meeting_id>' GROUP BY speaker"
  # Expected: rows for 'mic' and 'system'; no NULL for new transcripts.
  # Old (pre-merge) transcripts in the same DB will have speaker=NULL — that's expected.
  ```

#### Boundary 3: API payload

- [ ] When the meeting is fetched (e.g. via the meetings list), the JSON payload includes `source` per segment.
  ```bash
  # Tauri uses local SQLite, not HTTP, so 'API payload' here means the
  # invoke() return value. Easiest way: open DevTools, fetch the meeting,
  # inspect the response object. Or via Rust:
  # cargo test -p meetily-client (no test exists; manual inspection of
  # MeetingTranscript serialization in Tauri is the right check).
  ```
  *Lighter version: skip if the SQLite check is green; the API serializer just relays the column.*

#### Boundary 4: frontend rendering

- [ ] In the Tauri UI's transcript panel, each segment shows a colored badge: green `[YOU]` for mic-sourced segments, blue `[THEM]` for system-sourced segments.
- [ ] Mixed/legacy segments (from any pre-merge recording reloaded from history) render with NO badge (neutral). Confirm by reloading an old meeting.

### Fail diagnostics

- "No badges visible" → frontend type narrowing failed. Open DevTools, inspect a `transcripts[i]` in `TranscriptContext` state; `source` should be `'mic'` or `'system'`. If `undefined`, check `useTranscriptStreaming` / `usePaginatedTranscripts` mappings.
- "All badges show `[YOU]`" → backend is still emitting `device_type: Microphone` for system chunks (the legacy `// Mixed audio` mislabel didn't get removed). Inspect `pipeline.rs` against PR #70.
- "SQLite rows have `speaker = NULL`" → either the worker isn't reading `chunk.device_type`, or `recording_commands.rs` listener isn't passing `update.source` through. The architectural test catches mixing in transcription path but NOT this kind of data drop — codex round 1 caught it on review, but if the symptom recurs in production, grep `frontend/src/contexts/TranscriptContext.tsx` for the three narrowing blocks.

---

## Test 4 — UX shutdown drain (#68 acceptance)

Independent track. Run only if you want to validate the progress UX without going through the full stack drain.

### Run

```bash
./target/release/meetily-client record \
  --server http://localhost:5167 \
  --title "verify-UX-drain-$(date +%Y%m%d-%H%M%S)" \
  --model large-v3-turbo
```

Speak continuously for ~30 seconds (lots of utterances → lots of pending Whisper tasks). Then Ctrl+C ONCE and watch.

### Pass criteria

- [ ] Banner appears: `>>> Transcribing N pending utterances... (Ctrl+C again to hard-exit, abandoning pending transcripts) <<<`
- [ ] Per-completion lines appear: `>>> Transcribed X/N (M pending) <<<`. Counter decreases monotonically.
- [ ] After all complete, final transcript is uploaded and meeting summary printed.

### Hard-exit branch

Repeat the run, but during the drain hit Ctrl+C a SECOND time.

- [ ] Process exits within ~1 second.
- [ ] Exit code is 130 (`echo $?`).
- [ ] stderr contains `Second Ctrl+C received: hard-exiting. Any pending Whisper transcribes are abandoned; the final transcript was NOT uploaded.`
- [ ] No orphan Whisper threads continuing to print transcripts after exit (`pgrep -f meetily-client` returns nothing).

---

## After all tests pass

1. **Mark verified** in each PR comment: paste the green checklist into PR #63, #64, #66, #70 (and #68 if Test 4 was run).
2. **Update handoff doc** (`per-source-implementation-handoff.md`) — change WI status to "verified-shipping".
3. **Close issue #1** (CLI duplicate transcripts) — Test 2 covers it.
4. **B2 (#58) is now ready for impl** — Tauri AEC integration mirrors B1 but uses the Tauri pipeline. See [[per-source-pipeline-design]] §3 + B1 PR #66 as reference.

## If any test fails

- Don't roll back the stack — the architectural change is correct even if a specific WI's runtime acceptance bar isn't met yet.
- File a focused fix-up issue (`wi-<X>-fix: <one-line>`) with the failing log lines.
- Pause B2 until the failed test is green.
