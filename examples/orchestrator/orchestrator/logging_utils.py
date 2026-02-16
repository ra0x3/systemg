"""Logging helpers for compact terminal output."""

from __future__ import annotations

import logging
from typing import Final


class CompactingHandler(logging.Handler):
    """Collapse consecutive duplicate log messages into one counted line."""

    _SUMMARY_PREFIX: Final[str] = "+"

    def __init__(self, delegate: logging.Handler):
        """Wrap a concrete handler that performs the final output."""
        super().__init__(level=delegate.level)
        self.delegate = delegate
        self._pending_record: logging.LogRecord | None = None
        self._pending_message: str | None = None
        self._pending_count = 0

    def emit(self, record: logging.LogRecord) -> None:
        """Buffer and compact adjacent duplicate message bodies."""
        try:
            message = record.getMessage()
            self.acquire()
            if record.levelno >= logging.WARNING:
                self._emit_pending()
                self.delegate.handle(record)
                return
            if self._pending_record is None:
                self._pending_record = record
                self._pending_message = message
                self._pending_count = 1
                return
            if message == self._pending_message:
                self._pending_count += 1
                return
            self._emit_pending()
            self._pending_record = record
            self._pending_message = message
            self._pending_count = 1
        finally:
            self.release()

    def flush(self) -> None:
        """Flush any buffered compacted record and the delegate handler."""
        try:
            self.acquire()
            self._emit_pending()
            self.delegate.flush()
        finally:
            self.release()

    def close(self) -> None:
        """Flush buffered record before closing delegate."""
        try:
            self.flush()
        finally:
            self.delegate.close()
            super().close()

    def _emit_pending(self) -> None:
        """Emit the current pending record, compacted when repeated."""
        if self._pending_record is None:
            return
        if self._pending_count <= 1:
            self.delegate.handle(self._pending_record)
        else:
            summary_record = self._clone_record_with_message(
                self._pending_record,
                f"{self._SUMMARY_PREFIX}{self._pending_count} {self._pending_message}",
            )
            self.delegate.handle(summary_record)
        self._pending_record = None
        self._pending_message = None
        self._pending_count = 0

    @staticmethod
    def _clone_record_with_message(record: logging.LogRecord, message: str) -> logging.LogRecord:
        """Create a record copy that preserves metadata but overrides message text."""
        clone = logging.LogRecord(
            name=record.name,
            level=record.levelno,
            pathname=record.pathname,
            lineno=record.lineno,
            msg=message,
            args=(),
            exc_info=record.exc_info,
            func=record.funcName,
            sinfo=record.stack_info,
        )
        clone.created = record.created
        clone.msecs = record.msecs
        clone.relativeCreated = record.relativeCreated
        clone.thread = record.thread
        clone.threadName = record.threadName
        clone.processName = record.processName
        clone.process = record.process
        return clone
