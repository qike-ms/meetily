import asyncio
import json
import logging
from contextlib import asynccontextmanager
from datetime import datetime, timezone
from pathlib import Path
from typing import Any, Optional, Sequence
from uuid import uuid4

from fastapi import BackgroundTasks, FastAPI, HTTPException, status
from fastapi.middleware.cors import CORSMiddleware

try:
    from .database import get_db, init_db
    from .models import (
        MeetingCreate,
        MeetingDetailResponse,
        MeetingListResponse,
        MeetingResponse,
        SummaryResponse,
        TranscriptSegmentResponse,
        TranscriptUpload,
    )
except ImportError:  # pragma: no cover - supports running app.py directly.
    from database import get_db, init_db
    from models import (
        MeetingCreate,
        MeetingDetailResponse,
        MeetingListResponse,
        MeetingResponse,
        SummaryResponse,
        TranscriptSegmentResponse,
        TranscriptUpload,
    )


logger = logging.getLogger(__name__)


@asynccontextmanager
async def lifespan(_: FastAPI):
    await init_db()
    yield


app = FastAPI(title="Meetily Server API", lifespan=lifespan)

app.add_middleware(
    CORSMiddleware,
    allow_origins=["*"],
    allow_credentials=True,
    allow_methods=["*"],
    allow_headers=["*"],
)


def _row_to_meeting(row: Sequence[Any]) -> MeetingResponse:
    return MeetingResponse(
        id=row[0],
        title=row[1],
        status=row[2],
        client_id=row[3],
        created_at=row[4],
        ended_at=row[5],
    )


def _row_to_transcript_segment(row: Sequence[Any]) -> TranscriptSegmentResponse:
    return TranscriptSegmentResponse(
        id=row[0],
        meeting_id=row[1],
        timestamp=row[2],
        text=row[3],
        source=row[4],
        confidence=row[5],
        duration_ms=row[6],
    )


def _row_to_summary(row: Sequence[Any]) -> SummaryResponse:
    return SummaryResponse(
        id=row[0],
        meeting_id=row[1],
        content=row[2],
        created_at=row[3],
    )


async def _fetch_meeting(meeting_id: str) -> Optional[MeetingResponse]:
    async with get_db() as db:
        cursor = await db.execute(
            """
            SELECT id, title, status, client_id, created_at, ended_at
            FROM meetings
            WHERE id = ?
            """,
            (meeting_id,),
        )
        row = await cursor.fetchone()
    return _row_to_meeting(row) if row else None


async def _require_meeting(meeting_id: str) -> MeetingResponse:
    meeting = await _fetch_meeting(meeting_id)
    if meeting is None:
        raise HTTPException(status_code=status.HTTP_404_NOT_FOUND, detail="Meeting not found")
    return meeting


async def _fetch_transcript_segments(meeting_id: str) -> list[TranscriptSegmentResponse]:
    async with get_db() as db:
        cursor = await db.execute(
            """
            SELECT id, meeting_id, timestamp, text, source, confidence, duration_ms
            FROM transcript_segments
            WHERE meeting_id = ?
            ORDER BY timestamp IS NULL, timestamp ASC, id ASC
            """,
            (meeting_id,),
        )
        rows = await cursor.fetchall()
    return [_row_to_transcript_segment(row) for row in rows]


async def _fetch_latest_summary(meeting_id: str) -> Optional[SummaryResponse]:
    async with get_db() as db:
        cursor = await db.execute(
            """
            SELECT id, meeting_id, content, created_at
            FROM summaries
            WHERE meeting_id = ?
            ORDER BY created_at DESC, id DESC
            LIMIT 1
            """,
            (meeting_id,),
        )
        row = await cursor.fetchone()
    return _row_to_summary(row) if row else None


def _opencode_path() -> str:
    local_opencode = Path.home() / ".local" / "bin" / "opencode"
    if local_opencode.exists():
        return str(local_opencode)
    return "opencode"


def _extract_text_from_ndjson(stdout: bytes) -> str:
    parts: list[str] = []
    for raw_line in stdout.decode(errors="replace").splitlines():
        line = raw_line.strip()
        if not line:
            continue
        try:
            event = json.loads(line)
        except json.JSONDecodeError:
            logger.warning("Skipping non-JSON opencode output line: %s", line)
            continue

        if event.get("type") != "text":
            continue

        part = event.get("part")
        if isinstance(part, dict) and isinstance(part.get("text"), str):
            parts.append(part["text"])
        elif isinstance(event.get("text"), str):
            parts.append(event["text"])

    return "".join(parts).strip()


async def _summarize_meeting(meeting_id: str) -> None:
    try:
        meeting = await _fetch_meeting(meeting_id)
        if meeting is None:
            logger.warning("Skipping summary for missing meeting %s", meeting_id)
            return

        segments = await _fetch_transcript_segments(meeting_id)
        formatted_text = "\n".join(f"[{segment.source}] {segment.text}" for segment in segments)
        prompt = f"Summarize this meeting transcript:\n\n{formatted_text}"

        process = await asyncio.create_subprocess_exec(
            _opencode_path(),
            "run",
            "--format",
            "json",
            "--pure",
            prompt,
            stdout=asyncio.subprocess.PIPE,
            stderr=asyncio.subprocess.PIPE,
        )
        stdout, stderr = await process.communicate()

        if process.returncode != 0:
            stderr_text = stderr.decode(errors="replace").strip()
            raise RuntimeError(f"opencode failed with exit code {process.returncode}: {stderr_text}")

        summary_text = _extract_text_from_ndjson(stdout)
        if not summary_text:
            raise RuntimeError("opencode completed without text output")

        async with get_db() as db:
            await db.execute(
                """
                INSERT INTO summaries (meeting_id, content)
                VALUES (?, ?)
                """,
                (meeting_id, summary_text),
            )
            await db.commit()
    except Exception:
        logger.exception("Failed to summarize meeting %s", meeting_id)


