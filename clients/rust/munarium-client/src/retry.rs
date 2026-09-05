// SPDX-License-Identifier: Apache-2.0
//! Retry pacing: jittered exponential backoff without a rand dependency
//! (subsecond clock nanos are jitter enough for pacing).

use std::time::Duration;

/// Jittered backoff for attempt n (1-based): base 2^(n-1) * 100ms, ±50%,
/// clamped to [50ms, 2s].
pub(crate) async fn jitter_sleep(attempt: u32) {
    let base_ms = 100u64.saturating_mul(1 << attempt.saturating_sub(1).min(6));
    let nanos = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|d| d.subsec_nanos() as u64)
        .unwrap_or(0);
    let jitter = nanos % base_ms.max(1);
    let ms = (base_ms / 2 + jitter).clamp(50, 2_000);
    tokio::time::sleep(Duration::from_millis(ms)).await;
}
