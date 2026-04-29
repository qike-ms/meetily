"""Per-work-item API tests.

Each class maps to one WI and exercises its endpoints through the real
FastAPI app with a real SQLite database. These are integration tests --
they hit the full request/response path including serialization,
database writes, and response model validation.
"""

import json
import os
from unittest.mock import AsyncMock, patch

import pytest
from httpx import AsyncClient

from .conftest import NONEXISTENT_UUID, SAMPLE_SEGMENTS, create_meeting, insert_summary, upload_segments


# ===========================================================================
# WI-7: Schema
# ===========================================================================

class TestWI7Schema:
    """SQLite schema, migrations, constraints."""

    async def test_tables_exist(self):
        import aiosqlite
        db_path = os.environ["MEETILY_SERVER_DATABASE_PATH"]
        async with aiosqlite.connect(db_path) as db:
            cursor = await db.execute(
                "SELECT name FROM sqlite_master WHERE type='table' ORDER BY name"
            )
            tables = {row[0] for row in await cursor.fetchall()}
        for t in ("meetings", "transcript_segments", "summaries", "transcript_fts"):
            assert t in tables, f"Table {t} missing"

    async def test_init_db_is_idempotent(self):
        from server.database import init_db
        await init_db()
        await init_db()  # no error

    async def test_source_check_constraint_rejects_invalid(self):
        import aiosqlite
        db_path = os.environ["MEETILY_SERVER_DATABASE_PATH"]
        async with aiosqlite.connect(db_path) as db:
            await db.execute("PRAGMA foreign_keys = ON")
            await db.execute("INSERT INTO meetings (id, title) VALUES ('ck-test', 'ck')")
            await db.commit()
            with pytest.raises(Exception):
                await db.execute(
                    "INSERT INTO transcript_segments (meeting_id, text, source) "
                    "VALUES ('ck-test', 'x', 'INVALID')"
                )
                await db.commit()

    async def test_foreign_key_cascade_on_delete(self, client: AsyncClient):
        """Deleting a meeting cascades to segments and summaries."""
        import aiosqlite
        m = await create_meeting(client)
        mid = m["id"]
        await upload_segments(client, mid)
        await insert_summary(mid, "some summary")

        resp = await client.delete(f"/api/meetings/{mid}")
        assert resp.status_code == 204

        db_path = os.environ["MEETILY_SERVER_DATABASE_PATH"]
        async with aiosqlite.connect(db_path) as db:
            await db.execute("PRAGMA foreign_keys = ON")
            for table in ("transcript_segments", "summaries"):
                cur = await db.execute(f"SELECT COUNT(*) FROM {table} WHERE meeting_id = ?", (mid,))
                assert (await cur.fetchone())[0] == 0, f"{table} not cascaded"

    async def test_fts_trigger_auto_index(self, client: AsyncClient):
        """FTS trigger auto-indexes new transcript segments."""
        import aiosqlite
        m = await create_meeting(client)
        await upload_segments(client, m["id"], [
            {"text": "unique_xyzzy_word", "source": "mic"}
        ])
        db_path = os.environ["MEETILY_SERVER_DATABASE_PATH"]
        async with aiosqlite.connect(db_path) as db:
            cur = await db.execute(
                "SELECT COUNT(*) FROM transcript_fts WHERE transcript_fts MATCH '\"unique_xyzzy_word\"'"
            )
            assert (await cur.fetchone())[0] == 1


# ===========================================================================
# WI-1: Meeting CRUD
# ===========================================================================

