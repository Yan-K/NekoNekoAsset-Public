//! Portable packages keep their device profile beside the executable.
use anyhow::{Context, Result};
use serde::{Deserialize, Serialize};
use sha2::{Digest, Sha256};
use std::path::{Path, PathBuf};
use tauri::utils::config::AppDirectoriesOverride;

const PORTABLE_MARKER: &str = "portable.flag";
const RESTART_PARENT: &str = "NNA_RESTART_PARENT";
const DEFAULT_LIBRARY_FOLDER: &str = "NekoNekoDatabase";

fn executable_library_path(executable: &Path) -> Result<PathBuf> {
    Ok(executable
        .parent()
        .context("Executable location unavailable")?
        .join(DEFAULT_LIBRARY_FOLDER))
}

/// First setup suggests a library beside the actual executable for both
/// installed and portable copies. This neither creates nor selects a library.
pub(crate) fn default_library_path() -> Result<PathBuf> {
    let executable =
        tauri::utils::platform::current_exe().context("Executable location unavailable")?;
    executable_library_path(&executable)
}

fn library_subfolder(parent: &Path) -> PathBuf {
    let named = parent.file_name().and_then(|name| name.to_str());
    if named.is_some_and(|name| name.eq_ignore_ascii_case(DEFAULT_LIBRARY_FOLDER)) {
        parent.to_path_buf()
    } else {
        parent.join(DEFAULT_LIBRARY_FOLDER)
    }
}

/// A new-library choice is a parent folder, unless it already names the
/// database folder or contains an existing current library.
pub(crate) fn new_library_path(parent: &Path) -> PathBuf {
    if parent
        .join(".nna")
        .join("data")
        .join("catalogue.sqlite3")
        .is_file()
    {
        parent.to_path_buf()
    } else {
        library_subfolder(parent)
    }
}

/// Tauri starts the replacement before exiting. Keep its startup behind the
/// exact old process, whose exit releases the library and profile locks.
pub(crate) fn prepare_restart_handoff() -> Result<()> {
    #[cfg(windows)]
    std::env::set_var(RESTART_PARENT, restart_handoff::marker()?);
    Ok(())
}

fn wait_for_restart_parent() -> Result<()> {
    let Some(marker) = std::env::var_os(RESTART_PARENT) else {
        return Ok(());
    };
    std::env::remove_var(RESTART_PARENT);
    #[cfg(windows)]
    restart_handoff::wait(
        marker
            .to_str()
            .ok_or_else(|| anyhow::anyhow!("library_restart_failed"))?,
    )?;
    #[cfg(not(windows))]
    let _ = marker;
    Ok(())
}

#[cfg(windows)]
mod restart_handoff {
    use super::*;
    use std::ffi::c_void;

    #[repr(C)]
    #[derive(Default)]
    struct FileTime {
        low: u32,
        high: u32,
    }

    #[link(name = "kernel32")]
    #[allow(non_snake_case)]
    extern "system" {
        fn GetCurrentProcess() -> *mut c_void;
        fn OpenProcess(access: u32, inherit: i32, pid: u32) -> *mut c_void;
        fn GetProcessTimes(
            process: *mut c_void,
            created: *mut FileTime,
            exited: *mut FileTime,
            kernel: *mut FileTime,
            user: *mut FileTime,
        ) -> i32;
        fn WaitForSingleObject(handle: *mut c_void, milliseconds: u32) -> u32;
        fn GetLastError() -> u32;
        fn CloseHandle(handle: *mut c_void) -> i32;
    }

    struct Process(*mut c_void);
    impl Drop for Process {
        fn drop(&mut self) {
            unsafe {
                CloseHandle(self.0);
            }
        }
    }

    fn creation_time(process: *mut c_void) -> Result<u64> {
        let (mut created, mut exited, mut kernel, mut user) = (
            FileTime::default(),
            FileTime::default(),
            FileTime::default(),
            FileTime::default(),
        );
        anyhow::ensure!(
            unsafe { GetProcessTimes(process, &mut created, &mut exited, &mut kernel, &mut user) }
                != 0,
            "library_restart_failed"
        );
        Ok((u64::from(created.high) << 32) | u64::from(created.low))
    }

    pub(super) fn marker() -> Result<String> {
        Ok(format!(
            "{}:{}",
            std::process::id(),
            creation_time(unsafe { GetCurrentProcess() })?
        ))
    }

