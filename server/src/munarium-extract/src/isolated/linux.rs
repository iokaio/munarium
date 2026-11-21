// SPDX-License-Identifier: Apache-2.0
//! Linux limits for the extraction child: no core dump and `setrlimit`,
//! applied by the child to itself before it reads its input.
//!
//! Soft and hard limits are both set, and an unprivileged process cannot
//! raise a hard limit, so nothing the parser does afterwards can lift them.
//! The system calls go through safe wrappers (the `rlimit` crate and rustix,
//! pinned in this crate's `Cargo.toml` with their provenance), so this module
//! holds no `unsafe` code. The identity's `os=rlimit` describes the resource
//! limits; the dump refusal changes no indexed output.
//!
//! `RLIMIT_CORE` alone does not keep tenant content out of a core dump: when
//! the host's `core_pattern` pipes dumps to a handler (systemd-coredump,
//! apport, a container runtime's crash collector), the kernel ignores the
//! limit and streams the whole address space to that handler. A child that
//! hits its memory limit aborts, so this is the expected path for a hostile
//! document, not a rare one. Marking the process non-dumpable stops every
//! dump, piped or written, whatever `fs.suid_dumpable` says.

use rlimit::Resource;
use rustix::process::DumpableBehavior;

/// Not dumpable, no core file, CPU time with SIGXCPU at the limit and
/// SIGKILL one second later, and address space.
pub(super) fn apply(memory_bytes: u64, cpu_seconds: u64) -> std::io::Result<()> {
    rustix::process::set_dumpable_behavior(DumpableBehavior::NotDumpable)
        .map_err(std::io::Error::from)?;
    rlimit::setrlimit(Resource::CORE, 0, 0)?;
    rlimit::setrlimit(Resource::CPU, cpu_seconds, cpu_seconds.saturating_add(1))?;
    rlimit::setrlimit(Resource::AS, memory_bytes, memory_bytes)
}

#[cfg(test)]
mod tests {
    use super::*;

    const PROBE: &str = "MUNARIUM_TEST_LINUX_APPLY_PROBE";

    /// `apply` would bound this test process, so it runs in a fresh copy of
    /// the test binary, which reports what the kernel then holds.
    #[test]
    fn apply_makes_the_process_non_dumpable_and_sets_each_limit() {
        if std::env::var_os(PROBE).is_some() {
            apply(256 * 1024 * 1024, 7).expect("apply");
            println!(
                "probe dumpable={:?} core={:?} cpu={:?} as={:?}",
                rustix::process::dumpable_behavior().expect("dumpable"),
                rlimit::getrlimit(Resource::CORE).expect("core"),
                rlimit::getrlimit(Resource::CPU).expect("cpu"),
                rlimit::getrlimit(Resource::AS).expect("as"),
            );
            return;
        }
        let name = format!(
            "{}::apply_makes_the_process_non_dumpable_and_sets_each_limit",
            module_path!().split_once("::").expect("crate prefix").1
        );
        let out = std::process::Command::new(std::env::current_exe().expect("test binary"))
            .args(["--exact", &name, "--nocapture", "--test-threads=1"])
            .env(PROBE, "1")
            .output()
            .expect("run the probe");
        let stdout = String::from_utf8_lossy(&out.stdout);
        assert!(out.status.success(), "probe failed: {stdout}");
        assert!(
            stdout.contains(&format!(
                "probe dumpable=NotDumpable core=(0, 0) cpu=(7, 8) as=({0}, {0})",
                256 * 1024 * 1024
            )),
            "probe output: {stdout}"
        );
    }
}
