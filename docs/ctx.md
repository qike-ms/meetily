# Meetily - Session Context

**Updated**: 2026-04-28
**Repo**: https://github.com/qike-ms/meetily (fork of Zackriya-Solutions/meetily)

## What was done

- Forked Meetily to qike-ms, added OpenCode as LLM provider (feature/opencode-provider branch, merged)
- Designed client-server architecture via 4-phase adversarial debate (see projects/meetily/design.md)
- Implemented full Phase 1-3 with codex (gpt-5.5) as implementer, claude-code (opus) as adversarial reviewer
- 42 GitHub issues created, 3 PRs merged to main

### Components built

1. **meetily-server** (backend/server/) -- FastAPI REST API
   - Meeting CRUD, transcript ingestion, FTS5 search
   - OpenCode summarization via subprocess (NDJSON parsing)
   - Single-file HTML web UI at /app
   - Obsidian vault export with git sync
   - 84 tests (unit + integration + e2e)

2. **meetily-client** (meetily-client/) -- Standalone Rust CLI
   - Dual-stream audio capture (mic + system) via cpal
   - Whisper transcription with source tagging
   - Ring buffer pattern (no I/O on audio thread)
   - HTTP upload to meetily-server
   - Model download from HuggingFace

3. **Deployment** (deploy/) -- systemd + launchd + scripts

## Current state

- All code merged to main on qike-ms/meetily
- Server tests: 84/84 passing
- Client: cargo check passes
- 2 rounds of adversarial review per phase, all critical/high findings fixed

## Open items

- WI-19: Full e2e test across machines (M4 client -> M1 server)
- 4 medium findings from client review (silent sample drops, temp leak, FIR quality, hardcoded confidence)
- Whisper model: large-v3-turbo default, needs download before first use
- M1 runs CPU-only Whisper (no Metal on Asahi) -- use smaller model
