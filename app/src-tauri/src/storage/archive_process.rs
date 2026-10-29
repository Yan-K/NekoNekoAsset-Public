//! Tie 7-Zip's process lifetime to the App without changing its argument/pipe handling.
use anyhow::{ensure, Result};
use std::{ffi::c_void, os::windows::io::AsRawHandle, process::Child};

type Handle = *mut c_void;

#[repr(C)]
#[derive(Default)]
struct BasicLimits {
    process_time: i64,
    job_time: i64,
    flags: u32,
    minimum_working_set: usize,
    maximum_working_set: usize,
    active_process_limit: u32,
    affinity: usize,
    priority: u32,
    scheduling: u32,
}

#[repr(C)]
#[derive(Default)]
struct ExtendedLimits {
    basic: BasicLimits,
    io_counters: [u64; 6],
    process_memory_limit: usize,
    job_memory_limit: usize,
    peak_process_memory: usize,
    peak_job_memory: usize,
}

#[repr(C)]
#[derive(Default)]
struct ThreadEntry {
    size: u32,
    usage: u32,
    id: u32,
    process_id: u32,
    base_priority: i32,
    delta_priority: i32,
    flags: u32,
}

#[link(name = "kernel32")]
extern "system" {
    fn CreateJobObjectW(attributes: *const c_void, name: *const u16) -> Handle;
    fn SetInformationJobObject(job: Handle, class: i32, info: *const c_void, size: u32) -> i32;
    fn AssignProcessToJobObject(job: Handle, process: Handle) -> i32;
    fn CloseHandle(handle: Handle) -> i32;
    fn CreateToolhelp32Snapshot(flags: u32, process: u32) -> Handle;
    fn Thread32First(snapshot: Handle, entry: *mut ThreadEntry) -> i32;
    fn Thread32Next(snapshot: Handle, entry: *mut ThreadEntry) -> i32;
    fn OpenThread(access: u32, inherit: i32, id: u32) -> Handle;
    fn ResumeThread(thread: Handle) -> u32;
}

struct OwnedHandle(Handle);
impl Drop for OwnedHandle {
    fn drop(&mut self) {
        unsafe { CloseHandle(self.0) };
    }
}

pub(super) struct ChildJob {
    handle: OwnedHandle,
}
impl ChildJob {
    pub fn new() -> Result<Self> {
        let job = unsafe { CreateJobObjectW(std::ptr::null(), std::ptr::null()) };
        ensure!(!job.is_null(), "archive_process_io");
        let job = OwnedHandle(job);
        let mut limits = ExtendedLimits::default();
        limits.basic.flags = 0x2000; // JOB_OBJECT_LIMIT_KILL_ON_JOB_CLOSE
        ensure!(
            unsafe {
                SetInformationJobObject(
                    job.0,
                    9,
                    &limits as *const _ as *const c_void,
                    std::mem::size_of::<ExtendedLimits>() as u32,
                )
            } != 0,
            "archive_process_io"
        );
        Ok(Self { handle: job })
    }

    /// The caller spawned this child with CREATE_SUSPENDED and terminates it
    /// on any error. It cannot execute before assignment to the App-owned job.
    pub fn attach_and_resume(&self, child: &Child) -> Result<()> {
        ensure!(
            unsafe { AssignProcessToJobObject(self.handle.0, child.as_raw_handle()) } != 0,
            "archive_process_io"
        );
        let snapshot = unsafe { CreateToolhelp32Snapshot(0x4, 0) }; // TH32CS_SNAPTHREAD
        ensure!(snapshot != -1isize as Handle, "archive_process_io");
        let snapshot = OwnedHandle(snapshot);
        let mut entry = ThreadEntry {
            size: std::mem::size_of::<ThreadEntry>() as u32,
            ..Default::default()
        };
        let mut present = unsafe { Thread32First(snapshot.0, &mut entry) } != 0;
        let mut primary = None;
        while present {
            if entry.process_id == child.id() {
                // A newly created suspended process has one primary thread.
                // Refuse ambiguity rather than resuming an arbitrary injected thread.
                ensure!(primary.is_none(), "archive_process_io");
                primary = Some(entry.id);
            }
            present = unsafe { Thread32Next(snapshot.0, &mut entry) } != 0;
        }
        let primary = primary.ok_or_else(|| anyhow::anyhow!("archive_process_io"))?;
        let thread = unsafe { OpenThread(0x2, 0, primary) }; // THREAD_SUSPEND_RESUME
        ensure!(!thread.is_null(), "archive_process_io");
        let thread = OwnedHandle(thread);
        ensure!(
            unsafe { ResumeThread(thread.0) } != u32::MAX,
            "archive_process_io"
        );
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::{
        os::windows::process::CommandExt,
        process::{Command, Stdio},
    };

    #[test]
    fn closing_the_job_terminates_its_owned_child() {
        if std::env::var_os("NNA_ARCHIVE_JOB_FIXTURE").as_deref()
            == Some(std::ffi::OsStr::new("wait"))
        {
            std::fs::write(std::env::var_os("NNA_ARCHIVE_JOB_READY").unwrap(), b"ready").unwrap();
            std::thread::sleep(std::time::Duration::from_secs(60));
            return;
        }
        let ready =
            std::env::temp_dir().join(format!("nna-archive-child-ready-{}", uuid::Uuid::new_v4()));
        let job = ChildJob::new().unwrap();
        let mut child = Command::new(std::env::current_exe().unwrap())
            .args([
                "--exact",
                "storage::archive::process::tests::closing_the_job_terminates_its_owned_child",
            ])
            .env("NNA_ARCHIVE_JOB_FIXTURE", "wait")
            .env("NNA_ARCHIVE_JOB_READY", &ready)
            .stdin(Stdio::null())
            .stdout(Stdio::null())
            .stderr(Stdio::null())
            .creation_flags(0x08000004)
            .spawn()
            .unwrap();
        if let Err(error) = job.attach_and_resume(&child) {
            let _ = child.kill();
            let _ = child.wait();
            panic!("{error}");
        }
        let ready_deadline = std::time::Instant::now() + std::time::Duration::from_secs(5);
        while !ready.exists() {
            assert!(
                child.try_wait().unwrap().is_none(),
                "owned child exited before reaching its wait"
            );
            if std::time::Instant::now() >= ready_deadline {
                let _ = child.kill();
                let _ = child.wait();
                panic!("owned child did not reach its wait");
            }
            std::thread::sleep(std::time::Duration::from_millis(10));
        }
        assert!(child.try_wait().unwrap().is_none());
        drop(job);
        let deadline = std::time::Instant::now() + std::time::Duration::from_secs(2);
        loop {
            if child.try_wait().unwrap().is_some() {
                break;
            }
            if std::time::Instant::now() >= deadline {
                let _ = child.kill();
                let _ = child.wait();
                panic!("owned child survived job closure");
            }
            std::thread::sleep(std::time::Duration::from_millis(10));
        }
        std::fs::remove_file(ready).unwrap();
    }
}
