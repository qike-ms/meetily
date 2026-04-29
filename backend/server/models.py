from typing import List, Literal, Optional

from pydantic import BaseModel


class MeetingCreate(BaseModel):
    title: Optional[str] = None
    client_id: Optional[str] = None


class MeetingResponse(BaseModel):
    id: str
    title: Optional[str] = None
    status: str
    client_id: Optional[str] = None
    created_at: str
    ended_at: Optional[str] = None


class TranscriptSegment(BaseModel):
    timestamp: Optional[str] = None
    text: str
    source: Literal["mic", "system"]
    confidence: Optional[float] = None
    duration_ms: Optional[int] = None


class TranscriptSegmentResponse(TranscriptSegment):
    id: int
    meeting_id: str


class TranscriptUpload(BaseModel):
    segments: List[TranscriptSegment]


class SummaryResponse(BaseModel):
    id: int
    meeting_id: str
    content: Optional[str] = None
    created_at: str


class SearchResult(BaseModel):
    meeting_id: str
    meeting_title: Optional[str] = None
    snippet: str
    timestamp: Optional[str] = None


class MeetingListResponse(MeetingResponse):
    segment_count: int
    has_summary: bool


class MeetingDetailResponse(MeetingResponse):
    transcript_segments: List[TranscriptSegmentResponse]
    summary: Optional[SummaryResponse] = None
