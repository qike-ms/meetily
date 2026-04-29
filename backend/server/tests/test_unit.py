"""Unit tests for internal functions -- no HTTP, no database.

Covers: NDJSON parsing, slugify, FTS query builder, date extraction.
"""

import json

import pytest

from server.app import _extract_text_from_ndjson, _fts_query, _meeting_date, _slugify


# ---------------------------------------------------------------------------
# _extract_text_from_ndjson  (WI-3 internals)
# ---------------------------------------------------------------------------

class TestNDJSONParser:
    """Parse the opencode --format json NDJSON event stream."""

    def test_standard_output(self):
        """Extracts text from multiple type:text events."""
        lines = [
            json.dumps({"type": "step_start", "sessionID": "ses_abc", "part": {"type": "step-start"}}),
            json.dumps({"type": "text", "sessionID": "ses_abc", "part": {"type": "text", "text": "Alice wanted Friday, "}}),
            json.dumps({"type": "text", "sessionID": "ses_abc", "part": {"type": "text", "text": "Bob wanted Monday."}}),
            json.dumps({"type": "step_finish", "sessionID": "ses_abc", "part": {"type": "step-finish", "tokens": {}}}),
        ]
        result = _extract_text_from_ndjson("\n".join(lines).encode())
        assert result == "Alice wanted Friday, Bob wanted Monday."

    def test_single_text_event(self):
        line = json.dumps({"type": "text", "part": {"text": "Hello world"}})
        assert _extract_text_from_ndjson(line.encode()) == "Hello world"

    def test_empty_output(self):
        assert _extract_text_from_ndjson(b"") == ""

    def test_no_text_events(self):
        line = json.dumps({"type": "step_start", "part": {"type": "step-start"}})
        assert _extract_text_from_ndjson(line.encode()) == ""

    def test_malformed_json_lines_skipped(self):
        """Non-JSON lines are ignored, valid ones still parsed."""
        output = b"not json at all\n" + json.dumps({"type": "text", "part": {"text": "valid"}}).encode() + b"\n"
        assert _extract_text_from_ndjson(output) == "valid"

    def test_text_with_whitespace(self):
        lines = [
            json.dumps({"type": "text", "part": {"text": "  spaces  "}}),
            json.dumps({"type": "text", "part": {"text": "  around  "}}),
        ]
        result = _extract_text_from_ndjson("\n".join(lines).encode())
        assert result == "spaces    around"  # concatenated then stripped

    def test_fallback_text_field(self):
        """If part.text is missing, falls back to event.text."""
        line = json.dumps({"type": "text", "text": "fallback"})
        assert _extract_text_from_ndjson(line.encode()) == "fallback"

    def test_unicode_output(self):
        line = json.dumps({"type": "text", "part": {"text": "discussion content here"}})
        assert "discussion" in _extract_text_from_ndjson(line.encode())

    def test_tool_call_events_ignored(self):
        """Events like tool_use, tool_result are not text and should be skipped."""
        lines = [
            json.dumps({"type": "tool_use", "part": {"name": "bash", "input": "ls"}}),
            json.dumps({"type": "text", "part": {"text": "Result: 3 files"}}),
            json.dumps({"type": "tool_result", "part": {"output": "foo bar"}}),
        ]
        result = _extract_text_from_ndjson("\n".join(lines).encode())
        assert result == "Result: 3 files"


# ---------------------------------------------------------------------------
# _slugify
# ---------------------------------------------------------------------------

class TestSlugify:
    def test_normal_string(self):
        assert _slugify("Sprint Planning Q4") == "sprint-planning-q4"

    def test_special_chars(self):
        assert _slugify("Hello, World! @#$%") == "hello-world"

    def test_leading_trailing_whitespace(self):
        assert _slugify("  Hello World  ") == "hello-world"

    def test_empty_string(self):
        assert _slugify("") == "untitled-meeting"

    def test_only_special_chars(self):
        assert _slugify("!!!@@@") == "untitled-meeting"

    def test_numbers(self):
        assert _slugify("Meeting 42 on 2026-04-27") == "meeting-42-on-2026-04-27"


# ---------------------------------------------------------------------------
# _meeting_date
# ---------------------------------------------------------------------------

class TestMeetingDate:
    def test_iso_format(self):
        assert _meeting_date("2026-04-27T10:30:00+00:00") == "2026-04-27"

    def test_space_separated(self):
        assert _meeting_date("2026-04-27 10:30:00") == "2026-04-27"

    def test_date_only(self):
        assert _meeting_date("2026-04-27") == "2026-04-27"


# ---------------------------------------------------------------------------
# _fts_query
# ---------------------------------------------------------------------------

class TestFTSQuery:
    def test_single_word(self):
        assert _fts_query("roadmap") == '"roadmap"'

    def test_multiple_words(self):
        result = _fts_query("ship by Friday")
        assert '"ship"' in result
        assert '"by"' in result
        assert '"Friday"' in result

    def test_special_chars_stripped(self):
        result = _fts_query("hello! world?")
        assert '"hello"' in result
        assert '"world"' in result
        assert "!" not in result

    def test_empty_string(self):
        assert _fts_query("") == ""

    def test_only_special_chars(self):
        assert _fts_query("!@#$%") == ""
