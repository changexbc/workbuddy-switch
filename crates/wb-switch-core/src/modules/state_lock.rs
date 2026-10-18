//! Cooperative cross-process locks for shared account/CLI authentication state.
//! Lock files are stable sidecars: never unlink them or lock files replaced by
//! atomic_write. Closing the OS handle (also on process exit) releases the lock.
//!
//! One store-wide lock deliberately spans token rotation over the network and
//! its local commit: account deletion/import, token refresh and client switch
//! must not overwrite each other. Metadata edits on other accounts may be busy
//! during refresh. Fine-grained locks need a separate merge/lock-order design.
//! std::fs::File locking requires Rust 1.89; no native lock dependency is added.
use std::fs::{File, OpenOptions, TryLockError};
use std::io;
use std::path::Path;
use std::time::{Duration, Instant};

use crate::modules::config::{atomic_write, store_dir};

pub const BUSY_PREFIX: &str = "AUTH_STATE_BUSY:";
const AUTH_LOCK: &str = "auth-state.lock";
const REVISION: &str = "auth-state-revision";

#[derive(Debug)]
pub struct StateLock {
    _file: File,
}

impl StateLock {
    pub fn try_at(path: &Path) -> io::Result<Self> {
        if let Some(parent) = path.parent() {
            std::fs::create_dir_all(parent)?;
        }
        let file = OpenOptions::new()
            .read(true)
            .write(true)
            .create(true)
            .truncate(false)
            .open(path)?;
        match file.try_lock() {
            Ok(()) => Ok(Self { _file: file }),
            Err(TryLockError::WouldBlock) => Err(io::Error::new(
                io::ErrorKind::WouldBlock,
                format!("{BUSY_PREFIX} 另一个进程正在更新账号或 CLI 认证状态，请稍后重试"),
            )),
            Err(TryLockError::Error(error)) => Err(error),
        }
    }

    /// Bounded synchronous wait for short cache read-modify-write transactions.
    pub fn wait_at(path: &Path, timeout: Duration) -> io::Result<Self> {
        let started = Instant::now();
        loop {
            match Self::try_at(path) {
                Err(error)
                    if error.kind() == io::ErrorKind::WouldBlock && started.elapsed() < timeout =>
                {
                    std::thread::sleep(Duration::from_millis(20));
                }
                result => return result,
            }
        }
    }
}

/// Mutations fail before closing processes/writing files if another host owns
/// the authentication transaction. Do not reacquire while holding this guard.
pub fn try_auth() -> io::Result<StateLock> {
    StateLock::try_at(&store_dir().join(AUTH_LOCK))
}

/// Refresh can wait without blocking an async runtime thread. Cancellation drops
/// the owned handle; the timeout is independent of network request timeouts.
pub async fn wait_auth(timeout: Duration) -> io::Result<StateLock> {
    let started = Instant::now();
    loop {
        match try_auth() {
            Err(error)
                if error.kind() == io::ErrorKind::WouldBlock && started.elapsed() < timeout =>
            {
                tokio::time::sleep(Duration::from_millis(50)).await;
            }
            result => return result,
        }
    }
}

/// Call after a committed mutation, while still holding the authentication lock.
/// Notification failure must not misreport a successfully committed switch.
pub(crate) fn signal_auth_change(_guard: &StateLock) {
    if let Err(error) = atomic_write(
        &store_dir().join(REVISION),
        &uuid::Uuid::new_v4().to_string(),
    ) {
        eprintln!("[auth-state] 更新通知失败: {error}");
    }
}

/// GUI polling reads a committed revision only; skip transactions still in flight.
pub fn committed_revision() -> Option<String> {
    let _guard = try_auth().ok()?;
    std::fs::read_to_string(store_dir().join(REVISION)).ok()
}

pub fn spawn_revision_watcher(on_change: impl Fn() + Send + 'static) {
    std::thread::spawn(move || {
        let mut previous = committed_revision();
        loop {
            std::thread::sleep(Duration::from_secs(1));
            if let Some(revision) = committed_revision() {
                if previous.as_ref() != Some(&revision) {
                    previous = Some(revision);
                    on_change();
                }
            }
        }
    });
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn independent_handles_exclude_each_other_and_drop_releases() {
        let root = std::env::temp_dir().join(format!("wb-state-lock-{}", uuid::Uuid::new_v4()));
        let path = root.join("state.lock");
        let guard = StateLock::try_at(&path).unwrap();
        assert_eq!(
            StateLock::try_at(&path).unwrap_err().kind(),
            io::ErrorKind::WouldBlock
        );
        drop(guard);
        let guard = StateLock::try_at(&path).unwrap();
        drop(guard);
        std::fs::remove_dir_all(root).unwrap();
    }
}