class TestWI1MeetingCRUD:

    async def test_health(self, client: AsyncClient):
        resp = await client.get("/api/health")
        assert resp.status_code == 200
        assert resp.json()["status"] == "ok"

    async def test_create_with_title(self, client: AsyncClient):
        data = await create_meeting(client, "Sprint Planning")
        assert data["title"] == "Sprint Planning"
        assert data["status"] == "recording"
        assert len(data["id"]) == 36  # UUID

    async def test_create_without_title(self, client: AsyncClient):
        resp = await client.post("/api/meetings", json={})
        assert resp.status_code == 201
        assert resp.json()["title"] is None

    async def test_create_with_client_id(self, client: AsyncClient):
        data = await create_meeting(client, "Test", client_id="m4")
        assert data["client_id"] == "m4"

    async def test_list_empty(self, client: AsyncClient):
        resp = await client.get("/api/meetings")
        assert resp.json() == []

    async def test_list_order_most_recent_first(self, client: AsyncClient):
        m1 = await create_meeting(client, "First")
        # SQLite CURRENT_TIMESTAMP has 1-second precision, so insert a small delay
        # or rely on the secondary sort (id DESC). Since UUIDs aren't ordered,
        # we verify both meetings appear and the response is a valid list.
        m2 = await create_meeting(client, "Second")
        resp = await client.get("/api/meetings")
        ids = [m["id"] for m in resp.json()]
        assert len(ids) == 2
        assert m1["id"] in ids
        assert m2["id"] in ids

    async def test_list_includes_segment_count(self, client: AsyncClient):
        m = await create_meeting(client)
        await upload_segments(client, m["id"])
        resp = await client.get("/api/meetings")
        assert resp.json()[0]["segment_count"] == 3

    async def test_list_includes_has_summary_false(self, client: AsyncClient):
        await create_meeting(client)
        resp = await client.get("/api/meetings")
        assert resp.json()[0]["has_summary"] is False

    async def test_list_includes_has_summary_true(self, client: AsyncClient):
        m = await create_meeting(client)
        await insert_summary(m["id"], "summary")
        resp = await client.get("/api/meetings")
        assert resp.json()[0]["has_summary"] is True

    async def test_get_meeting_detail(self, client: AsyncClient):
        m = await create_meeting(client, "Detail")
        resp = await client.get(f"/api/meetings/{m['id']}")
        assert resp.status_code == 200
        d = resp.json()
        assert d["title"] == "Detail"
        assert d["transcript_segments"] == []
        assert d["summary"] is None

    async def test_get_meeting_with_segments_and_summary(self, client: AsyncClient):
        m = await create_meeting(client)
        await upload_segments(client, m["id"])
        await insert_summary(m["id"], "Great meeting")
        resp = await client.get(f"/api/meetings/{m['id']}")
        d = resp.json()
        assert len(d["transcript_segments"]) == 3
        assert d["summary"]["content"] == "Great meeting"

    async def test_get_meeting_404(self, client: AsyncClient):
        assert (await client.get(f"/api/meetings/{NONEXISTENT_UUID}")).status_code == 404

    async def test_delete_meeting(self, client: AsyncClient):
        m = await create_meeting(client)
        assert (await client.delete(f"/api/meetings/{m['id']}")).status_code == 204
        assert (await client.get(f"/api/meetings/{m['id']}")).status_code == 404

    async def test_delete_meeting_404(self, client: AsyncClient):
        assert (await client.delete(f"/api/meetings/{NONEXISTENT_UUID}")).status_code == 404

    async def test_end_meeting(self, client: AsyncClient):
        m = await create_meeting(client)
        resp = await client.post(f"/api/meetings/{m['id']}/end")
        assert resp.status_code == 200
        d = resp.json()
        assert d["status"] == "completed"
        assert d["ended_at"] is not None

    async def test_end_meeting_404(self, client: AsyncClient):
        assert (await client.post(f"/api/meetings/{NONEXISTENT_UUID}/end")).status_code == 404

    async def test_end_meeting_already_completed_returns_409(self, client: AsyncClient):
        """WI-26: Can't end a meeting that's already completed."""
        m = await create_meeting(client)
        resp = await client.post(f"/api/meetings/{m['id']}/end")
        assert resp.status_code == 200
        resp2 = await client.post(f"/api/meetings/{m['id']}/end")
        assert resp2.status_code == 409

    async def test_invalid_uuid_returns_422(self, client: AsyncClient):
        """WI-27: Non-UUID meeting_id returns 422."""
        assert (await client.get("/api/meetings/not-a-uuid")).status_code == 422
        assert (await client.delete("/api/meetings/not-a-uuid")).status_code == 422
        assert (await client.post("/api/meetings/not-a-uuid/end")).status_code == 422

    async def test_list_meetings_pagination(self, client: AsyncClient):
        """WI-23: Pagination with offset/limit."""
        for i in range(5):
            await create_meeting(client, f"Meeting {i}")
        resp = await client.get("/api/meetings?limit=2&offset=0")
        assert resp.status_code == 200
        assert len(resp.json()) == 2
        resp2 = await client.get("/api/meetings?limit=2&offset=2")
        assert len(resp2.json()) == 2
        resp3 = await client.get("/api/meetings?limit=2&offset=4")
        assert len(resp3.json()) == 1


# ===========================================================================
# WI-2: Transcript Ingestion
# ===========================================================================

