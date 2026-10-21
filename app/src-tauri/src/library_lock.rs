//! One live App owns a shared library. The OS lock, not file age, is authoritative.

use anyhow::{Context, Result};
use std::{fs::File, fs::OpenOptions, path::Path};

const LOCK_FILE: &str = ".nna-library.lock";

pub(crate) struct LibraryLock {
    file: File,
}

impl LibraryLock {
    /// Acquire before opening the catalogue or changing any library content.
    /// The selected root must already exist; no fallback root is created.
    pub(crate) fn acquire(root: &Path) -> Result<Self> {
        Self::acquire_with_create(root, true)
    }

    /// A setup probe must not materialize even a lock file in its candidate.
    pub(crate) fn acquire_existing(root: &Path) -> Result<Self> {
        Self::acquire_with_create(root, false)
    }

    fn acquire_with_create(root: &Path, create: bool) -> Result<Self> {
        if !root.is_dir() {
            anyhow::bail!("library_lock_unavailable");
        }
        let mut options = OpenOptions::new();
        options
            .read(true)
            .write(true)
            .create(create)
            .truncate(false);
        #[cfg(windows)]
        {
            use std::os::windows::fs::OpenOptionsExt;
            // Permit other Apps to attempt the byte-range lock, but prevent
            // deleting/replacing its file while this handle is alive.
            const FILE_SHARE_READ: u32 = 1;
            const FILE_SHARE_WRITE: u32 = 2;
            options.share_mode(FILE_SHARE_READ | FILE_SHARE_WRITE);
        }
        let file = options
            .open(root.join(LOCK_FILE))
            .context("library_lock_unavailable")?;
        fs2::FileExt::try_lock_exclusive(&file).map_err(|error| {
            if error.raw_os_error() == fs2::lock_contended_error().raw_os_error() {
                anyhow::anyhow!("library_in_use")
            } else {
                anyhow::Error::new(error).context("library_lock_unavailable")
            }
        })?;
        let guard = Self { file };
        guard.check_available()?;
        Ok(guard)
    }

    /// A surfaced network/handle error requires stopping library work. This
    /// never drops the handle, steals a stale lock, or retries acquisition.
    pub(crate) fn check_available(&self) -> Result<()> {
        self.file.sync_all().context("library_lock_unavailable")
    }
}

impl Drop for LibraryLock {
    fn drop(&mut self) {
        let _ = fs2::FileExt::unlock(&self.file);
        // Closing the handle also releases the lock after a crash/failed unlock.
        // Leave the stable lock file; deleting it would race another opener.
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::{
        io::{BufRead, BufReader, Read, Write},
        path::PathBuf,
        process::{Child, Command, Stdio},
        thread,
        time::{Duration, Instant},
    };

    struct TestRoot(PathBuf);
    impl TestRoot {
        fn new() -> Self {
            let root = PathBuf::from(env!("CARGO_MANIFEST_DIR"))
                .join("target")
                .join(format!("library-lock-test-{}", uuid::Uuid::new_v4()));
            std::fs::create_dir(&root).unwrap();
            Self(root)
        }
    }
    impl Drop for TestRoot {
        fn drop(&mut self) {
            let _ = std::fs::remove_file(self.0.join(LOCK_FILE));
            let _ = std::fs::remove_dir(&self.0);
        }
    }

    fn child(root: &Path, mode: &str) -> Child {
        let mut command = Command::new(std::env::current_exe().unwrap());
        command
            .args([
                "--exact",
                "library_lock::tests::lock_child_process",
                "--nocapture",
                "--test-threads=1",
            ])
            .env("NNA_LOCK_TEST_ROOT", root)
            .env("NNA_LOCK_TEST_MODE", mode)
            .stdin(Stdio::piped())
            .stdout(Stdio::piped())
            .stderr(Stdio::piped());
        #[cfg(windows)]
        {
            use std::os::windows::process::CommandExt;
            command.creation_flags(0x08000000); // CREATE_NO_WINDOW
        }
        command.spawn().unwrap()
    }

    #[test]
    fn lock_child_process() {
        let Some(root) = std::env::var_os("NNA_LOCK_TEST_ROOT") else {
            return;
        };
        let root = PathBuf::from(root);
        if std::env::var("NNA_LOCK_TEST_MODE").unwrap() == "reject" {
            assert_eq!(
                LibraryLock::acquire(&root).err().unwrap().to_string(),
                "library_in_use"
            );
            return;
        }
        let _guard = LibraryLock::acquire(&root).unwrap();
        println!("LIBRARY_LOCK_READY");
        std::io::stdout().flush().unwrap();
        let mut exit = [0];
        std::io::stdin().read_exact(&mut exit).unwrap();
    }

    #[test]
    fn second_process_is_rejected_until_owner_closes() {
        let root = TestRoot::new();
        let guard = LibraryLock::acquire(&root.0).unwrap();
        let denied = child(&root.0.join("."), "reject")
            .wait_with_output()
            .unwrap();
        assert!(denied.status.success());
        guard.check_available().unwrap();
        drop(guard);
        LibraryLock::acquire(&root.0).unwrap();
    }

    #[test]
    fn process_crash_releases_lock_without_stale_file_cleanup() {
        let root = TestRoot::new();
        let mut owner = child(&root.0, "hold");
        let mut output = BufReader::new(owner.stdout.take().unwrap());
        let mut line = String::new();
        loop {
            assert!(output.read_line(&mut line).unwrap() != 0);
            if line.contains("LIBRARY_LOCK_READY") {
                break;
            }
            line.clear();
        }
        assert_eq!(
            LibraryLock::acquire(&root.0).err().unwrap().to_string(),
            "library_in_use"
        );
        owner.kill().unwrap();
        owner.wait().unwrap();
        assert!(root.0.join(LOCK_FILE).is_file());
        let deadline = Instant::now() + Duration::from_secs(2);
        loop {
            match LibraryLock::acquire(&root.0) {
                Ok(_) => break,
                Err(error) if error.to_string() == "library_in_use" => {
                    assert!(Instant::now() < deadline);
                    thread::sleep(Duration::from_millis(10));
                }
                Err(error) => panic!("{error}"),
            }
        }
    }

    #[cfg(windows)]
    #[test]
    fn active_lock_file_cannot_be_replaced() {
        let root = TestRoot::new();
        let guard = LibraryLock::acquire(&root.0).unwrap();
        assert!(std::fs::remove_file(root.0.join(LOCK_FILE)).is_err());
        drop(guard);
        assert!(root.0.join(LOCK_FILE).is_file());
        LibraryLock::acquire(&root.0).unwrap();
    }
}
