//! Canonical owners for optional device data. Untrusted storage keeps its stricter checks.
use anyhow::{ensure, Context, Result};
use std::{
    fs,
    path::{Component, Path, PathBuf},
    sync::Mutex,
};

fn reject_link(metadata: &fs::Metadata) -> Result<()> {
    ensure!(!metadata.file_type().is_symlink(), "owned_path_refused");
    #[cfg(windows)]
    {
        use std::os::windows::fs::MetadataExt;
        ensure!(
            metadata.file_attributes() & 0x400 == 0,
            "owned_path_refused"
        );
    }
    Ok(())
}

/// Strict validation remains useful after resolving the trusted owner's ancestors.
pub(crate) fn no_links(path: &Path) -> Result<()> {
    for ancestor in path.ancestors().filter(|path| !path.as_os_str().is_empty()) {
        match fs::symlink_metadata(ancestor) {
            Ok(metadata) => reject_link(&metadata)?,
            Err(error) if error.kind() == std::io::ErrorKind::NotFound => (),
            Err(error) => return Err(error.into()),
        }
    }
    Ok(())
}

pub(crate) struct PinnedRoot {
    requested: PathBuf,
    canonical: Mutex<Option<PathBuf>>,
}

impl PinnedRoot {
    pub(crate) fn new(requested: PathBuf) -> Self {
        let root = Self {
            requested,
            canonical: Mutex::new(None),
        };
        // Optional services remain constructible when their storage is unavailable.
        // Retry lazily, and never change an owner that has already been pinned.
        let _ = root.resolve("");
        root
    }

    pub(crate) fn resolve(&self, relative: &str) -> Result<PathBuf> {
        ensure!(
            self.requested.is_absolute()
                && Path::new(relative)
                    .components()
                    .all(|part| matches!(part, Component::Normal(_))),
            "owned_path_refused"
        );
        let mut pinned = self
            .canonical
            .lock()
            .map_err(|_| anyhow::anyhow!("owned_root_lock"))?;
        if pinned.is_none() {
            let metadata = fs::symlink_metadata(&self.requested)?;
            reject_link(&metadata)?;
            ensure!(metadata.is_dir(), "owned_path_refused");
            let canonical = self.requested.canonicalize()?;
            no_links(&canonical)?;
            *pinned = Some(canonical);
        }
        let root = pinned.as_ref().context("owned_root_unavailable")?;
        // The pinned physical owner and all managed descendants remain link-free.
        // Retargeting a linked ancestor of `requested` cannot redirect this path.
        let metadata = fs::symlink_metadata(root)?;
        reject_link(&metadata)?;
        ensure!(metadata.is_dir(), "owned_path_refused");
        let path = root.join(relative);
        no_links(&path)?;
        Ok(path)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn owner_pins_lazily_and_refuses_relative_escape() {
        let root = std::env::temp_dir().join(format!("nna-owned-root-{}", uuid::Uuid::new_v4()));
        let owner = PinnedRoot::new(root.clone());
        assert!(!root.exists());
        assert!(owner.resolve("file.bin").is_err());
        fs::create_dir(&root).unwrap();
        let expected = root.canonicalize().unwrap().join("file.bin");
        assert_eq!(owner.resolve("file.bin").unwrap(), expected);
        for relative in ["../outside", "child/../../outside", "/outside"] {
            assert!(owner.resolve(relative).is_err());
        }
        #[cfg(windows)]
        for relative in ["C:\\outside", "child\\..\\outside"] {
            assert!(owner.resolve(relative).is_err());
        }
        fs::remove_dir(root).unwrap();
    }

    #[cfg(windows)]
    #[test]
    fn pinned_owner_ignores_external_junction_retarget_and_rejects_owned_links() {
        use std::{os::windows::process::CommandExt, process::Command};

        fn junction(link: &Path, target: &Path) {
            let output = Command::new("cmd")
                .args(["/d", "/c", "mklink", "/J"])
                .arg(link)
                .arg(target)
                .creation_flags(0x08000000)
                .output()
                .unwrap();
            assert!(
                output.status.success(),
                "junction fixture failed: status={}, stdout={}, stderr={}",
                output.status,
                String::from_utf8_lossy(&output.stdout),
                String::from_utf8_lossy(&output.stderr)
            );
        }
        let root =
            std::env::temp_dir().join(format!("nna-owned-junction-{}", uuid::Uuid::new_v4()));
        fs::create_dir(&root).unwrap();
        let first = root.join("first");
        let second = root.join("second");
        fs::create_dir(&first).unwrap();
        fs::create_dir(&second).unwrap();
        fs::create_dir(first.join("profile")).unwrap();
        fs::create_dir(second.join("profile")).unwrap();
        let alias = root.join("alias");
        junction(&alias, &first);
        let owner = PinnedRoot::new(alias.join("profile"));
        let expected = first.canonicalize().unwrap().join("profile/file.bin");
        assert_eq!(owner.resolve("file.bin").unwrap(), expected);
        fs::remove_dir(&alias).unwrap();
        junction(&alias, &second);
        assert_eq!(owner.resolve("file.bin").unwrap(), expected);

        let child = first.join("profile").join("child");
        junction(&child, &second);
        assert!(owner.resolve("child/file.bin").is_err());
        fs::remove_dir(&child).unwrap();
        fs::remove_dir(first.join("profile")).unwrap();
        junction(&first.join("profile"), &second.join("profile"));
        assert!(owner.resolve("file.bin").is_err());
        let direct = PinnedRoot::new(first.join("profile"));
        assert!(direct.resolve("file.bin").is_err());

        // Every junction is removed explicitly; no recursive deletion follows it.
        fs::remove_dir(first.join("profile")).unwrap();
        fs::remove_dir(&alias).unwrap();
        fs::remove_dir(second.join("profile")).unwrap();
        fs::remove_dir(first).unwrap();
        fs::remove_dir(second).unwrap();
        fs::remove_dir(root).unwrap();
    }
}
