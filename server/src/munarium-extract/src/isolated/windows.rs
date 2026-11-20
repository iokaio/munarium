// SPDX-License-Identifier: Apache-2.0
//! Windows limits for the extraction child: a Job Object.
//!
//! The job carries a per-process committed-memory limit, a per-process
//! user-mode CPU time limit, an active-process limit of one (the child cannot
//! start another process) and kill-on-close, so the child dies with the job
//! handle even if the parent never reaches its own kill. The declarations
//! below are the documented `kernel32` ABI; `std` already links `kernel32`,
//! so no crate is added.

use super::IsolatedPdfLimits;
use std::ffi::c_void;
use std::os::windows::io::{AsRawHandle, FromRawHandle, OwnedHandle};
use std::process::Child;
use std::time::Duration;

const JOB_OBJECT_BASIC_ACCOUNTING_INFORMATION: i32 = 1;
const JOB_OBJECT_EXTENDED_LIMIT_INFORMATION: i32 = 9;

const JOB_OBJECT_LIMIT_PROCESS_TIME: u32 = 0x0000_0002;
const JOB_OBJECT_LIMIT_ACTIVE_PROCESS: u32 = 0x0000_0008;
const JOB_OBJECT_LIMIT_PROCESS_MEMORY: u32 = 0x0000_0100;
const JOB_OBJECT_LIMIT_DIE_ON_UNHANDLED_EXCEPTION: u32 = 0x0000_0400;
const JOB_OBJECT_LIMIT_KILL_ON_JOB_CLOSE: u32 = 0x0000_2000;

/// 100-nanosecond intervals per second, the unit of job time limits.
const TICKS_PER_SECOND: u64 = 10_000_000;

#[repr(C)]
#[derive(Default)]
struct BasicLimitInformation {
    per_process_user_time_limit: i64,
    per_job_user_time_limit: i64,
    limit_flags: u32,
    minimum_working_set_size: usize,
    maximum_working_set_size: usize,
    active_process_limit: u32,
    affinity: usize,
    priority_class: u32,
    scheduling_class: u32,
}

#[repr(C)]
#[derive(Default)]
struct IoCounters {
    read_operation_count: u64,
    write_operation_count: u64,
    other_operation_count: u64,
    read_transfer_count: u64,
    write_transfer_count: u64,
    other_transfer_count: u64,
}

#[repr(C)]
#[derive(Default)]
struct ExtendedLimitInformation {
    basic: BasicLimitInformation,
    io: IoCounters,
    process_memory_limit: usize,
    job_memory_limit: usize,
    peak_process_memory_used: usize,
    peak_job_memory_used: usize,
}

#[repr(C)]
#[derive(Default)]
struct BasicAccountingInformation {
    total_user_time: i64,
    total_kernel_time: i64,
    this_period_total_user_time: i64,
    this_period_total_kernel_time: i64,
    total_page_fault_count: u32,
    total_processes: u32,
    active_processes: u32,
    total_terminated_processes: u32,
}

#[link(name = "kernel32")]
extern "system" {
    fn CreateJobObjectW(attributes: *mut c_void, name: *const u16) -> *mut c_void;
    fn SetInformationJobObject(
        job: *mut c_void,
        class: i32,
        info: *const c_void,
        length: u32,
    ) -> i32;
    fn AssignProcessToJobObject(job: *mut c_void, process: *mut c_void) -> i32;
    fn QueryInformationJobObject(
        job: *mut c_void,
        class: i32,
        info: *mut c_void,
        length: u32,
        returned: *mut u32,
    ) -> i32;
}

/// What the job observed. Read after the child exits.
pub(super) struct Usage {
    pub cpu: Option<Duration>,
    pub cpu_user: Option<Duration>,
    pub peak_memory_bytes: Option<u64>,
}

/// A job holding one extraction child. Dropping it closes the handle, which
/// kills anything still in the job.
pub(super) struct Job {
    handle: OwnedHandle,
}

