# Meetily Client-Server Architecture

**Status**: Design (post-debate)
**Date**: 2026-04-27
**Context**: [[meetily]] fork at `qike-ms/meetily`
**Debate**: See `temp/prd-debate.md` for full 4-phase architectural debate

## Problem

Meetily is a monolithic Tauri desktop app. We need meeting capture + transcription + summarization across the fleet:
- M4 (macOS) -- primary meeting machine, has GPU
- i7 (Linux desktop) -- secondary meeting machine
- M1 (Asahi Fedora) -- server, web UI host

Current blockers: Tauri requires GUI, no headless mode, no web UI, no client-server split.

## Key Decisions (from debate)

1. **Not Vexa** -- Vexa joins meetings as a bot participant. We need local mic/system capture.
2. **Batch mode for v1** -- Record full meeting, transcribe after, POST text to server. No real-time streaming. (Granola does this too.)
3. **Rust client** -- Extract audio/Whisper code from Meetily Tauri, remove Tauri deps. Shell/ffmpeg pipeline too fragile.
4. **Simple web UI** -- Single HTML file served by FastAPI. No React/Next.js for v1.
5. **Obsidian export** -- Write summaries to vault as markdown for long-term reference.
6. **Defer chat/follow-ups** -- Core value is record → transcribe → summarize. Chat is v2.
7. **Stateless follow-ups (v2)** -- Re-inject transcript as context per request, don't depend on OpenCode session persistence.
8. **Source tagging, not diarization** -- Mic segments tagged `"mic"`, system segments tagged `"system"`. No pyannote needed.

## Architecture

```
Client (M4 / i7)                    Server (M1)
┌───────────────────┐               ┌──────────────────────┐
│ meetily-client     │  POST text    │ meetily-server       │
│                    │──────────────>│                      │
│ - mic capture      │  (Tailscale)  │ - REST API (FastAPI) │
│ - system capture   │               │ - SQLite storage     │
│ - Whisper (local)  │               │ - OpenCode summary   │
│ - source tagging   │               │ - Single-file web UI │
│                    │               │ - Obsidian export    │
└───────────────────┘               └──────────────────────┘
```

## meetily-client (Rust binary)

Records audio, transcribes locally, sends text to server. No GUI needed.

**Reuse from Meetily** (already modular, minimal Tauri deps):
- `src-tauri/src/audio/capture/microphone.rs` -- mic via cpal
- `src-tauri/src/audio/capture/system.rs` -- system audio via cpal
- `src-tauri/src/audio/pipeline.rs` -- VAD (Silero), audio mixing
- `src-tauri/src/whisper_engine/whisper_engine.rs` -- Whisper transcription
- `src-tauri/src/audio/devices/` -- device discovery

**Remove**: Tauri `AppHandle`, `emit()`, `invoke()`, all UI code.

**Flow**:
```
meetily-client record --server http://m1:5167
  1. Create meeting via POST /api/meetings
  2. Start recording mic + system as separate WAV streams
  3. User presses Ctrl+C (or meeting ends)
  4. Transcribe each stream with Whisper (parallel)
  5. Interleave segments by timestamp, tag source (mic/system)
  6. POST segments to /api/meetings/{id}/transcript
  7. Trigger summarization via POST /api/meetings/{id}/summarize
```

**Transcript segment format**:
```json
{
  "segments": [
    {"timestamp": "00:01:23", "text": "Let's discuss the roadmap", "source": "system", "duration_ms": 2100},
    {"timestamp": "00:01:26", "text": "Sure, I think we should prioritize...", "source": "mic", "duration_ms": 4500}
  ]
}
```

**Platform support**:
- macOS: Metal GPU for Whisper (`large-v3-turbo`), ScreenCaptureKit for system audio
- Linux: CPU for Whisper (`small` or `base`), PulseAudio/ALSA for capture

## meetily-server (Python FastAPI)

Stores meetings, generates summaries, serves web UI.

**API** (8 endpoints):
```
POST   /api/meetings                  Create meeting
GET    /api/meetings                  List meetings
GET    /api/meetings/{id}             Get meeting + transcript + summary
DELETE /api/meetings/{id}             Delete meeting
POST   /api/meetings/{id}/end        Mark meeting ended
POST   /api/meetings/{id}/transcript  Upload transcript segments
POST   /api/meetings/{id}/summarize   Trigger OpenCode summary
GET    /api/search?q=                 Full-text search
```

**Summarization**: `opencode run --format json --pure "<transcript>"` via Python subprocess. Parse NDJSON, extract text events (same logic we built in Rust, ported to Python).

**Web UI**: Single `index.html` served by FastAPI. Vanilla JS + fetch(). Meeting list, transcript view (mic left / system right like a chat), summary panel, search.

