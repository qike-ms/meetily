# CLAUDE.md

Guidance for Claude Code / agents working in this repo.

## Project Overview

**Meetily** — privacy-first AI meeting assistant. Captures, transcribes, and summarizes meetings on local infrastructure.

- **Frontend**: Tauri 2.x desktop app — Rust + Next.js 14 + React 18 + TypeScript
- **Backend**: FastAPI + SQLite (aiosqlite) for meeting storage and LLM summarization
- **Audio**: Rust (cpal, whisper-rs) with professional mixing
- **Transcription**: Whisper.cpp, local, GPU-accelerated
- **LLM providers**: Ollama (local), Claude, Groq, OpenRouter

## Development Commands

### Frontend (`/frontend`)

```bash
# macOS
./clean_run.sh [debug]         # Clean build + run (info or debug logging)
./clean_build.sh               # Production build

# Windows
clean_run_windows.bat
clean_build_windows.bat

# Manual
pnpm install
pnpm run dev                   # Next.js only, port 3118
pnpm run tauri:dev             # Full Tauri dev mode
pnpm run tauri:build           # Production build

# GPU-specific (testing acceleration)
pnpm run tauri:dev:metal | :cuda | :vulkan | :cpu
```

### Backend (`/backend`)

```bash
# macOS
./build_whisper.sh small       # Build Whisper with model
./clean_start_backend.sh       # Start FastAPI on :5167

# Windows
build_whisper.cmd small
start_with_output.ps1          # Interactive setup
clean_start_backend.cmd

# Docker (cross-platform)
./run-docker.sh start --interactive    # macOS/Linux
.\run-docker.ps1 start -Interactive    # Windows
./run-docker.sh logs --service app
```

**Whisper models**: `tiny[.en]`, `base[.en]`, `small[.en]`, `medium[.en]`, `large-v1`, `large-v2`, `large-v3`, `large-v3-turbo`

### Service ports
- Whisper server: 8178
- Backend API: 5167 (docs at `/docs`, `/redoc`)
- Frontend dev: 3118

## Architecture (high level)

Three tiers: **Tauri desktop app** (Next.js UI ↔ Rust backend ↔ local Whisper) communicates with **FastAPI backend** (SQLite + LLM provider) over HTTP/WebSocket.

### Audio pipeline (critical)

Two parallel paths from raw audio (mic + system):
- **Recording path** — pre-mixed via `RecordingSaver.save()`. Professional mixing in `pipeline.rs`: RMS-based ducking prevents system audio from drowning out mic; clipping prevention.
- **Transcription path** — VAD-filtered, sent to `WhisperEngine.transcribe()`. Reduces Whisper load ~70% (speech only).

Mic and system streams arrive asynchronously; ring buffer (`VecDeque`) accumulates samples until both have aligned 50ms windows. Pipeline expects consistent **48kHz** (resampled at capture).

### Audio module layout (`frontend/src-tauri/src/audio/`)

```
devices/          # discovery.rs, microphone.rs, speakers.rs, configuration.rs
  platform/       # windows.rs (WASAPI), macos.rs (ScreenCaptureKit), linux.rs (ALSA/PulseAudio)
capture/          # microphone.rs, system.rs, core_audio.rs (macOS ScreenCaptureKit)
pipeline.rs       # mixing + VAD
recording_manager.rs / recording_commands.rs / recording_saver.rs
```

Where to look:
- Device detection → `devices/discovery.rs` or `devices/platform/{os}.rs`
- Mic/speaker → `devices/microphone.rs` or `devices/speakers.rs`
- Capture streams → `capture/microphone.rs` or `capture/system.rs`
- Mixing/VAD → `pipeline.rs`
- Recording workflow → `recording_manager.rs`

Refactor history: see [AUDIO_MODULARIZATION_PLAN.md](AUDIO_MODULARIZATION_PLAN.md).

### Tauri ↔ frontend

- **Commands** (frontend → Rust): `await invoke('cmd_name', {...})`. Defined with `#[tauri::command]` in `src-tauri/src/lib.rs`, registered in `tauri::Builder.invoke_handler(...)`.
- **Events** (Rust → frontend): `app.emit("event-name", payload)?` ↔ frontend `await listen<T>('event-name', cb)`.

### Whisper