impl Job {
    pub(super) fn confine(child: &Child, limits: &IsolatedPdfLimits) -> std::io::Result<Self> {
        // SAFETY: both arguments may be null (default security, unnamed job).
        let raw = unsafe { CreateJobObjectW(std::ptr::null_mut(), std::ptr::null()) };
        if raw.is_null() {
            return Err(std::io::Error::last_os_error());
        }
        // SAFETY: `raw` is a valid job handle we own and close exactly once.
        let handle = unsafe { OwnedHandle::from_raw_handle(raw) };

        let mut info = ExtendedLimitInformation::default();
        info.basic.limit_flags = JOB_OBJECT_LIMIT_PROCESS_MEMORY
            | JOB_OBJECT_LIMIT_PROCESS_TIME
            | JOB_OBJECT_LIMIT_ACTIVE_PROCESS
            | JOB_OBJECT_LIMIT_DIE_ON_UNHANDLED_EXCEPTION
            | JOB_OBJECT_LIMIT_KILL_ON_JOB_CLOSE;
        info.basic.per_process_user_time_limit =
            i64::try_from(limits.cpu_seconds.saturating_mul(TICKS_PER_SECOND)).unwrap_or(i64::MAX);
        info.basic.active_process_limit = 1;
        info.process_memory_limit = usize::try_from(limits.memory_bytes).unwrap_or(usize::MAX);

        // SAFETY: `info` is a live, correctly laid out
        // JOBOBJECT_EXTENDED_LIMIT_INFORMATION and the length matches it.
        let ok = unsafe {
            SetInformationJobObject(
                handle.as_raw_handle(),
                JOB_OBJECT_EXTENDED_LIMIT_INFORMATION,
                (&info as *const ExtendedLimitInformation).cast(),
                std::mem::size_of::<ExtendedLimitInformation>() as u32,
            )
        };
        if ok == 0 {
            return Err(std::io::Error::last_os_error());
        }
        // SAFETY: both handles are valid for the duration of the call.
        let ok = unsafe { AssignProcessToJobObject(handle.as_raw_handle(), child.as_raw_handle()) };
        if ok == 0 {
            return Err(std::io::Error::last_os_error());
        }
        Ok(Self { handle })
    }

    pub(super) fn usage(&self) -> Usage {
        let mut accounting = BasicAccountingInformation::default();
        // SAFETY: the buffer is a live, correctly laid out
        // JOBOBJECT_BASIC_ACCOUNTING_INFORMATION and the length matches it.
        let accounted = unsafe {
            QueryInformationJobObject(
                self.handle.as_raw_handle(),
                JOB_OBJECT_BASIC_ACCOUNTING_INFORMATION,
                (&mut accounting as *mut BasicAccountingInformation).cast(),
                std::mem::size_of::<BasicAccountingInformation>() as u32,
                std::ptr::null_mut(),
            )
        } != 0;
        let mut extended = ExtendedLimitInformation::default();
        // SAFETY: as above, for JOBOBJECT_EXTENDED_LIMIT_INFORMATION.
        let limited = unsafe {
            QueryInformationJobObject(
                self.handle.as_raw_handle(),
                JOB_OBJECT_EXTENDED_LIMIT_INFORMATION,
                (&mut extended as *mut ExtendedLimitInformation).cast(),
                std::mem::size_of::<ExtendedLimitInformation>() as u32,
                std::ptr::null_mut(),
            )
        } != 0;
        let ticks =
            |t: i64| Duration::from_nanos(u64::try_from(t).unwrap_or(0).saturating_mul(100));
        Usage {
            cpu: accounted
                .then(|| ticks(accounting.total_user_time) + ticks(accounting.total_kernel_time)),
            cpu_user: accounted.then(|| ticks(accounting.total_user_time)),
            peak_memory_bytes: limited.then_some(extended.peak_process_memory_used as u64),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn structures_match_the_documented_x64_sizes() {
        // JOBOBJECT_BASIC_LIMIT_INFORMATION is 64 bytes and the extended form
        // 144 on x64; a wrong layout would set the wrong limits silently.
        if cfg!(target_pointer_width = "64") {
            assert_eq!(std::mem::size_of::<BasicLimitInformation>(), 64);
            assert_eq!(std::mem::size_of::<ExtendedLimitInformation>(), 144);
            assert_eq!(std::mem::size_of::<BasicAccountingInformation>(), 48);
        }
    }
}
