use crate::{Error, Result};
use std::path::{Path, PathBuf};

pub(crate) struct RuntimeDirectory {
    path: Option<PathBuf>,
}

impl RuntimeDirectory {
    pub(crate) fn create(root: Option<&str>) -> Result<Self> {
        let root = root.map(PathBuf::from).unwrap_or_else(std::env::temp_dir);
        if !root.is_absolute() {
            return Err(Error::Launch("runtime_dir must be absolute".into()));
        }
        std::fs::create_dir_all(&root)?;
        let root = std::fs::canonicalize(root)?;
        for _ in 0..100 {
            let path = root.join(format!("eoka-{:016x}", fastrand::u64(..)));
            #[cfg(unix)]
            let created = {
                use std::os::unix::fs::DirBuilderExt;
                std::fs::DirBuilder::new().mode(0o700).create(&path)
            };
            #[cfg(not(unix))]
            let created = std::fs::create_dir(&path);
            match created {
                Ok(()) => return Ok(Self { path: Some(path) }),
                Err(error) if error.kind() == std::io::ErrorKind::AlreadyExists => continue,
                Err(error) => return Err(error.into()),
            }
        }
        Err(Error::Launch(
            "Cannot allocate a unique browser runtime directory".into(),
        ))
    }

    pub(crate) fn path(&self) -> &Path {
        self.path.as_deref().expect("runtime ownership is active")
    }
    pub(crate) fn cleanup(&mut self) -> std::io::Result<()> {
        if let Some(path) = &self.path {
            remove_dir_if_exists(path).map_err(|error| {
                std::io::Error::new(
                    error.kind(),
                    format!(
                        "Failed to remove owned browser runtime {}: {}",
                        path.display(),
                        error
                    ),
                )
            })?;
            self.path = None;
        }
        Ok(())
    }
}

impl Drop for RuntimeDirectory {
    fn drop(&mut self) {
        if let Err(error) = self.cleanup() {
            tracing::warn!("Failed to remove owned browser runtime: {}", error);
        }
    }
}

fn remove_dir_if_exists(path: &Path) -> std::io::Result<()> {
    for attempt in 0..10 {
        match std::fs::remove_dir_all(path) {
            Ok(()) => return Ok(()),
            Err(error) if error.kind() == std::io::ErrorKind::NotFound => return Ok(()),
            Err(error)
                if matches!(error.raw_os_error(), Some(39) | Some(66) | Some(145))
                    && attempt < 9 =>
            {
                std::thread::sleep(std::time::Duration::from_millis(50));
            }
            Err(error) => return Err(error),
        }
    }
    Ok(())
}
