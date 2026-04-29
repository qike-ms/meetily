import os
from contextlib import asynccontextmanager
from typing import AsyncIterator

import aiosqlite


DEFAULT_DB_PATH = "meetily_server.db"
DB_PATH = os.getenv("MEETILY_SERVER_DATABASE_PATH", DEFAULT_DB_PATH)

SCHEMA_SQL = """
PRAGMA foreign_keys = ON;

CREATE TABLE IF NOT EXISTS meetings (
    id TEXT PRIMARY KEY,
    title TEXT,
    status TEXT DEFAULT 'recording',
    client_id TEXT,
    created_at TIMESTAMP DEFAULT CURRENT_TIMESTAMP,
    ended_at TIMESTAMP
);

CREATE TABLE IF NOT EXISTS transcript_segments (
    id INTEGER PRIMARY KEY AUTOINCREMENT,
    meeting_id TEXT REFERENCES meetings(id) ON DELETE CASCADE,
    timestamp TEXT,
    text TEXT,
    source TEXT CHECK(source IN ('mic', 'system')),
    confidence REAL,
    duration_ms INTEGER
);

CREATE TABLE IF NOT EXISTS summaries (
    id INTEGER PRIMARY KEY AUTOINCREMENT,
    meeting_id TEXT REFERENCES meetings(id) ON DELETE CASCADE,
    content TEXT,
    created_at TIMESTAMP DEFAULT CURRENT_TIMESTAMP
);

CREATE VIRTUAL TABLE IF NOT EXISTS transcript_fts USING fts5(
    text,
    content=transcript_segments,
    content_rowid=id
);

CREATE TRIGGER IF NOT EXISTS transcript_fts_insert AFTER INSERT ON transcript_segments BEGIN
    INSERT INTO transcript_fts(rowid, text) VALUES (new.id, new.text);
END;

CREATE TRIGGER IF NOT EXISTS transcript_fts_delete AFTER DELETE ON transcript_segments BEGIN
    INSERT INTO transcript_fts(transcript_fts, rowid, text) VALUES('delete', old.id, old.text);
END;

CREATE TRIGGER IF NOT EXISTS transcript_fts_update AFTER UPDATE ON transcript_segments BEGIN
    INSERT INTO transcript_fts(transcript_fts, rowid, text) VALUES('delete', old.id, old.text);
    INSERT INTO transcript_fts(rowid, text) VALUES (new.id, new.text);
END;
"""


@asynccontextmanager
async def get_db(db_path: str = DB_PATH) -> AsyncIterator[aiosqlite.Connection]:
    db = await aiosqlite.connect(db_path)
    await db.execute("PRAGMA journal_mode=WAL")
    await db.execute("PRAGMA busy_timeout=5000")
    await db.execute("PRAGMA foreign_keys = ON")
    try:
        yield db
        await db.commit()
    except BaseException:
        await db.rollback()
        raise
    finally:
        await db.close()


async def init_db(db_path: str = DB_PATH) -> None:
    async with aiosqlite.connect(db_path) as db:
        await db.execute("PRAGMA journal_mode=WAL")
        await db.execute("PRAGMA busy_timeout=5000")
        await db.executescript(SCHEMA_SQL)
        await db.commit()
