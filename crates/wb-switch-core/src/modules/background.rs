//! 桌面端和 HTTP 服务共用的账号后台任务。

use std::{future::Future, path::Path, time::Duration};

use serde_json::Value;
use tokio::time::sleep;

use super::{
    checkin, config, refresh, rotate,
    session_link::{try_lock_file, LockError},
    travel,
};

const OWNER_RETRY_INTERVAL: Duration = Duration::from_secs(30);
const ROTATE_POLL_INTERVAL: Duration = Duration::from_secs(30);

/// 整组任务在同一个锁守卫内运行。候补进程定期重试，持锁进程退出时由系统释放锁。
pub async fn run(notify_rotate: impl Fn(&Value), notify_keepalive: impl Fn()) {
    run_as_owner(&config::store_dir(), OWNER_RETRY_INTERVAL, || async {
        // 所有循环都由同一个 future 持有，取消时不会留下脱离锁守卫的循环。
        tokio::join!(
            checkin_loop(),
            travel_loop(),
            travel_claim_loop(),
            rotate_and_keepalive_loop(notify_rotate, notify_keepalive),
        );
    })
    .await;
}

async fn run_as_owner<Fut: Future<Output = ()>>(
    store: &Path,
    retry_interval: Duration,
    run_tasks: impl FnOnce() -> Fut,
) {
    let lock_path = store.join("locks").join("background-owner.lock");
    let _owner = loop {
        match try_lock_file(&lock_path) {
            Ok(owner) => break owner,
            Err(LockError::Busy) => {}
            Err(error) => eprintln!("[后台任务] {}", error.message("选主锁")),
        }
        sleep(retry_interval).await;
    };
    run_tasks().await;
}

async fn checkin_loop() {
    if let Err(error) = config::compact_checkin_logs() {
        eprintln!("[签到] 历史日志整理失败: {error}");
    }
    let _ = checkin::run_checkin_cycle(checkin::CheckinCycleMode::StartupVerify).await;
    loop {
        sleep(checkin::next_cycle_delay()).await;
        let _ = checkin::run_checkin_cycle(checkin::CheckinCycleMode::PeriodicRecovery).await;
    }
}

async fn travel_loop() {
    loop {
        let _ = travel::run_travel_cycle().await;
        sleep(travel::TRAVEL_RETRY_INTERVAL).await;
    }
}

async fn travel_claim_loop() {
    loop {
        let _ = travel::run_travel_claim_cycle().await;
        sleep(travel::TRAVEL_CLAIM_INTERVAL).await;
    }
}

async fn rotate_and_keepalive_loop(notify_rotate: impl Fn(&Value), notify_keepalive: impl Fn()) {
    let mut last_rotate_at = 0;
    let mut last_keepalive_day = String::new();
    loop {
        let cfg = config::load_auto_rotate_config();
        if cfg.get("enabled").and_then(Value::as_bool) == Some(true) {
            let interval_minutes = cfg
                .get("check_interval_minutes")
                .and_then(Value::as_i64)
                .unwrap_or(5)
                .max(1);
            let now = config::now_ms();
            if now - last_rotate_at >= interval_minutes * 60_000 {
                last_rotate_at = now;
                notify_rotate(&rotate::run_rotate_cycle().await);
            }
        }

        let today = checkin::date_str(None);
        if today != last_keepalive_day {
            last_keepalive_day = today;
            notify_keepalive();
            let _ = refresh::run_keepalive_cycle().await;
            notify_keepalive();
        }
        sleep(ROTATE_POLL_INTERVAL).await;
    }
}

#[cfg(test)]
mod tests {
    use std::{
        future::{pending, poll_fn},
        task::Poll,
    };

    use super::*;

    #[tokio::test]
    async fn follower_skips_tasks_until_the_owner_releases_the_lock() {
        let store = std::env::temp_dir().join(uuid::Uuid::new_v4().to_string());
        let lock_path = store.join("locks/background-owner.lock");
        let mut leader = Box::pin(run_as_owner(&store, Duration::ZERO, || pending()));
        poll_fn(|cx| {
            assert!(leader.as_mut().poll(cx).is_pending());
            Poll::Ready(())
        })
        .await;
        assert!(matches!(try_lock_file(&lock_path), Err(LockError::Busy)));

        let mut follower = Box::pin(run_as_owner(&store, Duration::ZERO, || async {
            assert!(matches!(try_lock_file(&lock_path), Err(LockError::Busy)));
        }));
        poll_fn(|cx| {
            assert!(follower.as_mut().poll(cx).is_pending());
            Poll::Ready(())
        })
        .await;
        drop(leader);

        tokio::time::timeout(Duration::from_secs(5), follower)
            .await
            .expect("原持锁任务退出后，候补进程应接管并执行任务");
        assert!(try_lock_file(&lock_path).is_ok());
        std::fs::remove_dir_all(store).unwrap();
    }
}