@app.get("/api/health")
async def health_check() -> dict[str, str]:
    return {"status": "ok"}


@app.post("/api/meetings", response_model=MeetingResponse, status_code=status.HTTP_201_CREATED)
async def create_meeting(payload: MeetingCreate) -> MeetingResponse:
    meeting_id = str(uuid4())
    async with get_db() as db:
        await db.execute(
            """
            INSERT INTO meetings (id, title, client_id)
            VALUES (?, ?, ?)
            """,
            (meeting_id, payload.title, payload.client_id),
        )
        await db.commit()

    meeting = await _fetch_meeting(meeting_id)
    if meeting is None:
        raise HTTPException(status_code=status.HTTP_500_INTERNAL_SERVER_ERROR, detail="Meeting was not created")
    return meeting


@app.get("/api/meetings", response_model=list[MeetingListResponse])
async def list_meetings() -> list[MeetingListResponse]:
    async with get_db() as db:
        cursor = await db.execute(
            """
            SELECT
                m.id,
                m.title,
                m.status,
                m.client_id,
                m.created_at,
                m.ended_at,
                COUNT(ts.id) AS segment_count,
                EXISTS(
                    SELECT 1 FROM summaries s WHERE s.meeting_id = m.id
                ) AS has_summary
            FROM meetings m
            LEFT JOIN transcript_segments ts ON ts.meeting_id = m.id
            GROUP BY m.id, m.title, m.status, m.client_id, m.created_at, m.ended_at
            ORDER BY m.created_at DESC, m.id DESC
            """
        )
        rows = await cursor.fetchall()

    return [
        MeetingListResponse(
            id=row[0],
            title=row[1],
            status=row[2],
            client_id=row[3],
            created_at=row[4],
            ended_at=row[5],
            segment_count=row[6],
            has_summary=bool(row[7]),
        )
        for row in rows
    ]


@app.get("/api/meetings/{meeting_id}", response_model=MeetingDetailResponse)
async def get_meeting(meeting_id: str) -> MeetingDetailResponse:
    meeting = await _require_meeting(meeting_id)
    segments = await _fetch_transcript_segments(meeting_id)
    summary = await _fetch_latest_summary(meeting_id)
    return MeetingDetailResponse(
        **meeting.model_dump(),
        transcript_segments=segments,
        summary=summary,
    )


@app.delete("/api/meetings/{meeting_id}", status_code=status.HTTP_204_NO_CONTENT)
async def delete_meeting(meeting_id: str) -> None:
    async with get_db() as db:
        cursor = await db.execute("DELETE FROM meetings WHERE id = ?", (meeting_id,))
        await db.commit()
        if cursor.rowcount == 0:
            raise HTTPException(status_code=status.HTTP_404_NOT_FOUND, detail="Meeting not found")


@app.post("/api/meetings/{meeting_id}/end", response_model=MeetingResponse)
async def end_meeting(meeting_id: str) -> MeetingResponse:
    ended_at = datetime.now(timezone.utc).isoformat()
    async with get_db() as db:
        cursor = await db.execute(
            """
            UPDATE meetings
            SET status = 'completed', ended_at = ?
            WHERE id = ?
            """,
            (ended_at, meeting_id),
        )
        await db.commit()
        if cursor.rowcount == 0:
            raise HTTPException(status_code=status.HTTP_404_NOT_FOUND, detail="Meeting not found")

    meeting = await _fetch_meeting(meeting_id)
    if meeting is None:
        raise HTTPException(status_code=status.HTTP_404_NOT_FOUND, detail="Meeting not found")
    return meeting


@app.post("/api/meetings/{meeting_id}/transcript", status_code=status.HTTP_201_CREATED)
async def upload_transcript(meeting_id: str, payload: TranscriptUpload) -> dict[str, int | str]:
    await _require_meeting(meeting_id)
    values = [
        (
            meeting_id,
            segment.timestamp,
            segment.text,
            segment.source,
            segment.confidence,
            segment.duration_ms,
        )
        for segment in payload.segments
    ]

    async with get_db() as db:
        await db.executemany(
            """
            INSERT INTO transcript_segments (
                meeting_id, timestamp, text, source, confidence, duration_ms
            )
            VALUES (?, ?, ?, ?, ?, ?)
            """,
            values,
        )
        await db.commit()

    return {"meeting_id": meeting_id, "count": len(values)}


@app.get("/api/meetings/{meeting_id}/transcripts", response_model=list[TranscriptSegmentResponse])
async def get_transcripts(meeting_id: str) -> list[TranscriptSegmentResponse]:
    await _require_meeting(meeting_id)
    return await _fetch_transcript_segments(meeting_id)


@app.post("/api/meetings/{meeting_id}/summarize", status_code=status.HTTP_202_ACCEPTED)
async def summarize_meeting(meeting_id: str, background_tasks: BackgroundTasks) -> dict[str, str]:
    await _require_meeting(meeting_id)
    background_tasks.add_task(_summarize_meeting, meeting_id)
    return {"meeting_id": meeting_id}


@app.get("/api/meetings/{meeting_id}/summary", response_model=SummaryResponse)
async def get_summary(meeting_id: str) -> SummaryResponse:
    await _require_meeting(meeting_id)
    summary = await _fetch_latest_summary(meeting_id)
    if summary is None:
        raise HTTPException(status_code=status.HTTP_404_NOT_FOUND, detail="Summary not found")
    return summary
