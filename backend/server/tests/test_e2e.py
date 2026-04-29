"""End-to-end scenario tests.

These test complete user workflows -- not individual endpoints in isolation,
but the full sequences a real client would execute. They hit the actual
FastAPI app with a real SQLite database.
"""

import asyncio
import json
import os
from unittest.mock import AsyncMock, patch

import pytest
from httpx import AsyncClient

from .conftest import SAMPLE_SEGMENTS, create_meeting, insert_summary, upload_segments


class TestE2EFullMeetingLifecycle:
    """Scenario: A user records a meeting on M4, uploads transcript, gets summary on M1."""

    async def test_record_upload_summarize_view(self, client: AsyncClient):
        # 1. Client creates a meeting
        meeting = await create_meeting(client, "Weekly Standup", client_id="m4")
        mid = meeting["id"]
        assert meeting["status"] == "recording"
        assert meeting["client_id"] == "m4"

        # 2. Client uploads transcript segments (batch after recording)
        segments = [
            {"timestamp": "00:00:02", "text": "Good morning everyone", "source": "mic", "confidence": 0.94, "duration_ms": 1500},
            {"timestamp": "00:00:05", "text": "Hey, let's start with updates", "source": "system", "confidence": 0.89, "duration_ms": 2000},
            {"timestamp": "00:00:10", "text": "I finished the API refactor", "source": "mic", "confidence": 0.92, "duration_ms": 2500},
            {"timestamp": "00:00:15", "text": "Great, any blockers?", "source": "system", "confidence": 0.91, "duration_ms": 1200},
            {"timestamp": "00:00:18", "text": "Just waiting on the database migration", "source": "mic", "confidence": 0.87, "duration_ms": 3000},
            {"timestamp": "00:00:25", "text": "Let's get that done by Friday", "source": "system", "confidence": 0.93, "duration_ms": 2000},
        ]
        result = await upload_segments(client, mid, segments)
        assert result["count"] == 6

        # 3. Client ends the meeting
        resp = await client.post(f"/api/meetings/{mid}/end")
        assert resp.status_code == 200
        assert resp.json()["status"] == "completed"

        # 4. Client triggers summarization (mock opencode subprocess)
        ndjson_lines = [
            json.dumps({"type": "step_start", "sessionID": "ses_e2e", "part": {"type": "step-start"}}),
            json.dumps({"type": "text", "sessionID": "ses_e2e", "part": {"type": "text", "text": "The team discussed API refactor progress. "}}),
            json.dumps({"type": "text", "sessionID": "ses_e2e", "part": {"type": "text", "text": "One blocker: database migration pending. Target: Friday."}}),
            json.dumps({"type": "step_finish", "sessionID": "ses_e2e", "part": {"type": "step-finish"}}),
        ]
        mock_proc = AsyncMock()
        mock_proc.communicate = AsyncMock(return_value=("\n".join(ndjson_lines).encode(), b""))
        mock_proc.returncode = 0

        with patch("server.app.asyncio.create_subprocess_exec", return_value=mock_proc) as mock_exec:
            with patch("server.app.export_to_obsidian", new_callable=AsyncMock):
                from server.app import _summarize_meeting
                await _summarize_meeting(mid)

        # Verify opencode was called with the right prompt
        mock_exec.assert_called_once()
        call_args = mock_exec.call_args[0]
        assert "opencode" in call_args[0]
        assert "run" in call_args
        assert "--format" in call_args
        assert "--pure" in call_args
        prompt_arg = call_args[-1]
        assert "[mic]" in prompt_arg  # source tags present
        assert "[system]" in prompt_arg
        assert "API refactor" in prompt_arg  # transcript content present

        # 5. Web UI fetches meeting detail -- sees transcript + summary
        resp = await client.get(f"/api/meetings/{mid}")
        assert resp.status_code == 200
        detail = resp.json()
        assert detail["status"] == "completed"
        assert detail["client_id"] == "m4"
        assert len(detail["transcript_segments"]) == 6
        assert detail["summary"]["content"] == (
            "The team discussed API refactor progress. "
            "One blocker: database migration pending. Target: Friday."
        )

        # 6. Segments are correctly source-tagged
        mic_segs = [s for s in detail["transcript_segments"] if s["source"] == "mic"]
        sys_segs = [s for s in detail["transcript_segments"] if s["source"] == "system"]
        assert len(mic_segs) == 3
        assert len(sys_segs) == 3

        # 7. Meeting appears in list with correct metadata
        resp = await client.get("/api/meetings")
        listing = resp.json()
        assert len(listing) == 1
        assert listing[0]["id"] == mid
        assert listing[0]["segment_count"] == 6
        assert listing[0]["has_summary"] is True


class TestE2ESearchAcrossMeetings:
    """Scenario: User searches across multiple meetings for specific topics."""

    async def test_search_finds_content_across_meetings(self, client: AsyncClient):
        # Create two meetings with distinct content
        m1 = await create_meeting(client, "API Design Review")
        m2 = await create_meeting(client, "Sprint Retro")
        m3 = await create_meeting(client, "Unrelated Meeting")

        await upload_segments(client, m1["id"], [
            {"text": "The authentication endpoint needs rate limiting", "source": "mic", "timestamp": "00:00:10"},
            {"text": "Agreed, and we should add OAuth support", "source": "system", "timestamp": "00:00:15"},
        ])
        await upload_segments(client, m2["id"], [
            {"text": "Authentication was the biggest blocker this sprint", "source": "mic", "timestamp": "00:00:05"},
            {"text": "We should allocate more time for security features", "source": "system", "timestamp": "00:00:10"},
        ])
        await upload_segments(client, m3["id"], [
            {"text": "The new office space is really nice", "source": "mic", "timestamp": "00:00:05"},
        ])

        # Search for "authentication" -- should find m1 and m2, not m3
        resp = await client.get("/api/search?q=authentication")
        results = resp.json()
        found_ids = {r["meeting_id"] for r in results}
        assert m1["id"] in found_ids
        assert m2["id"] in found_ids
        assert m3["id"] not in found_ids

        # Each result has meeting title and snippet
        for r in results:
            assert r["meeting_title"] in ("API Design Review", "Sprint Retro")
            assert "authentication" in r["snippet"].lower() or "<mark>" in r["snippet"]