- **Model storage** — dev: `frontend/models/` or `backend/whisper-server-package/models/`. Prod: `~/Library/Application Support/Meetily/models/` (macOS), `%APPDATA%\Meetily\models\` (Windows).
- **Loader**: `frontend/src-tauri/src/whisper_engine/whisper_engine.rs`. Auto-detects GPU (Metal/CUDA/Vulkan), falls back to CPU.
- **GPU**: macOS Metal + CoreML auto. Windows/Linux via Cargo features `--features cuda` or `--features vulkan`. Models cached on load — changing models requires app restart or manual unload/reload.

## Critical patterns

- **Concurrency**: `Arc<RwLock<T>>` for shared async state, `Arc<AtomicBool>` for flags. See `recording_state.rs`.
- **Hot-path logging**: `perf_debug!()` / `perf_trace!()` in `lib.rs` — zero cost in release builds (cfg-gated). Use these instead of `log::debug!` in audio pipeline code.
- **Audio metrics**: batch via `AudioMetricsBatcher` (pipeline.rs); pre-allocate via `AudioBufferPool` (buffer_pool.rs).
- **Frontend state**: `components/Sidebar/SidebarProvider.tsx` is the global context. Pattern: Tauri command → updates Rust state → emits event → React listener → context → components. Backend API at `http://localhost:5167`, also WebSocket for real-time.
- **Naming convention**: audio devices are `microphone` / `system` (NOT `input`/`output`).
- **Error handling**: Rust uses `anyhow::Result`; frontend uses try/catch with user-friendly messages.
- **File paths**: use Tauri path APIs (`downloadDir`, etc.) — never hardcode.

## Adding things

- **Tauri command**: define `#[tauri::command] async fn ...` in `src-tauri/src/lib.rs`, add to `invoke_handler!` macro, call via `invoke<T>('name', args)` from frontend.
- **Audio device platform**: add `audio/devices/platform/<os>.rs`, update `platform/mod.rs` exports, add types in `configuration.rs`. Verify with `cargo check`.
- **Backend endpoint**: add to `backend/app/main.py`. Use `DatabaseManager` (`backend/app/db.py`) for all SQLite access (async via `aiosqlite`).

## Debugging

```bash
# Verbose Rust logging
RUST_LOG=debug ./clean_run.sh                              # macOS
$env:RUST_LOG="debug"; ./clean_run_windows.bat             # Windows PowerShell
RUST_LOG=app_lib::audio=debug ./clean_run.sh               # Audio-only

# DevTools: Cmd+Shift+I (macOS) / Ctrl+Shift+I (Windows). Console toggle in app UI.
# Backend logs: stdout, formatted "ts - LEVEL - [file:line - func()] - msg"
# API exploration: http://localhost:5167/docs (Swagger), /redoc
```

Audio pipeline emits real-time metrics (buffer sizes, mixing window count, VAD detection rate, dropped chunk warnings) to the in-app DevTools console during recording.

## Platform notes

- **macOS**: System audio captured via **Apple Core Audio Tap** on **macOS 14.2+** in both the Tauri desktop app (default) and the `meetily-client` CLI (default; pass `--backend cpal` to fall back to the legacy cpal default-output loopback path that requires BlackHole + Multi-Output Device). `NSAudioCaptureUsageDescription` Info.plist key prompts for permission. Mic permission separate. ScreenCaptureKit (macOS 13+, requires screen recording permission) remains an alternate path. Metal + CoreML auto.
- **Windows**: WASAPI for capture (loopback for system audio). WASAPI exclusive mode can conflict with other apps. Build needs Visual Studio Build Tools with C++ workload. CUDA (NVIDIA) or Vulkan (AMD/Intel) via Cargo features.
- **Linux**: ALSA/PulseAudio. Build deps: `cmake`, `llvm`, `libomp`. CUDA/Vulkan via Cargo features.

## Constraints / gotchas

- **Backend optional**: frontend runs standalone with local Whisper, but meeting persistence + LLM summarization require the FastAPI backend running.
- **CORS**: backend allows `*` for dev; restrict before production.
- **Audio permissions**: request early. macOS needs **both** mic AND screen recording for system audio.
- **Model selection** — dev: `base`/`small` (fast iteration). Prod: `medium` or `large-v3` (best quality). GPU is 5-10x faster than CPU. Parallel batch processing in `whisper_engine/parallel_processor.rs`.
- **Frontend perf**: state batched via Sidebar context; transcript rendering virtualized; audio level monitoring throttled to 60fps.

## Git

- `main` — stable releases
- `fix/*` — bug fixes (current branch: `fix/audio-mixing`)
- `enhance/*` — feature enhancements

## Key files

**Core**: [src-tauri/src/lib.rs](frontend/src-tauri/src/lib.rs) (Tauri entry, command registration), [audio/mod.rs](frontend/src-tauri/src/audio/mod.rs), [backend/app/main.py](backend/app/main.py).

**Audio**: [recording_manager.rs](frontend/src-tauri/src/audio/recording_manager.rs), [pipeline.rs](frontend/src-tauri/src/audio/pipeline.rs), [recording_saver.rs](frontend/src-tauri/src/audio/recording_saver.rs).

**UI**: [page.tsx](frontend/src/app/page.tsx) (main recording UI), [SidebarProvider.tsx](frontend/src/components/Sidebar/SidebarProvider.tsx) (global state).

**Whisper**: [whisper_engine.rs](frontend/src-tauri/src/whisper_engine/whisper_engine.rs).
