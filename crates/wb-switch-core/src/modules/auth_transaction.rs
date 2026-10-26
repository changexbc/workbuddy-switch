//! Serialize Switch's token refresh and native auth export for one store.
//! The stable sidecar must never be unlinked: replacing it would split the lock.
//! This is cooperative; the official client does not acquire this lock.
use std::fs::{File, OpenOptions, TryLockError};
use std::io;
use std::time::{Duration, Instant};

use crate::modules::config::store_dir;

pub const BUSY_PREFIX: &str = "AUTH_STATE_BUSY:";

#[derive(Debug)]
pub struct AuthTransaction {
    _file: File,
}

/// Fail before backups or process management if refresh/export is in flight.
/// The OS releases the lock on drop, including error returns and process exit.
pub fn try_auth() -> io::Result<AuthTransaction> {
    let path = store_dir().join("auth-state.lock");
    std::fs::create_dir_all(path.parent().unwrap())?;
    let file = OpenOptions::new()
        .read(true)
        .write(true)
        .create(true)
        .truncate(false)
        .open(path)?;
    match file.try_lock() {
        Ok(()) => Ok(AuthTransaction { _file: file }),
        Err(TryLockError::WouldBlock) => Err(io::Error::new(
            io::ErrorKind::WouldBlock,
            format!("{BUSY_PREFIX} 另一个进程正在刷新或切换账号，请稍后重试"),
        )),
        Err(TryLockError::Error(error)) => Err(error),
    }
}

/// Refresh waits without blocking the async executor. Cancellation drops its
/// owned guard; a timeout is a local error, never evidence of invalid credentials.
pub async fn wait_auth(timeout: Duration) -> io::Result<AuthTransaction> {
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
