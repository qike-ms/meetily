"""Shared fixtures for meetily-server tests.

Every test gets a fresh in-memory-like SQLite database (temp file)
and an HTTPX AsyncClient wired to the real FastAPI app.
"""

import os
import tempfile

import pytest
import pytest_asyncio
from httpx import ASGITransport, AsyncClient

# Override DB path BEFORE any app import so init_db targets a temp file.
_TMP_DB = tempfile.NamedTemporaryFile(suffix=".db", delete=False)
os.environ["MEETILY_SERVER_DATABASE_PATH"] = _TMP_DB.name
_TMP_DB.close()

from server.app import app  # noqa: E402
from server.database import init_db  # noqa: E402


@pytest_asyncio.fixture(autouse=True)
async def _fresh_db():
    """Wipe and recreate all tables before every test."""
    import aiosqlite

    db_path = os.environ["MEETILY_SERVER_DATABASE_PATH"]
    async with aiosqlite.connect(db_path) as db:
        await db.executescript(
            """
            DROP TABLE IF EXISTS transcript_fts;
            DROP TRIGGER IF EXISTS transcript_fts_insert;
            DROP TRIGGER IF EXISTS transcript_fts_delete;
            DROP TRIGGER IF EXISTS transcript_fts_update;
            DROP TABLE IF EXISTS summaries;
            DROP TABLE IF EXISTS transcript_segments;
            DROP TABLE IF EXISTS meetings;
            """
        )
        await db.commit()
    await init_db()
    yield


@pytest_asyncio.fixture
async def client():
    """HTTPX async client that talks to the real FastAPI app over ASGI."""
    transport = ASGITransport(app=app)
    async with AsyncClient(transport=transport, base_url="http://test") as c:
        yield c


# -- Shared helpers ----------------------------------------------------------

SAMPLE_SEGMENTS = [
    {
        "timestamp": "00:00:05",
        "text": "Let's discuss the roadmap",
        "source": "system",
        "confidence": 0.95,
        "duration_ms": 2100,
    },
    {
        "timestamp": "00:00:08",
        "text": "Sure, I think we should prioritize API work",
        "source": "mic",
        "confidence": 0.91,
        "duration_ms": 3500,
    },
    {
        "timestamp": "00:00:14",
        "text": "Agreed, and we need to ship by Friday",
        "source": "system",
        "confidence": 0.88,
        "duration_ms": 2800,
    },
]


async def create_meeting(client: AsyncClient, title: str = "Test Meeting", client_id: str | None = None) -> dict:
    payload: dict = {"title": title}
    if client_id:
        payload["client_id"] = client_id
    resp = await client.post("/api/meetings", json=payload)
    assert resp.status_code == 201, resp.text
    return resp.json()


async def upload_segments(client: AsyncClient, meeting_id: str, segments=None) -> dict:
    segments = segments or SAMPLE_SEGMENTS
    resp = await client.post(
        f"/api/meetings/{meeting_id}/transcript",
        json={"segments": segments},
    )
    assert resp.status_code == 201, resp.text
    return resp.json()


# A valid UUID format that doesn't exist in the DB -- use for 404 tests.
# (Invalid UUIDs like "no-such-id" now return 422 due to WI-27 validation.)
NONEXISTENT_UUID = "00000000-0000-4000-a000-000000000000"


async def insert_summary(meeting_id: str, content: str) -> None:
    """Directly insert a summary (bypasses opencode subprocess)."""
    import aiosqlite

    db_path = os.environ["MEETILY_SERVER_DATABASE_PATH"]
    async with aiosqlite.connect(db_path) as db:
        await db.execute("PRAGMA foreign_keys = ON")
        await db.execute(
            "INSERT INTO summaries (meeting_id, content) VALUES (?, ?)",
            (meeting_id, content),
        )
        await db.commit()
