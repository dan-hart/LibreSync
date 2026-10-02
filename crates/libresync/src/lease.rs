//! An exclusive lease belongs to its runtime owner, not inherited descriptors.
use crate::{Error, Result, SessionErrorCode};
use std::fs::File;

pub(crate) struct StateLease(File);

impl StateLease {
    pub(crate) fn acquire(file: File, description: &str) -> Result<Self> {
        #[cfg(target_os = "android")]
        {
            use std::os::fd::AsRawFd;
            // Rust 1.91 std file locks are unsupported on Android; Bionic has flock.
            // SAFETY: file owns a live descriptor; these flags have no pointer arguments.
            if unsafe { libc::flock(file.as_raw_fd(), libc::LOCK_EX | libc::LOCK_NB) } != 0 {
                let error = std::io::Error::last_os_error();
                return Err(Error::Managed {
                    code: if error.kind() == std::io::ErrorKind::WouldBlock {
                        SessionErrorCode::Busy
                    } else {
                        SessionErrorCode::StorageUnavailable
                    },
                    message: format!("{description} lease failed: {error}"),
                });
            }
        }
        #[cfg(not(target_os = "android"))]
        file.try_lock().map_err(|error| match error {
            std::fs::TryLockError::WouldBlock => Error::Managed {
                code: SessionErrorCode::Busy,
                message: format!("{description} is already open"),
            },
            std::fs::TryLockError::Error(error) => Error::Managed {
                code: SessionErrorCode::StorageUnavailable,
                message: format!("{description} lease failed: {error}"),
            },
        })?;
        Ok(Self(file))
    }

    // Models a fork-inherited open-file description without unsafe process forks.
    #[cfg(test)]
    pub(crate) fn try_clone(&self) -> std::io::Result<File> {
        self.0.try_clone()
    }
}

impl Drop for StateLease {
    fn drop(&mut self) {
        // Explicit unlock ends ownership even when fork/dup retains this open-file
        // description. The kernel still releases the lock when all descriptors close,
        // including process exit, or if best-effort explicit unlock fails. Owners place this guard last,
        // after all runtime fields, and stop workers before field destruction.
        #[cfg(target_os = "android")]
        {
            use std::os::fd::AsRawFd;
            // SAFETY: self still owns a live descriptor; LOCK_UN has no pointer argument.
            let _ = unsafe { libc::flock(self.0.as_raw_fd(), libc::LOCK_UN) };
        }
        #[cfg(not(target_os = "android"))]
        let _ = self.0.unlock();
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::{fs::OpenOptions, path::Path, time::Duration};

    fn acquire(path: &Path) -> Result<StateLease> {
        StateLease::acquire(
            OpenOptions::new()
                .read(true)
                .write(true)
                .create(true)
                .truncate(false)
                .open(path)?,
            "test state directory",
        )
    }

    #[test]
    fn process_exit_releases_lease_without_running_drop() {
        const ROLE: &str = "LIBRESYNC_LEASE_EXIT_TEST_DIR";
        if let Some(directory) = std::env::var_os(ROLE) {
            let directory = std::path::PathBuf::from(directory);
            let _owner = acquire(&directory.join("state.lock")).unwrap();
            std::fs::write(directory.join("ready"), b"ready").unwrap();
            let deadline = std::time::Instant::now() + Duration::from_secs(15);
            while !directory.join("exit").exists() {
                assert!(
                    std::time::Instant::now() < deadline,
                    "parent did not release child"
                );
                std::thread::sleep(Duration::from_millis(5));
            }
            // Unlike a normal return, exit does not invoke StateLease::drop.
            std::process::exit(0);
        }
        let directory = tempfile::tempdir().unwrap();
        let mut child = std::process::Command::new(std::env::current_exe().unwrap())
            .args([
                "--exact",
                "lease::tests::process_exit_releases_lease_without_running_drop",
            ])
            .env(ROLE, directory.path())
            .spawn()
            .unwrap();
        let deadline = std::time::Instant::now() + Duration::from_secs(10);
        while !directory.path().join("ready").exists() {
            if std::time::Instant::now() >= deadline {
                let _ = child.kill();
                let _ = child.wait();
                panic!("child did not acquire lease");
            }
            std::thread::sleep(Duration::from_millis(5));
        }
        let excluded = matches!(
            acquire(&directory.path().join("state.lock")),
            Err(Error::Managed {
                code: SessionErrorCode::Busy,
                ..
            })
        );
        std::fs::write(directory.path().join("exit"), b"exit").unwrap();
        assert!(child.wait().unwrap().success());
        assert!(excluded, "live child must exclude a second owner");
        let reopened = acquire(&directory.path().join("state.lock")).unwrap();
        drop(reopened);
    }
}
