# Meetily Dogfood — Track B

Owner: Qi
Window: 7 days starting build verification
Branch: track-b-bootstrap (qike-ms fork)

## Build status

(Filled in by bootstrap PR.)

## Start a meeting

```bash
cd ~/git/meetily
./backend/clean_start_backend.sh           # transcript + summary backend
cd frontend && ./clean_run.sh              # macOS Tauri app
```

Pick app target, grant mic + screen-recording permissions, hit Record. Output lives in the meetily SQLite DB (not yet on disk in meeting-folder-v1 shape — see EXPORTER-PLAN.md).

## Stop a meeting

Click Stop in the Tauri app. Summary is generated automatically (Ollama gemma4:e4b by default; swap to Anthropic API if quality is weak — see Pushback below).

## Success criteria (frozen — do not move the goalposts)

- 5+ real meetings over 7 days, mix of:
  - Zoom desktop
  - Teams desktop
  - In-person via Mac mic only
- >=90% transcript usable (no >30s drops, no hallucination loops)
- Me vs Them diarization >=80% accurate (meetily distinguishes mic vs system audio sources — usable proxy)
- Summary available <=2 min after meeting end
- Output exports cleanly to meeting-folder-v1 schema (after exporter ships — week 2)

## Filing issues

For each failed meeting, capture:
- Date, app, duration, network conditions
- What went wrong (drop, garbled, missed speaker, slow summary, etc.)
- Time index in the transcript if applicable
- Whether re-running the same audio fixes it

Open as GitHub issues on qike-ms/meetily with label `dogfood-2026-05`.

## Pushback worth remembering

Meetily defaults to local Ollama (gemma4:e4b on M5) for summaries. That is not Claude Opus quality. If summary quality is the bottleneck after a couple of meetings, swap the summarizer layer for an Anthropic API call (cheapest fix, highest impact). The transcript layer (Whisper/Parakeet) is the part that needs to stay fast + local.

## Top 3 risks

1. Mic vs system-audio diarization fails on in-person meetings (no system audio source).
2. Long meetings (>1h) may hit Ollama context limits and produce truncated summaries.
3. macOS screen-recording permission flow can silently fail; verify permission grant after every macOS update.

## Hard exit criteria

After 7 days, if fewer than 3 meetings pass all 5 success criteria, fall back to Track A (webnote) as primary and revisit meetily after upstream advances.
