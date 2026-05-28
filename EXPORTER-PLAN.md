# Meetily Exporter Plan — meeting-folder-v1

Status: Planning, not implemented
Target schema: https://github.com/qike-ms/oc-agent-life/blob/main/specs/meeting-folder-v1.md
Owner: TBD (next ticket after dogfood passes)

## What meetily natively emits

Verified by reading backend/server/models.py and backend/app/db.py on commit 338790d.

- SQLite tables: meetings, transcript_segments, summaries.
- Per-meeting: id (UUID), title, status, created_at, ended_at, client_id.
- Per segment: timestamp (string), text, source ("mic" | "system"), confidence, duration_ms.
- Summary: stored as JSON blob in DB (SummaryResponse.content).
- No filesystem export. No native markdown output. No audio file kept by default.

## What meeting-folder-v1 needs

- Folder `~/git/obsidian-vault/qi/meetings/YYYY-MM-DD-<slug>/`
- metadata.json: schema_version, source, source_version, start, end, duration_seconds, app, title, participants[], audio_sha256, audio_path_relative, transcript_model, summary_model
- transcript.md: speaker-tagged, timestamped, Me/Them or named
- summary.md: TL;DR, Decisions, Action items, Open questions, Notes (5 required sections)
- notes.md: optional, user bullets
- raw/: optional, audio if kept; gitignored

## Gap analysis

| Field / artifact          | Meetily has it?         | Gap                                                         |
| ------------------------- | ----------------------- | ----------------------------------------------------------- |
| schema_version            | n/a                     | exporter hard-codes "meeting-folder-v1"                     |
| source                    | n/a                     | exporter hard-codes "meetily"                               |
| source_version            | implicit (git SHA)      | exporter resolves from `git -C ~/git/meetily rev-parse`     |
| start / end / duration    | yes (created_at/ended_at)| convert string to ISO 8601 with offset                     |
| app                       | no                      | infer from foregrounded app at meeting start (best-effort)  |
| title                     | yes                     | passthrough                                                 |
| participants[]            | partial                 | only mic vs system; map mic -> Me, system -> Them, no names |
| audio_sha256              | n/a                     | only if exporter saves audio; null otherwise                |
| transcript_model          | yes (config)            | read from backend config                                    |
| summary_model             | yes (config)            | read from backend config                                    |
| transcript.md             | partial                 | reformat segments, render Me/Them based on source           |
| summary.md (5 sections)   | partial                 | meetily emits JSON blob; restructure into 5 sections        |
| notes.md                  | no                      | meetily has no live note pane; skip for now                 |
| raw/audio                 | no                      | meetily does not retain by default                          |

## Approach (when implemented)

1. Run as a post-meeting hook in meetily backend, or a CLI invoked manually.
2. Query SQLite for meeting + segments + summary by meeting_id.
3. Map source -> role: mic = "me", system = "them"; speaker_id = "M0" / "S0".
4. Build participants[] with 2 entries: {name: "Qi Ke", role: "me", speaker_id: "M0"} and {name: "Unknown", role: "them", speaker_id: "S0"}.
5. Render transcript.md from segments in time order; Me: vs Unknown: prefix.
6. Parse the summary JSON. If it already has Decisions / Action items keys, map them. If not (plain prose), prompt Claude with the transcript to fill the 5 sections.
7. Write all files. Write metadata.json LAST (Hive readiness signal).

## Open questions before implementing

- Q1: Where does the slug come from when title is empty? Proposal: first-noun-of-first-utterance (LLM) with fallback to "untitled".
- Q2: Should the exporter also push to webnote's pipeline for cross-validation, or is single-source-of-truth enough? Default: single source.
- Q3: Do we re-run summarization through Claude if the Ollama summary fails the 5-section structure check? Default: yes, with a 30s timeout and fallback to "Notes" dump.

## Not in scope for v1

- Speaker identification by voice (name matching).
- Audio retention.
- Editing transcript before export.
- Multi-language transcript handling.
