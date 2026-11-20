// SPDX-License-Identifier: Apache-2.0
//! Linux limits for the extraction child: `setrlimit`, applied by the child
//! to itself before it reads its input.
//!
//! Soft and hard limits are both set, and an unprivileged process cannot
//! raise a hard limit, so nothing the parser does afterwards can lift them.
//! The system call goes through the `rlimit` crate's safe wrapper (pinned in
//! this crate's `Cargo.toml`, with its provenance), so this module holds no
//! `unsafe` code. The mechanism is unchanged from the hand-written binding it
//! replaces: the same three resources and values, so the identity's
//! `os=rlimit` still describes the enforcement.

use rlimit::Resource;

/// No core dumps (a dump would write tenant content to disk), CPU time with
/// SIGXCPU at the limit and SIGKILL one second later, and address space.
pub(super) fn apply(memory_bytes: u64, cpu_seconds: u64) -> std::io::Result<()> {
    rlimit::setrlimit(Resource::CORE, 0, 0)?;
    rlimit::setrlimit(Resource::CPU, cpu_seconds, cpu_seconds.saturating_add(1))?;
    rlimit::setrlimit(Resource::AS, memory_bytes, memory_bytes)
}
