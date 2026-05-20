# Meetily Per-Source Pipeline — Implementation Handoff

**Date:** 2026-05-08
**Author:** QI-M4 OpenCode session (handoff to next session)
**Design:** [[per-source-pipeline-design]] (v3.1, codex APPROVED)
**Tracking:** GitHub issue #51 + sub-issues #52–#60
**Branch convention:** `wi-<id>-<short-desc>` off `main`

---

## TL;DR for resuming

You are picking up an in-flight implementation of the per-source audio pipeline for meetily. The design (`per-source-pipeline-design.md` v3.1) is codex-approved across 3 review rounds. 9 sub-issues are filed (#52–#60). **A1 is implemented and PR'd (#61, branch `wi-A1-meetily-audio-skeleton`); 7 WIs remain**.

Your job: continue from A1 → A2 → A3 → A4 → B1 → Tauri-Unmix → B2 → UX. Each WI = its own branch, codex review until LGTM, PR. Per Qi: "don't wait for my review, directly go to implementation after codex approves your design."

**Stop and ask Qi only if:**
- A WI requires architectural deviation from the design doc
- Codex finds a blocker that needs human judgment to resolve
- Existing tests start failing in a way you don't understand

Otherwise, keep moving.

---

## Current state

### Done — Session 2 (2026-05-09)

| WI | Issue | PR | Branch | Codex | Status |
|----|-------|-----|--------|-------|--------|
| Design v3.2 | #51 | — | — | — | ✅ updated this session — sonora-aec3 swap, drop policy, block size, aligner correction |
| A1 | #52 | #61 | `wi-A1-meetily-audio-skeleton` | LGTM r1 | open, awaiting Qi merge |
| A2 | #53 | #62 | `wi-A2-extract-dsp` | LGTM r1 | open, stacked on A1 |
| A3 | #54 | #63 | `wi-A3-coreaudio-backend` | APPROVE r1 (suggestions applied) | open, stacked on A2 |
| A4 | #55 | #64 | `wi-A4-coreaudio-default` | LGTM r2 (r1 caught real `--streaming false` regression) | open, stacked on A3 |
| B1 | #56 | #66 | `wi-B1-aec3` | APPROVE r2 (r1 caught real ~50% audio loss) | open, stacked on A4 |
| UX | #60 | #68 | `wi-UX-streaming-drain` | LGTM r2 (r1 caught abort_all + 2s window lies) | open, off main directly |
| Tauri-Unmix | #57 | #70 | `wi-Tauri-Unmix` | APPROVE r2 (r1 caught source-dropped-in-frontend blocker) | open, stacked on A2 |

### Remaining

| WI | Issue | Blocked by | Description |
|----|-------|-----------|-------------|
| B2 | #58 | B1 + Tauri-Unmix | AEC3 in Tauri pipeline. **Designed but not started — see "B2 pre-thought" section.** |
| #1 (user bug) | #59 | A3, B1 verification | Closes when CLI passes dedup acceptance per [[verification-protocol]] Test 2 |

### Followup issues filed this session

| # | Title | Origin | Priority |
|---|-------|--------|----------|
| #65 | paired-frame coherent drop via centralized AEC pump | B1 design pivot | low (only if drops observed in production) |
| #67 | per-source `WhisperContext` for parallel CLI transcription | UX bench finding | medium (memory vs throughput tradeoff) |
| #69 | parallel Tauri transcription via per-source `WhisperContext` + workers | Tauri-Unmix scope cut | medium (mirror of #67 for Tauri) |

### Stack visualization

```
main
 ├── #61 (A1)    ← merge first
 │    └── #62 (A2)
 │         ├── #63 (A3)
 │         │    └── #64 (A4)
 │         │         └── #66 (B1)
 │         └── #70 (Tauri-Unmix)
 └── #68 (UX)    ← independent, off main
```

7 PRs, 4-deep critical path. Per Qi's call: **park here, no new code**, until Qi merges at least #61–#62 so the rest can rebase cleanly.

**Order:** A1 → A2 → A3 → A4 → B1 → Tauri-Unmix → B2. UX in parallel anytime. **B2 must come after Tauri-Unmix** (wiring AEC into Tauri while it still pre-mixes defeats per-source labels).

---

## Verifying A1 (what's already shipped)

Before continuing, sanity-check that A1 compiles + tests pass. From `~/git/meetily`:

```bash
git fetch origin
git checkout wi-A1-meetily-audio-skeleton
git pull

# 1. Both clients still compile (no behavioral change)
cargo check -p meetily-audio
cargo check -p meetily-client
cargo check -p meetily   # Tauri package — slow first time, ~1 min

# 2. Run compile-fail tests (the architectural enforcement)
cargo test -p meetily-audio --test no_mixing
# Expected: test result: ok. 1 passed; 0 failed.

# 3. Confirm CLI still runs
cargo run --release -p meetily-client -- devices
# Expected: lists mic + system audio devices

# 4. Type rule manually verifiable: try to construct TranscriptionFrame externally
mkdir -p meetily-audio/examples
cat > meetily-audio/examples/break.rs <<'EOF'
use meetily_audio::{SourceLabel, TranscriptionFrame};
fn main() {
    let _ = TranscriptionFrame { source: SourceLabel::Mic, samples: vec![], timestamp_ms: 0 };
}
EOF
cargo build -p meetily-audio --example break 2>&1 | grep "error\[E"
# Expected: error[E0451]: fields ... are private
rm meetily-audio/examples/break.rs
```

If all pass, A1 is good. If not, fix before continuing — diagnostics in PR #61.

---

## Workflow per WI

For each remaining WI, follow this exact pattern. **Do not skip codex review.** Per Qi: "make sure codex said LGTM."

### 1. Branch off main (NOT off the previous WI's branch)

```bash
cd ~/git/meetily
git fetch origin
git checkout -b wi-<id>-<short-desc> main
```

If the WI depends on a previous unmerged PR, branch off that PR's branch instead and rebase later. For now, all PRs land independently because A1 has zero behavioral change.

### 2. Implement per the design doc + issue acceptance criteria

Read the issue carefully. Acceptance criteria are explicit. The design doc (`obsidian-vault/projects/meetily/per-source-pipeline-design.md`) has the full architectural reasoning.

**Reference code locations** (verified accurate as of 2026-05-08):
- Tauri pre-mix: `frontend/src-tauri/src/audio/pipeline.rs:145-188, 823-849`
- Tauri macOS capture (Core Audio Tap, default): `frontend/src-tauri/src/audio/capture/core_audio.rs:91`
- Tauri backend selection: `frontend/src-tauri/src/audio/capture/backend_config.rs:79`
- Tauri transcription worker (hardcoded `"Audio"`): `frontend/src-tauri/src/audio/transcription/worker.rs`
- CLI per-source streaming: `meetily-client/src/main.rs:135-217`
- CLI BlackHole capture: `meetily-client/src/audio/capture.rs:302-306, 377-381`
- CLI VAD wrapper: `meetily-client/src/audio/vad.rs`
- CLI resampler: `meetily-client/src/audio/resample.rs`
- Tauri Silero VAD: `frontend/src-tauri/src/audio/vad.rs`

### 3. Verify locally

```bash
cargo check -p meetily-audio
cargo check -p meetily-client
cargo check -p meetily
cargo test -p meetily-audio       # if you added tests there
cargo test -p meetily-client      # if you touched CLI
# For Tauri-Unmix specifically: also build the frontend
cd frontend && pnpm install && pnpm tauri build --debug && cd ..
```

### 4. Codex review

```bash
git add -A   # stage everything; codex reviews staged changes

cat > /tmp/codex-<wi>.md <<'EOF'
You are reviewing the implementation of WI-<id> (issue #<n>) for the meetily project.

WI scope: <one sentence>
Design doc: ~/git/obsidian-vault/projects/meetily/per-source-pipeline-design.md (v3.1, codex APPROVED)
Issue: https://github.com/qike-ms/meetily/issues/<n>

Files changed: <list>

Verified before review:
- cargo check -p <packages> passes
- <any tests run>

Please review:
1. Correctness vs design doc and issue acceptance criteria
2. Type-level no-mixing rule preserved (TranscriptionFrame still uninstantiable externally)
3. Object-safety of any new traits
4. Backpressure / channel sizing per design (esp. for B1)
5. Any regressions in existing tests
6. API surface for downstream WIs
7. Anything you'd flag in code review

Output:
- Issues found (severity: blocker / nit / suggestion)
- Verdict: LGTM / APPROVE WITH MINOR CHANGES / REVISE

Max 3 review rounds. If still REVISE after round 3, escalate to Qi.
EOF

codex exec --skip-git-repo-check -C ~/git/meetily --sandbox read-only - < /tmp/codex-<wi>.md
```

**Loop:** apply changes from each round, re-run codex, until LGTM or hit round 3.

### 5. Commit + push + PR

```bash
git add -A
git commit -m "wi-<id>: <description>

<body explaining what + why; reference design doc + codex review history>

Fixes #<issue>
Refs #51"

git push -u origin wi-<id>-<short-desc>

gh pr create --repo qike-ms/meetily \
  --title "wi-<id>: <description>" \
  --body "<full body>" \
  --base main
```

PR body should include:
- What this PR does
- Verification (cargo check / test results)
- Codex review summary
- Out of scope (what comes in next WI)
- `Fixes #<n>` and `Refs #51`

---

## WI-by-WI implementation notes

### A2 (#53) — Extract resampler + VAD

**Source:**
- CLI: `meetily-client/src/audio/resample.rs` (~119 LOC) and `meetily-client/src/audio/vad.rs` (~211 LOC)
- Tauri: `frontend/src-tauri/src/audio/vad.rs` (Silero wrapper — verify it's the same algorithm)

**Approach:**
1. Create `meetily-audio/src/resample.rs` with the CLI's resampler. Public type `Resampler::new(src_rate, dst_rate)`, method `process(&[f32]) -> Vec<f32>`.
2. Create `meetily-audio/src/vad.rs` with Silero wrapper. Public type `SileroVad::new(...) -> Result<Self>`, method `process(&[f32]) -> Vec<SpeechSegment>`.
3. Update CLI to use `meetily_audio::Resampler` and `meetily_audio::vad::SileroVad`. Delete duplicate code.
4. If Tauri's VAD is identical, route Tauri through the crate too. If different, leave Tauri alone for now (Tauri-Unmix will revisit).

**Tests:**
- Unit tests in `meetily-audio/tests/dsp.rs`: resample 1s of 48k sine to 16k, check length is 16000 ± tolerance.
- Run existing CLI streaming session end-to-end with a known audio file; confirm transcript unchanged.

**Acceptance:** transcripts byte-identical (or token-identical) before/after on a reference recording.

### A3 (#54) — Core Audio Tap macOS backend

**Source:** `frontend/src-tauri/src/audio/capture/core_audio.rs` (cidre-based tap, line 91 has the key call `with_mono_global_tap_excluding_processes`).

**Approach:**
1. Create `meetily-audio/src/capture/mod.rs` with the `AudioSource` trait wired up.
2. Create `meetily-audio/src/capture/core_audio.rs` lifting the Tauri implementation. Behind a Cargo feature `coreaudio`.
3. Update `meetily-audio/Cargo.toml` to add `[target.'cfg(target_os = "macos")'.dependencies]` for cidre.
4. Add `--backend coreaudio` to CLI. When set, use the new source instead of cpal loopback.
5. Tauri continues to use the same code path — verify by reading Tauri's existing call sites.

**Tests:**
- CLI: `meetily-client record --backend coreaudio --mic "..." --system "..."` records without BlackHole on macOS 14.2+
- Tauri: existing flow unchanged

**Permissions to document:** `NSAudioCaptureUsageDescription` Info.plist key required (macOS 14.2+).

### A4 (#55) — Default + drop BlackHole from docs

**Approach:**
1. Make `--backend coreaudio` the macOS default in `meetily-client` CLI parser. cpal still selectable via `--backend cpal`.
2. README diff: remove BlackHole installation, Audio MIDI Setup steps, Multi-Output Device instructions for macOS.
3. Add a paragraph: "macOS 14.2+: system audio captured via Apple's native Core Audio Tap. No third-party drivers required."

**Acceptance:** new macOS user can record without installing anything.

### B1 (#56) — WebRTC AEC3 + CLI

**Crate:** `webrtc-audio-processing` (tonarino).

**Approach:**
1. Add `webrtc-audio-processing` to `meetily-audio/Cargo.toml`. Verify it builds on macOS (likely needs cmake/meson; check first with `cargo check`).
2. Create `meetily-audio/src/aec.rs` with `AecPipeline` per design v3.1:
   - `new(sample_rate) -> Result<(Self, AecOutputs)>`
   - `ingest_mic(frame)`, `ingest_system(frame)`
   - Internal aligner: pair frames by timestamp ±100ms, paired-drop on overflow
   - 200ms warm-up (buffer fill); document AEC3 takes 2–5s for full convergence
3. Wire into CLI between capture and VAD chains. Add `--no-aec` flag (default: AEC on).

**Hard parts:**
- Cross-platform build of `webrtc-audio-processing` — may need to vendor or use a system package on Linux. Document gotchas.
- AEC3 needs 10ms frames at 16kHz. Reframe after resampler.
- Far-end delay hint: `set_stream_delay_ms(100)` initially; tune empirically.

**Tests:**
- Unit: feed AEC3 a known mic+far-end pair, check ERLE ≥ 20dB.
- Integration: 30s recording with mic + system playback. Count duplicate pairs (`[YOU]` vs `[THEM]` ±500ms / ≥0.85 token overlap). Target: ≥80% reduction vs no-AEC baseline.

**Acceptance:** see issue #56.

### Tauri-Unmix (#57) — Remove pre-mix

**Files to change** (from design doc v3.1):
- `frontend/src-tauri/src/audio/pipeline.rs` — remove `ProfessionalAudioMixer` from transcription path; two parallel VAD chains
- `frontend/src-tauri/src/audio/transcription/worker.rs` — emit `mic` / `system` source instead of `Audio`
- `frontend/src-tauri/src/audio/recording_state.rs` — track per-source streams
- `frontend/src-tauri/src/audio/recording_saver.rs` — `TranscriptSegment` add `source` field
- Tauri events: `transcript-update` payload includes `source`
- `frontend/src/types/index.ts` — add `source: "mic" | "system"` to transcript segment type
- `frontend/src/components/` — render with source labels (distinct visual treatment)
- API uploads: ensure `source` is sent (backend schema in `design.md` already supports it)

**This is the biggest WI by far.** Treat it as a full data-model migration. Allocate accordingly.

**Tests:**
- Tauri end-to-end: record meeting, verify transcript JSON has per-segment `source` field
- UI: visual confirmation mic and system segments render distinctly
- Architectural test in CI: `grep` finds no `mix` calls in transcription path (only in recording-mixer code if any remains)

### B2 (#58) — AEC in Tauri

**Approach:** identical to B1, but wired into Tauri's per-source pipeline (which Tauri-Unmix built). Reuse `meetily_audio::AecPipeline` directly.

**Critical:** must NOT land before Tauri-Unmix. Verify Tauri is per-source first.

**Acceptance:** Tauri matches CLI on bleed-reduction criterion.

### UX (#60) — Shutdown drain

**File:** `meetily-client/src/main.rs:135-217` (the `run_streaming_session` function).

**Approach:**
1. Add a counter of pending transcribe tasks. After Ctrl+C, print `Transcribing N pending utterances...` and decrement as each completes.
2. Spawn a second `ctrl_c` listener after the first fires. If second fires within 2s, abort remaining `JoinHandle`s, print drop count, exit with partial.
3. **Investigation step:** measure whether `spawn_blocking` transcribe tasks are actually parallel on Metal. Time:
   - 1 utterance solo: how long?
   - N utterances spawned concurrently: total time?
   - If concurrent ≈ N × solo, tasks serialize → file sub-issue for parallelization
   - If concurrent ≈ solo, tasks parallelize → progress counter alone is enough
4. Document investigation result in issue #60 comments.

**Tests:**
- Manual: record 30s, Ctrl+C, observe progress counter
- Manual: record 30s, Ctrl+C twice within 2s, observe abort + partial transcript exit

---

## Codex review template (copy-paste ready)

For each WI, save this as `/tmp/codex-<wi>.md` with placeholders filled in:

```markdown
You are reviewing the implementation of WI-<id> (issue #<n>) for the meetily project.

**WI scope:** <one sentence from the issue>
**Design doc:** ~/git/obsidian-vault/projects/meetily/per-source-pipeline-design.md (v3.1, codex APPROVED)
**Issue:** https://github.com/qike-ms/meetily/issues/<n>

**Files changed:** <git diff --stat output>

**Verified before review:**
- cargo check -p <packages>: PASS
- cargo test -p <packages>: PASS / N/A
- <any manual smoke tests>

**Please review:**
1. Correctness vs design doc and issue acceptance criteria
2. Type-level no-mixing rule preserved (TranscriptionFrame still uninstantiable externally)
3. Object-safety of any new traits (Box<dyn ...> works)
4. Backpressure / channel sizing per design (B1 specifically: aligner-only drop, not per-source)
5. Any regressions in existing tests / behavior
6. API surface for downstream WIs
7. Anything you'd flag in code review

**Output:**
- Issues found (severity: blocker / nit / suggestion)
- Verdict: LGTM / APPROVE WITH MINOR CHANGES / REVISE

Max 3 review rounds. If REVISE after round 3, escalate to Qi.
```

Run with:

```bash
codex exec --skip-git-repo-check -C ~/git/meetily --sandbox read-only - < /tmp/codex-<wi>.md
```

If a review round produces specific edits, apply them, re-stage, re-run codex. Loop until LGTM.

---

## Things that bit me (lessons learned)

1. **Read code, not comments.** I claimed Tauri uses ScreenCaptureKit because the `backend_config.rs` enum comment said so. Codex caught that the actual default in `default()` returns `CoreAudio`, and `core_audio.rs` uses cidre's tap. Always verify by reading the code path that actually runs.

2. **Object-safety of traits.** I sketched `AudioSource::start() -> impl Stream<...>` which is not object-safe (`Box<dyn AudioSource>` won't compile). Use `Pin<Box<dyn Stream<Item = AudioFrame> + Send>>` for trait return types.

3. **Tokio mpsc receivers are single-consumer.** My v2 design had `output_mic(&self) -> Receiver` which is wrong — Receiver can't be cloned or fetched twice. Use `new() -> (Self, Outputs)` returning the receivers once at construction.

4. **trybuild stderr snapshots.** First run creates `wip/*.stderr` files; promote them to `tests/compile-fail/*.stderr` and re-run for the test to pass.

5. **Workspace member paths.** Cargo.toml relative paths bite: `meetily-client/Cargo.toml` says `../meetily-audio`; `frontend/src-tauri/Cargo.toml` says `../../meetily-audio` (two levels up).

6. **Tauri builds slowly.** First `cargo check -p meetily` after a workspace change is ~1 min. Plan accordingly.

---

## Session 2 learnings (2026-05-09) — what v3.1 didn't anticipate

### 1. Build-risk gates: probe upstream deps BEFORE writing wrapper code

v3.1 named `webrtc-audio-processing` (tonarino) for AEC. Verified before any code that:
- 0.3.x default = pkg-config + system `webrtc-audio-processing` lib (not on macOS).
- 0.3.x `bundled` feature = needs `glibtoolize`+`aclocal`+`automake`+`autoconf` (none installed by default).
- 2.0.x `bundled` = needs `meson` + `ninja` (also missing).

→ Switched to `sonora-aec3` v0.1.0 (pure-Rust port of WebRTC AEC3, BSD-3, by `dignifiedquire`). Builds clean in 14s with zero system deps. Same algorithm. Pinned exact (`=0.1.0`) per PM-M1 condition. **Always run a 5-minute probe build before designing around an upstream dep.**

Captured in design doc v3.2 §3 "Dependency choice"; followup vendoring path documented.

### 2. Whisper Metal serialization is real (1.12x speedup with 4 concurrent)

Bench in `meetily-client/examples/whisper_parallel_bench.rs` measured:
- solo: 805 ms / 3s utterance
- 4 concurrent (shared `Arc<WhisperContext>`, per-task `create_state()`): 717 ms per task, 2867 ms total
- speedup: 1.12x

**4 spawn_blocking tasks effectively serialize on Metal** when sharing a context. Implications:
- UX progress counter is essential (drain time really is N × solo).
- Tauri-Unmix's two parallel mic+system VAD chains feed a single shared worker; they queue not parallelize. Filed #69 for per-source `WhisperContext`s as a v2 work item (memory cost: ~3.2 GB for large-v3-turbo at f16).
- For CLI: same followup #67.
- **Don't share `WhisperContext` across parallel pipelines without verifying parallelism works on the target hardware first.**

### 3. AEC3 algorithm-native granularity is 4 ms / 64 samples, not 10 ms / 160

v3.1 said AEC3 "processes 10ms frames (160 samples at 16kHz)". That's the upper-level WebRTC frame size. sonora-aec3's `BlockProcessor` is one layer below: 4 ms / 64 samples per `Block`. We feed at 64-sample boundaries; reframing distinct from the AEC's internal accumulator is unnecessary.

This caught me on B1 round-1 codex review: the original code dropped the trailing 32-sample residue per call (480 = 7×64 + 32), losing ~50% of mic audio on the AEC path. Fix = `vad_acc` accumulator carried across pump iterations.

**Always verify block-size assumptions against the actual crate's API, not the reference algorithm doc.**

### 4. sonora-aec3's `RenderDelayController` replaces the custom aligner

v3.1 §1 mandated paired-frame coherent drop at a custom aligner. sonora-aec3 has its own `RenderDelayController` that does delay estimation internally — no custom aligner exists in the wrapper.

Drop policy v3.2 (CLI tee overflow): drop *newest* render frame on `try_send` Full (sync_channel semantics, NOT drop-oldest as v3.1 implied), bump `AecMetrics::render_drops`, accept ~2-5 s `RenderDelayController` reconvergence. Acceptable for v1; followup #65 (paired-frame drop via centralized AEC pump) deferred to "only if drops observed in production".

**When swapping a dep, re-verify which guarantees the dep gives you vs which you have to build.**

### 5. SQLite `speaker` column was already there, never wired

Migration `20251110000001_add_speaker_field.sql` added a `speaker TEXT` column to the Tauri `transcripts` table months ago. **Verified by grep that no code reads or writes it.** Tauri-Unmix re-uses this column directly as the canonical source field — zero new migration needed, zero version-skew complications.

**Always grep for unwired existing infrastructure before adding new schemas.**

### 6. Frontend listener path: codex's load-bearing catch

Compile + tsc both green. Architectural test green. Yet Tauri-Unmix would have shipped silently broken because the *primary* live transcript-update listener in `TranscriptContext.tsx` (line 306) didn't include `source` in its `Transcript` construction — only the manual `addTranscript` callback did. Backend correctly emitting `source`, frontend silently dropping it before storage and render.

Three frontend listener paths must always be updated together:
- Live: `transcriptService.onTranscriptUpdate` callback (TranscriptContext.tsx line 306).
- Manual: `addTranscript` callback (TranscriptContext.tsx line 408).
- Reload-sync: `getTranscriptHistory()` mapping (TranscriptContext.tsx line 382).
- IndexedDB recovery: `useTranscriptRecovery.ts` line 163.

**For any new field added to TranscriptUpdate, grep `frontend/src/` for ALL these paths and update each.** This is the kind of break compile-time + tsc miss because it's a missing field, not a type error.

### 7. Always have an architectural test that's enforceable, not just designed

`meetily-audio/tests/no_mixing_in_tauri.rs` two-pass grep gate enforces "no mixing in transcription path" structurally. v3.1 talked about the rule; v3.2 + this WI added a test that fails CI if the rule is broken. The architectural test catches structural regressions; codex catches data-flow regressions. **Both layers are necessary** — neither subsumes the other.

### 8. Codex round 1 catches real bugs roughly half the time

Across 7 PRs this session, codex round 1 caught **real blockers on 4 of 7**:
- A4: `--streaming false` regression on macOS default (clap default_value_t made non-streaming flow fail).
- B1: ~50% mic audio lost on AEC path (frame-size mismatch between AEC blocks and VAD frames).
- UX: `abort_all` doesn't actually abort `spawn_blocking`; "within 2s" was a lie.
- Tauri-Unmix: `source` dropped in the primary frontend listener and reload-sync paths.

**Don't skip codex review even when "obviously correct".** The 4 catches above all looked obviously correct in code review.

---

## B2 (#58) pre-thought — ready to start when B1 + Tauri-Unmix are merged

B2 is the mirror of B1 but for the Tauri pipeline. After both this WI's merges, the work is mechanical:

### Scope

Wire `meetily_audio::AecPipeline` into Tauri's per-source pipeline. Mic chunks go through AEC before VAD; system chunks tee into `AecPipeline::ingest_render`.

### Files to touch (estimate ~150 LOC)

- `frontend/src-tauri/Cargo.toml`: enable `meetily-audio/aec` feature.
- `frontend/src-tauri/src/audio/pipeline.rs`:
  - Add `aec: Option<AecPipeline>` field (lazy-allocated when `enable_aec` is on).
  - In the run loop, mic chunk handler:
    1. Drain render-tee non-blocking → `aec.ingest_render(...)` for each chunk.
    2. Propagate render_drops counter (Tauri can use a per-pipeline `AtomicU64` or just call `aec.record_render_drop()` directly since both paths run in the same task — no thread-cross needed).
    3. `aec.process_capture(&chunk.data)` → output → `vad_acc` accumulator → drain in 480-sample windows.
  - System chunk handler: also push to `aec.ingest_render(samples)` directly (no tee channel needed since pipeline.rs owns both paths in one task — **simpler than B1's CLI design which needed a tee because mic+system are separate threads**).
- `recording_manager.rs` or `recording_commands.rs`: surface a Tauri preference / setting for AEC enable/disable. Default on; `--no-aec` equivalent in Tauri settings UI.
- Architectural test: extend `no_mixing_in_tauri.rs` to verify `AecPipeline` is *used* somewhere in `pipeline.rs` (positive assertion, not negative).

### Why B2 is simpler than B1's CLI integration

B1 had to thread render audio across **two separate threads** (mic pump + system pump) via a sync_channel tee + atomic drop counter. Tauri's pipeline runs in a **single async task** that already sees both raw streams, so no tee + no atomic needed. The `AecPipeline` is just owned by the task and called inline.

### Acceptance (mirrors B1)

Same as Test 2 in [[verification-protocol]] but on the Tauri app instead of the CLI. ERLE ≥ 20 dB synthetic + ≥80% dup-pair reduction in real recording.

### What NOT to change

- Per-source label flow (already done in Tauri-Unmix).
- Tauri's `WhisperContext` count (still 1; #69 is separate).
- Recording-WAV mixer (still separate per design v3.2 §1).

### Estimated effort

Half-day given the B1 reference, plus codex review rounds. Lower complexity than B1 because the threading collapse is gone.

### Order of operations after Qi merges

1. Merge stack → main.
2. Run [[verification-protocol]] Tests 1-4. Each green = corresponding PR verified.
3. Branch `wi-B2-tauri-aec` off main.
4. Implement per the sketch above.
5. Codex review (expect 1-2 rounds; design is well-understood).
6. PR.

---

## Communication / handoff

- **Status pings:** Qi mentioned "way to monitor my progress so I'm not invisible to you for hours." If you're a separate agent session and have signal-send access, ping after each PR lands. Otherwise: PR-per-WI is the natural progress signal.
- **PM-M1 collaboration:** earlier in this session, PM-M1 (an OpenCode session on M1) was coordinating WI-41 (PR #50) and reviewed scope decisions for the per-source design. If they're still active, loop them in on PR reviews.
- **Escalation:** if a WI requires deviating from the design doc or codex finds a blocker after 3 rounds, stop and ask Qi.

---

## Quick references

- **Repo:** `qike-ms/meetily` (default branch `main`)
- **SSH config:** `m1` proxies via `kay`. Backend at `http://localhost:5167` after `ssh -fN -L 5167:localhost:5167 m1` (run inside tmux for persistence: `tmux new-session -d -s meetily-tunnel "ssh -N -L 5167:localhost:5167 m1"`)
- **Whisper model:** `~/.local/share/meetily/models/ggml-large-v3-turbo.bin` (already downloaded)
- **codex binary:** `/Users/qike/.npm-global/bin/codex` (v0.128.0)
- **Design doc location:** `~/git/obsidian-vault/projects/meetily/per-source-pipeline-design.md`
- **GitHub labels created:** `area: audio`, `area: cli`, `area: aec`, `area: refactor`, `area: tauri`, `design`

## Open questions / future issues to file as work progresses

1. **Whisper context parallelism.** Investigate during UX (#60). May spawn a sub-issue for explicit parallelization or batch transcribe.
2. **AEC bypass for headphone users.** `--no-aec` CLI flag is in B1 scope; Tauri toggle goes in B2.
3. **Mixed playback WAV.** Recording deprioritized for v1 — file as a low-priority issue when someone needs it.
4. **Multi-speaker diarization in system stream.** `[THEM-1]`, `[THEM-2]` for multi-participant Zoom calls. Future, separate ML model.
5. **Linux PipeWire + Windows WASAPI native loopback.** Currently CLI uses cpal default-output workaround. File when prioritized.