    pub(super) fn wait(marker: &str) -> Result<()> {
        let (pid, created) = marker
            .split_once(':')
            .ok_or_else(|| anyhow::anyhow!("library_restart_failed"))?;
        let pid: u32 = pid
            .parse()
            .map_err(|_| anyhow::anyhow!("library_restart_failed"))?;
        let created: u64 = created
            .parse()
            .map_err(|_| anyhow::anyhow!("library_restart_failed"))?;
        anyhow::ensure!(
            pid != 0 && pid != std::process::id() && created != 0,
            "library_restart_failed"
        );
        // SYNCHRONIZE | PROCESS_QUERY_LIMITED_INFORMATION; the handle pins this
        // process identity even if its PID is reused after it exits.
        let handle = unsafe { OpenProcess(0x0010_0000 | 0x1000, 0, pid) };
        if handle.is_null() {
            // ERROR_INVALID_PARAMETER: this process has already exited.
            anyhow::ensure!(unsafe { GetLastError() } == 87, "library_restart_failed");
            return Ok(());
        }
        let process = Process(handle);
        if creation_time(process.0)? != created {
            return Ok(());
        }
        // Bound an invalid handoff, without releasing or stealing any lock.
        anyhow::ensure!(
            unsafe { WaitForSingleObject(process.0, 30_000) } == 0,
            "library_restart_failed"
        );
        Ok(())
    }
}

/// A device remembers which library to open; credentials stay in this profile.
#[derive(Clone, Serialize, Deserialize)]
pub(crate) struct LibrarySelection {
    pub path: PathBuf,
    pub locale: String,
}

pub(crate) fn selected_library(device_dir: &Path) -> Result<Option<LibrarySelection>> {
    let path = device_dir.join("library.json");
    if !path.exists() {
        return Ok(None);
    }
    let selection: LibrarySelection = serde_json::from_slice(&std::fs::read(path)?)
        .map_err(|_| anyhow::anyhow!("library_preferences_invalid"))?;
    anyhow::ensure!(selection.path.is_absolute(), "library_preferences_invalid");
    Ok(Some(selection))
}

pub(crate) fn select_library(device_dir: &Path, selection: &LibrarySelection) -> Result<()> {
    std::fs::create_dir_all(device_dir)?;
    let temporary = device_dir.join("library.json.pending");
    let destination = device_dir.join("library.json");
    {
        use std::io::Write;
        let mut file = std::fs::File::create(&temporary)?;
        file.write_all(&serde_json::to_vec(selection)?)?;
        file.sync_all()?;
    }
    crate::ai_provider::replace_file(&temporary, &destination)
        .map_err(|_| anyhow::anyhow!("library_preferences_write_failed"))?;
    Ok(())
}

fn portable_data_dir(executable: &Path) -> Option<PathBuf> {
    let folder = executable.parent()?;
    folder
        .join(PORTABLE_MARKER)
        .is_file()
        .then(|| folder.join("data"))
}

fn portable_identifier(identifier: &str, data_dir: &Path) -> String {
    let path = data_dir.to_string_lossy().replace('\\', "/");
    #[cfg(windows)]
    let path = path.to_lowercase();
    let digest = Sha256::digest(path.trim_end_matches('/').as_bytes());
    format!("{identifier}.portable.{}", hex::encode(digest))
}