**Obsidian export**: On summary completion, write `projects/meetily/meetings/YYYY-MM-DD-{title}.md` to the vault.

**Database** (single SQLite):
```sql
CREATE TABLE meetings (
    id TEXT PRIMARY KEY,
    title TEXT,
    status TEXT DEFAULT 'recording',
    client_id TEXT,
    created_at TIMESTAMP,
    ended_at TIMESTAMP
);

CREATE TABLE transcript_segments (
    id INTEGER PRIMARY KEY,
    meeting_id TEXT REFERENCES meetings(id),
    timestamp TEXT,
    text TEXT,
    source TEXT,        -- 'mic' or 'system'
    confidence REAL,
    duration_ms INTEGER
);

CREATE TABLE summaries (
    id INTEGER PRIMARY KEY,
    meeting_id TEXT REFERENCES meetings(id),
    content TEXT,
    created_at TIMESTAMP
);

CREATE VIRTUAL TABLE transcript_fts USING fts5(
    text, content=transcript_segments, content_rowid=id
);
```

## Work Items

> Track on GitHub Issues at `qike-ms/meetily`. Every commit references a work item.

### Phase 1: Server

| ID | Work Item | Acceptance Criteria |
|----|-----------|-------------------|
| WI-1 | Create `/api/meetings` CRUD endpoints (create, list, get, delete, end) | Can create a meeting, list all, fetch by ID, mark ended via curl |
| WI-2 | Create `/api/meetings/{id}/transcript` endpoint (POST segments) | Can upload array of `{timestamp, text, source, duration_ms}` segments; stored in SQLite |
| WI-3 | Create `/api/meetings/{id}/summarize` endpoint | Triggers `opencode run --format json` subprocess; parses NDJSON text events; stores summary in DB |
| WI-4 | Create `/api/search` endpoint with FTS5 | Full-text search across transcript_segments returns matching meetings |
| WI-5 | Single-file HTML web UI served by FastAPI | Meeting list, transcript view (mic/system differentiated), summary panel, search bar -- all via fetch() |
| WI-6 | Obsidian vault export on summary completion | Writes `projects/meetily/meetings/YYYY-MM-DD-{title}.md` with transcript + summary |
| WI-7 | SQLite schema setup (meetings, transcript_segments, summaries, FTS) | Migrations run on startup; schema matches design doc |

### Phase 2: Client

| ID | Work Item | Acceptance Criteria |
|----|-----------|-------------------|
| WI-8 | Create `meetily-client` Rust crate in workspace (no Tauri deps) | `cargo check` passes; binary compiles with `tokio`, `cpal`, `whisper-rs` only |
| WI-9 | Extract audio device discovery from Tauri | List mic + system audio devices via CLI; works on macOS and Linux |
| WI-10 | Extract mic capture stream (cpal) | Record mic audio to WAV file; headless, no GUI required |
| WI-11 | Extract system audio capture stream | Record system/speaker audio to separate WAV file; macOS (ScreenCaptureKit) + Linux (PulseAudio) |
| WI-12 | Extract Whisper engine (whisper-rs) | Transcribe WAV file to timestamped segments; Metal GPU on macOS, CPU on Linux |
| WI-13 | Dual-stream batch transcription pipeline | Record mic + system simultaneously; after stop, transcribe each; interleave by timestamp; tag source |
| WI-14 | HTTP client: POST transcript to server | Upload segments to `/api/meetings/{id}/transcript`; trigger `/summarize` |
| WI-15 | CLI interface (`meetily-client record --server URL`) | Start/stop recording via Ctrl+C; configurable mic/system device, Whisper model, server URL |

### Phase 3: Deployment

| ID | Work Item | Acceptance Criteria |
|----|-----------|-------------------|
| WI-16 | Model download CLI (`meetily-client download-model`) | Download and cache Whisper models locally; verify checksum |
| WI-17 | systemd service file for Linux | `systemctl start meetily-server` works on M1 |
| WI-18 | launchd plist for macOS client | Client auto-starts on login on M4 |
| WI-19 | End-to-end test: M4 records meeting → M1 shows transcript + summary | Full pipeline works across Tailscale |

## Deferred to v2

| Feature | Reason |
|---------|--------|
| Real-time streaming (WebSocket) | Batch is sufficient for v1 |
| Chat / follow-up per meeting | Not core value |
| Meeting auto-detection | Manual start/stop is fine |
| Calendar integration | Not needed |
| React/Next.js web UI | Single HTML is enough |
| Speaker diarization (pyannote) | Source tagging is sufficient |
| Multi-client same meeting | Edge case |