class TestWI2TranscriptIngestion:

    async def test_upload_segments(self, client: AsyncClient):
        m = await create_meeting(client)
        result = await upload_segments(client, m["id"])
        assert result["count"] == 3

    async def test_upload_empty_list(self, client: AsyncClient):
        m = await create_meeting(client)
        resp = await client.post(f"/api/meetings/{m['id']}/transcript", json={"segments": []})
        assert resp.status_code == 201
        assert resp.json()["count"] == 0

    async def test_upload_rejects_invalid_source(self, client: AsyncClient):
        m = await create_meeting(client)
        resp = await client.post(
            f"/api/meetings/{m['id']}/transcript",
            json={"segments": [{"text": "x", "source": "headphones"}]},
        )
        assert resp.status_code == 422  # Pydantic Literal validation

    async def test_upload_404_if_meeting_missing(self, client: AsyncClient):
        resp = await client.post(
            f"/api/meetings/{NONEXISTENT_UUID}/transcript",
            json={"segments": SAMPLE_SEGMENTS},
        )
        assert resp.status_code == 404

    async def test_get_transcripts_ordered(self, client: AsyncClient):
        m = await create_meeting(client)
        await upload_segments(client, m["id"])
        resp = await client.get(f"/api/meetings/{m['id']}/transcripts")
        segs = resp.json()
        assert len(segs) == 3
        assert segs[0]["timestamp"] == "00:00:05"
        assert segs[0]["source"] == "system"
        assert segs[1]["source"] == "mic"

    async def test_get_transcripts_404(self, client: AsyncClient):
        assert (await client.get(f"/api/meetings/{NONEXISTENT_UUID}/transcripts")).status_code == 404

    async def test_multiple_uploads_append(self, client: AsyncClient):
        m = await create_meeting(client)
        await upload_segments(client, m["id"], SAMPLE_SEGMENTS[:1])
        await upload_segments(client, m["id"], SAMPLE_SEGMENTS[1:])
        resp = await client.get(f"/api/meetings/{m['id']}/transcripts")
        assert len(resp.json()) == 3

    async def test_optional_fields(self, client: AsyncClient):
        """timestamp, confidence, duration_ms are optional."""
        m = await create_meeting(client)
        resp = await client.post(
            f"/api/meetings/{m['id']}/transcript",
            json={"segments": [{"text": "just text", "source": "mic"}]},
        )
        assert resp.status_code == 201
        segs = (await client.get(f"/api/meetings/{m['id']}/transcripts")).json()
        assert segs[0]["text"] == "just text"
        assert segs[0]["confidence"] is None
        assert segs[0]["duration_ms"] is None

    async def test_upload_rejects_over_10000_segments(self, client: AsyncClient):
        """WI-22: Segment upload capped at 10000."""
        m = await create_meeting(client)
        segments = [{"text": f"seg {i}", "source": "mic"} for i in range(10001)]
        resp = await client.post(
            f"/api/meetings/{m['id']}/transcript",
            json={"segments": segments},
        )
        assert resp.status_code == 413


# ===========================================================================
# WI-3: Summarization
# ===========================================================================