pub fn configure(context: &mut tauri::Context<tauri::Wry>) -> Result<()> {
    wait_for_restart_parent()?;
    #[cfg(debug_assertions)]
    if let Some(device_dir) = std::env::var_os("NNA_DATA_DIR") {
        let config = context.config_mut();
        config.identifier = portable_identifier(&config.identifier, Path::new(&device_dir));
        config.app.app_directories_override =
            Some(AppDirectoriesOverride::Root(PathBuf::from(device_dir)));
        return Ok(());
    }
    let executable =
        tauri::utils::platform::current_exe().context("Executable location unavailable")?;
    if let Some(data_dir) = portable_data_dir(&executable) {
        let config = context.config_mut();
        // Resolve before windows and plugins start, so WebView storage and the
        // single-instance guard follow the same profile as the catalogue.
        config.identifier = portable_identifier(&config.identifier, &data_dir);
        config.app.app_directories_override = Some(AppDirectoriesOverride::Root(data_dir));
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn initial_library_default_uses_the_executable_parent_for_both_distributions() {
        let authored_root = PathBuf::from(env!("CARGO_MANIFEST_DIR"))
            .join("target")
            .join(format!("authored-library-default-{}", uuid::Uuid::new_v4()));
        for folder in ["Installed App", "Portable App"] {
            let directory = authored_root.join(folder);
            let executable = directory.join("NekoNekoAsset.exe");
            assert_eq!(
                executable_library_path(&executable).unwrap(),
                directory.join("NekoNekoDatabase")
            );
        }
        assert!(
            !authored_root.exists(),
            "default lookup created a directory"
        );
    }

    #[test]
    fn initial_library_default_resolves_the_running_executable() {
        let executable = tauri::utils::platform::current_exe().unwrap();
        assert_eq!(
            default_library_path().unwrap(),
            executable.parent().unwrap().join("NekoNekoDatabase")
        );
    }

    #[cfg(windows)]
    #[test]
    fn new_library_subfolder_is_added_once_for_drive_and_unc_parents() {
        for (parent, expected) in [
            ("F:\\Assets", "F:\\Assets\\NekoNekoDatabase"),
            ("F:\\Assets\\", "F:\\Assets\\NekoNekoDatabase"),
            ("F:\\", "F:\\NekoNekoDatabase"),
            ("F:/Assets/", "F:/Assets/NekoNekoDatabase"),
            (
                "F:\\Assets\\NekoNekoDatabase",
                "F:\\Assets\\NekoNekoDatabase",
            ),
            (
                "F:\\Assets\\nekonekodatabase\\",
                "F:\\Assets\\nekonekodatabase\\",
            ),
            (
                "\\\\authored-server\\share\\Assets\\",
                "\\\\authored-server\\share\\Assets\\NekoNekoDatabase",
            ),
            (
                "\\\\authored-server\\share\\NekoNekoDatabase\\",
                "\\\\authored-server\\share\\NekoNekoDatabase\\",
            ),
        ] {
            let result = library_subfolder(Path::new(parent));
            assert_eq!(result, Path::new(expected));
            assert_eq!(library_subfolder(&result), result);
        }
    }

    #[test]
    fn new_library_choice_retains_an_existing_catalogue_at_its_exact_root() {
        let root = PathBuf::from(env!("CARGO_MANIFEST_DIR"))
            .join("target")
            .join(format!(
                "authored-existing-library-{}",
                uuid::Uuid::new_v4()
            ));
        let data = root.join(".nna").join("data");
        std::fs::create_dir_all(&data).unwrap();
        assert_eq!(new_library_path(&root), root.join("NekoNekoDatabase"));
        let marker = data.join("catalogue.sqlite3");
        std::fs::write(&marker, b"authored library marker").unwrap();
        assert_eq!(new_library_path(&root), root);
        assert_eq!(new_library_path(&root).as_os_str(), root.as_os_str());
        assert_eq!(std::fs::read(&marker).unwrap(), b"authored library marker");
        std::fs::remove_file(marker).unwrap();
        std::fs::remove_dir(data).unwrap();
        std::fs::remove_dir(root.join(".nna")).unwrap();
        std::fs::remove_dir(root).unwrap();
    }

    #[cfg(windows)]
    #[test]
    fn restart_handoff_child() {
        use std::io::{Read, Write};
        let Some(mode) = std::env::var_os("NNA_PROFILE_HANDOFF_TEST") else {
            return;
        };
        let root = PathBuf::from(std::env::var_os("NNA_PROFILE_HANDOFF_ROOT").unwrap());
        if mode == "owner" {
            let _lock = crate::library_lock::LibraryLock::acquire(&root).unwrap();
            println!("HANDOFF_OWNER:{}", restart_handoff::marker().unwrap());
            std::io::stdout().flush().unwrap();
            std::io::stdin().read_exact(&mut [0]).unwrap();
        } else {
            println!("HANDOFF_WAITING");
            std::io::stdout().flush().unwrap();
            wait_for_restart_parent().unwrap();
            assert!(std::env::var_os(RESTART_PARENT).is_none());
            assert_eq!(
                std::env::var("NNA_DATA_DIR").unwrap(),
                "authored-device-profile"
            );
            let _lock = crate::library_lock::LibraryLock::acquire(&root).unwrap();
        }
    }

    #[cfg(windows)]
    #[test]
    fn restart_handoff_waits_for_exact_parent_and_preserves_device_environment() {
        use std::{
            io::{BufRead, BufReader},
            os::windows::process::CommandExt,
            process::{Command, Stdio},
        };
        let root = PathBuf::from(env!("CARGO_MANIFEST_DIR"))
            .join("target")
            .join(format!("authored-restart-{}", uuid::Uuid::new_v4()));
        std::fs::create_dir_all(&root).unwrap();
        let spawn = |mode: &str, marker: Option<&str>| {
            let mut command = Command::new(std::env::current_exe().unwrap());
            command
                .args([
                    "--exact",
                    "profile::tests::restart_handoff_child",
                    "--nocapture",
                    "--test-threads=1",
                ])
                .env("NNA_PROFILE_HANDOFF_TEST", mode)
                .env("NNA_PROFILE_HANDOFF_ROOT", &root)
                .env("NNA_DATA_DIR", "authored-device-profile")
                .env_remove(RESTART_PARENT)
                .stdin(Stdio::piped())
                .stdout(Stdio::piped())
                .stderr(Stdio::piped())
                .creation_flags(0x08000000);
            if let Some(marker) = marker {
                command.env(RESTART_PARENT, marker);
            }
            command.spawn().unwrap()
        };
        let read_until = |reader: &mut BufReader<std::process::ChildStdout>, prefix: &str| {
            let mut line = String::new();
            loop {
                assert!(
                    reader.read_line(&mut line).unwrap() != 0,
                    "authored child ended before {prefix}"
                );
                if let Some(offset) = line.find(prefix) {
                    break line[offset + prefix.len()..].trim().to_owned();
                }
                line.clear();
            }
        };
        let mut owner = spawn("owner", None);
        let marker = read_until(
            &mut BufReader::new(owner.stdout.take().unwrap()),
            "HANDOFF_OWNER:",
        );
        let (pid, created) = marker.split_once(':').unwrap();
        // A live unrelated process with a reused PID must never hold startup.
        restart_handoff::wait(&format!("{pid}:{}", created.parse::<u64>().unwrap() + 1)).unwrap();
        assert!(restart_handoff::wait("invalid").is_err());
        assert!(restart_handoff::wait(&restart_handoff::marker().unwrap()).is_err());
        let mut replacement = spawn("replacement", Some(&marker));
        let mut replacement_output = BufReader::new(replacement.stdout.take().unwrap());
        read_until(&mut replacement_output, "HANDOFF_WAITING");
        std::thread::sleep(std::time::Duration::from_millis(50));
        assert!(
            replacement.try_wait().unwrap().is_none(),
            "replacement passed its still-live parent's gate"
        );
        owner.kill().unwrap();
        owner.wait().unwrap();
        assert!(replacement.wait_with_output().unwrap().status.success());
        std::fs::remove_file(root.join(".nna-library.lock")).unwrap();
        std::fs::remove_dir(root).unwrap();
    }

    #[test]
    fn marker_selects_executable_folder_without_reading_or_copying_local_profile() {
        let folder = std::env::temp_dir().join(format!("nna-profile-{}", uuid::Uuid::new_v4()));
        std::fs::create_dir_all(&folder).unwrap();
        let executable = folder.join("NekoNekoAsset.exe");
        assert_eq!(portable_data_dir(&executable), None);
        std::fs::write(folder.join(PORTABLE_MARKER), "").unwrap();
        assert_eq!(portable_data_dir(&executable), Some(folder.join("data")));
        assert!(!folder.join("data").exists());
        std::fs::remove_dir_all(&folder).unwrap();
    }

    #[test]
    fn portable_copies_have_independent_instance_ids() {
        let base = "app.nekonekoasset.desktop";
        let first = portable_identifier(base, Path::new("F:/Portable/One/data"));
        assert_ne!(first, base);
        assert_ne!(
            first,
            portable_identifier(base, Path::new("F:/Portable/Two/data"))
        );
        #[cfg(windows)]
        assert_eq!(
            first,
            portable_identifier(base, Path::new("f:\\portable\\one\\data\\"))
        );
    }

    #[test]
    fn library_selection_replaces_only_the_device_locator() {
        let directory = std::env::temp_dir().join(format!("nna-locator-{}", uuid::Uuid::new_v4()));
        std::fs::create_dir_all(&directory).unwrap();
        std::fs::write(directory.join("session-sentinel"), b"device-owned").unwrap();
        assert!(selected_library(&directory).unwrap().is_none());
        for (name, locale) in [("first", "en"), ("second", "zh-Hant")] {
            let path = directory.join(name);
            select_library(
                &directory,
                &LibrarySelection {
                    path: path.clone(),
                    locale: locale.into(),
                },
            )
            .unwrap();
            let selected = selected_library(&directory).unwrap().unwrap();
            assert_eq!(selected.path, path);
            assert_eq!(selected.locale, locale);
        }
        assert_eq!(
            std::fs::read(directory.join("session-sentinel")).unwrap(),
            b"device-owned"
        );
        std::fs::write(
            directory.join("library.json"),
            br#"{"path":"relative","locale":"en"}"#,
        )
        .unwrap();
        assert!(selected_library(&directory).is_err());
        std::fs::remove_dir_all(&directory).unwrap();
    }
}
