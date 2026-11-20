// SPDX-License-Identifier: Apache-2.0
//! Linux limits for the extraction child: `setrlimit`, applied by the child
//! to itself before it reads its input.
//!
//! Soft and hard limits are both set, and an unprivileged process cannot
//! raise a hard limit, so nothing the parser does afterwards can lift them.
//! The declaration below is the libc ABI on 64-bit Linux (glibc and musl,
//! where `rlim_t` is 64 bits); `std` already links libc, so no crate is added.

const RLIMIT_CPU: i32 = 0;
const RLIMIT_CORE: i32 = 4;
const RLIMIT_AS: i32 = 9;

#[repr(C)]
struct Rlimit {
    current: u64,
    maximum: u64,
}

extern "C" {
    fn setrlimit(resource: i32, limit: *const Rlimit) -> i32;
}

fn set(resource: i32, current: u64, maximum: u64) -> std::io::Result<()> {
    let limit = Rlimit { current, maximum };
    // SAFETY: `limit` is a live, correctly laid out `struct rlimit`.
    if unsafe { setrlimit(resource, &limit) } != 0 {
        return Err(std::io::Error::last_os_error());
    }
    Ok(())
}

/// No core dumps (a dump would write tenant content to disk), CPU time with
/// SIGXCPU at the limit and SIGKILL one second later, and address space.
pub(super) fn apply(memory_bytes: u64, cpu_seconds: u64) -> std::io::Result<()> {
    set(RLIMIT_CORE, 0, 0)?;
    set(RLIMIT_CPU, cpu_seconds, cpu_seconds.saturating_add(1))?;
    set(RLIMIT_AS, memory_bytes, memory_bytes)
}