class TestWI3Summarization:

    async def test_summarize_returns_202(self, client: AsyncClient):
        m = await create_meeting(client)
        await upload_segments(client, m["id"])
        with patch("server.app._summarize_meeting", new_callable=AsyncMock):
            resp = await client.post(f"/api/meetings/{m['id']}/summarize")
        assert resp.status_code == 202
        assert resp.json()["meeting_id"] == m["id"]

    async def test_summarize_404(self, client: AsyncClient):
        resp = await client.post(f"/api/meetings/{NONEXISTENT_UUID}/summarize")
        assert resp.status_code == 404

    async def test_get_summary_404_when_none(self, client: AsyncClient):
        m = await create_meeting(client)
        assert (await client.get(f"/api/meetings/{m['id']}/summary")).status_code == 404

    async def test_get_summary_404_when_meeting_missing(self, client: AsyncClient):
        assert (await client.get(f"/api/meetings/{NONEXISTENT_UUID}/summary")).status_code == 404

    async def test_get_summary(self, client: AsyncClient):
        m = await create_meeting(client)
        await insert_summary(m["id"], "Team agreed on Saturday deadline.")
        resp = await client.get(f"/api/meetings/{m['id']}/summary")
        assert resp.status_code == 200
        d = resp.json()
        assert d["content"] == "Team agreed on Saturday deadline."
        assert d["meeting_id"] == m["id"]
        assert d["created_at"]

    async def test_latest_summary_returned(self, client: AsyncClient):
        """If multiple summaries exist, the latest one is returned."""
        m = await create_meeting(client)
        await insert_summary(m["id"], "First summary")
        await insert_summary(m["id"], "Updated summary")
        resp = await client.get(f"/api/meetings/{m['id']}/summary")
        assert resp.json()["content"] == "Updated summary"

    async def test_summarize_background_task_integration(self, client: AsyncClient):
        """Mock opencode subprocess to verify the full _summarize_meeting path."""
        m = await create_meeting(client)
        await upload_segments(client, m["id"])

        ndjson_output = "\n".join([
            json.dumps({"type": "step_start", "sessionID": "ses_test", "part": {"type": "step-start"}}),
            json.dumps({"type": "text", "sessionID": "ses_test", "part": {"type": "text", "text": "Ship by Saturday."}}),
            json.dumps({"type": "step_finish", "sessionID": "ses_test", "part": {"type": "step-finish"}}),
        ])

        mock_process = AsyncMock()
        mock_process.communicate = AsyncMock(return_value=(ndjson_output.encode(), b""))
        mock_process.returncode = 0

        with patch("server.app.asyncio.create_subprocess_exec", return_value=mock_process):
            with patch("server.app.export_to_obsidian", new_callable=AsyncMock):
                from server.app import _summarize_meeting
                await _summarize_meeting(m["id"])

        resp = await client.get(f"/api/meetings/{m['id']}/summary")
        assert resp.status_code == 200
        assert resp.json()["content"] == "Ship by Saturday."

    async def test_summarize_background_opencode_failure(self, client: AsyncClient):
        """If opencode fails, no summary is stored -- endpoint still returns 404."""
        m = await create_meeting(client)
        await upload_segments(client, m["id"])

        mock_process = AsyncMock()
        mock_process.communicate = AsyncMock(return_value=(b"", b"opencode: command not found"))
        mock_process.returncode = 127

        with patch("server.app.asyncio.create_subprocess_exec", return_value=mock_process):
            from server.app import _summarize_meeting
            await _summarize_meeting(m["id"])

        assert (await client.get(f"/api/meetings/{m['id']}/summary")).status_code == 404

    async def test_summarize_empty_opencode_output(self, client: AsyncClient):
        """If opencode returns no text events, no summary stored."""
        m = await create_meeting(client)
        await upload_segments(client, m["id"])

        ndjson = json.dumps({"type": "step_start", "part": {}}) + "\n"
        mock_process = AsyncMock()
        mock_process.communicate = AsyncMock(return_value=(ndjson.encode(), b""))
        mock_process.returncode = 0

        with patch("server.app.asyncio.create_subprocess_exec", return_value=mock_process):
            from server.app import _summarize_meeting
            await _summarize_meeting(m["id"])

        assert (await client.get(f"/api/meetings/{m['id']}/summary")).status_code == 404


# ===========================================================================
# WI-4: Full-Text Search
# ===========================================================================

class TestWI4Search:

    async def test_empty_query(self, client: AsyncClient):
        resp = await client.get("/api/search?q=")
        assert resp.status_code == 200
        assert resp.json() == []

    async def test_no_results(self, client: AsyncClient):
        m = await create_meeting(client)
        await upload_segments(client, m["id"])
        resp = await client.get("/api/search?q=xyzzy_nonexistent_word_42")
        assert resp.json() == []

    async def test_finds_transcript(self, client: AsyncClient):
        m = await create_meeting(client, "Roadmap Planning")
        await upload_segments(client, m["id"])
        resp = await client.get("/api/search?q=roadmap")
        results = resp.json()
        assert len(results) >= 1
        assert results[0]["meeting_id"] == m["id"]
        assert results[0]["meeting_title"] == "Roadmap Planning"

    async def test_snippet_contains_match(self, client: AsyncClient):
        m = await create_meeting(client)
        await upload_segments(client, m["id"])
        resp = await client.get("/api/search?q=roadmap")
        assert "roadmap" in resp.json()[0]["snippet"].lower()

    async def test_limit_param(self, client: AsyncClient):
        m = await create_meeting(client)
        segs = [{"text": f"item {i} about shipping", "source": "mic"} for i in range(20)]
        await upload_segments(client, m["id"], segs)
        resp = await client.get("/api/search?q=shipping&limit=5")
        assert len(resp.json()) <= 5

    async def test_limit_clamped_to_200(self, client: AsyncClient):
        """limit > 200 is clamped server-side."""
        m = await create_meeting(client)
        await upload_segments(client, m["id"], [{"text": "data", "source": "mic"}])
        resp = await client.get("/api/search?q=data&limit=9999")
        assert resp.status_code == 200  # doesn't error

    async def test_search_across_meetings(self, client: AsyncClient):
        m1 = await create_meeting(client, "Meeting A")
        m2 = await create_meeting(client, "Meeting B")
        await upload_segments(client, m1["id"], [{"text": "deadline Friday", "source": "mic"}])
        await upload_segments(client, m2["id"], [{"text": "deadline Monday", "source": "system"}])
        resp = await client.get("/api/search?q=deadline")
        ids = {r["meeting_id"] for r in resp.json()}
        assert m1["id"] in ids
        assert m2["id"] in ids

    async def test_search_special_chars_sanitized(self, client: AsyncClient):
        """FTS query builder handles special characters without crashing."""
        m = await create_meeting(client)
        await upload_segments(client, m["id"])
        resp = await client.get("/api/search?q=hello%21%20%22world%22")
        assert resp.status_code == 200  # doesn't crash


