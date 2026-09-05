# SPDX-License-Identifier: Apache-2.0
"""Retry pacing: jittered exponential backoff (clock-nanos jitter — no
security requirement, just pacing)."""

from __future__ import annotations

import asyncio
import time


def backoff_seconds(attempt: int) -> float:
    """Jittered backoff for attempt n (1-based): base 2^(n-1) * 100ms, ±50%,
    clamped to [0.05s, 2s]."""
    base_ms = 100 * (1 << min(attempt - 1, 6))
    jitter = time.time_ns() % max(base_ms, 1)
    return min(max((base_ms / 2 + jitter) / 1000.0, 0.05), 2.0)


def sleep_sync(attempt: int) -> None:
    time.sleep(backoff_seconds(attempt))


async def sleep_async(attempt: int) -> None:
    await asyncio.sleep(backoff_seconds(attempt))