class TestE2EDeleteCascade:
    """Scenario: Deleting a meeting removes everything -- segments, summaries, FTS index."""

    async def test_delete_removes_all_associated_data(self, client: AsyncClient):
        m = await create_meeting(client, "To Be Deleted")
        mid = m["id"]
        await upload_segments(client, mid, [
            {"text": "searchable unique content zxcv123", "source": "mic"},
        ])
        await insert_summary(mid, "Summary for deletion test")

        # Verify data exists
        assert (await client.get(f"/api/meetings/{mid}/summary")).status_code == 200
        assert len((await client.get(f"/api/meetings/{mid}/transcripts")).json()) == 1
        assert len((await client.get("/api/search?q=zxcv123")).json()) >= 1

        # Delete
        assert (await client.delete(f"/api/meetings/{mid}")).status_code == 204

        # Everything is gone
        assert (await client.get(f"/api/meetings/{mid}")).status_code == 404
        # FTS index should also be clean (no stale entries)
        assert (await client.get("/api/search?q=zxcv123")).json() == []


class TestE2EConcurrentMeetings:
    """Scenario: Multiple clients recording simultaneously."""

    async def test_concurrent_meetings_isolated(self, client: AsyncClient):
        # Two clients start meetings at the same time
        m_m4 = await create_meeting(client, "M4 Meeting", client_id="m4")
        m_i7 = await create_meeting(client, "i7 Meeting", client_id="i7")

        # Each uploads their own segments
        await upload_segments(client, m_m4["id"], [
            {"text": "Hello from M4", "source": "mic", "timestamp": "00:00:01"},
        ])
        await upload_segments(client, m_i7["id"], [
            {"text": "Hello from i7", "source": "mic", "timestamp": "00:00:01"},
            {"text": "Remote person on i7 call", "source": "system", "timestamp": "00:00:03"},
        ])

        # Verify isolation -- each meeting has only its own segments
        m4_segs = (await client.get(f"/api/meetings/{m_m4['id']}/transcripts")).json()
        i7_segs = (await client.get(f"/api/meetings/{m_i7['id']}/transcripts")).json()
        assert len(m4_segs) == 1
        assert len(i7_segs) == 2
        assert m4_segs[0]["text"] == "Hello from M4"
        assert any(s["text"] == "Remote person on i7 call" for s in i7_segs)

        # List shows both with correct client_id
        listing = (await client.get("/api/meetings")).json()
        client_ids = {m["client_id"] for m in listing}
        assert "m4" in client_ids
        assert "i7" in client_ids


class TestE2ELargeTranscript:
    """Scenario: Hour-long meeting with 1000+ segments."""

    async def test_large_transcript_upload_and_search(self, client: AsyncClient):
        m = await create_meeting(client, "All Hands")
        segments = [
            {
                "text": f"Discussion point {i} about the quarterly results" if i % 10 == 0 else f"Segment {i} general discussion",
                "source": "mic" if i % 2 == 0 else "system",
                "timestamp": f"{i // 3600:02d}:{(i % 3600) // 60:02d}:{i % 60:02d}",
                "confidence": 0.85 + (i % 10) * 0.01,
                "duration_ms": 1500 + (i % 5) * 200,
            }
            for i in range(500)
        ]
        result = await upload_segments(client, m["id"], segments)
        assert result["count"] == 500

        # Verify retrieval
        resp = await client.get(f"/api/meetings/{m['id']}/transcripts")
        assert len(resp.json()) == 500

        # Search works on large dataset
        resp = await client.get("/api/search?q=quarterly")
        assert len(resp.json()) >= 1

        # Meeting list shows correct count
        listing = (await client.get("/api/meetings")).json()
        assert listing[0]["segment_count"] == 500


class TestE2EObsidianExportIntegration:
    """Scenario: Summarization triggers Obsidian export end-to-end."""

    async def test_summarize_triggers_export(self, client: AsyncClient):
        import tempfile
        from pathlib import Path

        m = await create_meeting(client, "Export E2E Test")
        await upload_segments(client, m["id"])

        ndjson = json.dumps({"type": "text", "part": {"text": "Team aligned on priorities."}})
        mock_proc = AsyncMock()
        mock_proc.communicate = AsyncMock(return_value=(ndjson.encode(), b""))
        mock_proc.returncode = 0

        with tempfile.TemporaryDirectory() as tmpdir:
            os.environ["OBSIDIAN_VAULT_PATH"] = tmpdir
            with patch("server.app.asyncio.create_subprocess_exec", return_value=mock_proc):
                with patch("server.app._run_vault_git_sync"):
                    from server.app import _summarize_meeting
                    await _summarize_meeting(m["id"])

            # Verify: summary in DB
            resp = await client.get(f"/api/meetings/{m['id']}/summary")
            assert resp.status_code == 200
            assert resp.json()["content"] == "Team aligned on priorities."

            # Verify: markdown file in vault
            files = list((Path(tmpdir) / "projects" / "meetily" / "meetings").glob("*.md"))
            assert len(files) == 1
            content = files[0].read_text()
            assert "Team aligned on priorities." in content
            assert "[mic]" in content or "[system]" in content  # transcript labels