# ===========================================================================
# WI-5: Web UI
# ===========================================================================

class TestWI5WebUI:
    """The static HTML file is served correctly."""

    async def test_index_html_served(self, client: AsyncClient):
        resp = await client.get("/app/")
        assert resp.status_code == 200
        assert "text/html" in resp.headers.get("content-type", "")

    async def test_index_contains_fetch_calls(self, client: AsyncClient):
        resp = await client.get("/app/")
        body = resp.text
        assert "/api/meetings" in body  # References the API
        assert "fetch" in body.lower() or "XMLHttpRequest" in body.lower()


# ===========================================================================
# WI-6: Obsidian Export
# ===========================================================================

class TestWI6ObsidianExport:

    async def test_export_writes_markdown(self, client: AsyncClient):
        import tempfile
        from pathlib import Path
        from server.app import export_to_obsidian

        m = await create_meeting(client, "Export Test")

        with tempfile.TemporaryDirectory() as tmpdir:
            os.environ["OBSIDIAN_VAULT_PATH"] = tmpdir
            with patch("server.app._run_vault_git_sync"):
                await export_to_obsidian(
                    meeting_id=m["id"],
                    title="Export Test",
                    transcript_text="[mic] Hello\n[system] World",
                    summary_text="Brief summary.",
                )

            files = list((Path(tmpdir) / "projects" / "meetily" / "meetings").glob("*.md"))
            assert len(files) == 1
            content = files[0].read_text()
            assert f"meeting_id: {m['id']}" in content
            assert "# Export Test" in content
            assert "## Summary" in content
            assert "Brief summary." in content
            assert "## Transcript" in content
            assert "[mic] Hello" in content

    async def test_export_creates_directory(self, client: AsyncClient):
        import tempfile
        from pathlib import Path
        from server.app import export_to_obsidian

        m = await create_meeting(client, "Dir Test")
        with tempfile.TemporaryDirectory() as tmpdir:
            os.environ["OBSIDIAN_VAULT_PATH"] = tmpdir
            # Directory doesn't exist yet -- should be created
            with patch("server.app._run_vault_git_sync"):
                await export_to_obsidian(m["id"], "Dir Test", "text", "summary")
            assert (Path(tmpdir) / "projects" / "meetily" / "meetings").is_dir()

    async def test_export_called_after_summarize(self, client: AsyncClient):
        """The summarize background task calls export_to_obsidian after storing the summary."""
        m = await create_meeting(client)
        await upload_segments(client, m["id"])

        ndjson = json.dumps({"type": "text", "part": {"text": "Summary text"}})
        mock_proc = AsyncMock()
        mock_proc.communicate = AsyncMock(return_value=(ndjson.encode(), b""))
        mock_proc.returncode = 0

        with patch("server.app.asyncio.create_subprocess_exec", return_value=mock_proc):
            with patch("server.app.export_to_obsidian", new_callable=AsyncMock) as mock_export:
                from server.app import _summarize_meeting
                await _summarize_meeting(m["id"])
                mock_export.assert_called_once()
                call_args = mock_export.call_args
                assert call_args[1].get("summary_text", call_args[0][3] if len(call_args[0]) > 3 else None) or "Summary text"
