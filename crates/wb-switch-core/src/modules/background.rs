//! 桌面端和 HTTP 服务共用的账号后台任务。

use std::{
    fs,
    future::{poll_fn, Future},
    panic::{catch_unwind, AssertUnwindSafe},
    path::Path,
    pin::pin,
    task::Poll,
    time::Duration,
};

use serde_json::Value;
use tokio::time::sleep;

use super::{
    checkin, config, refresh, rotate,
    session_link::{try_lock_file, LockError},
    travel,
};

const OWNER_RETRY_INTERVAL: Duration = Duration::from_secs(30);
const ROTATE_POLL_INTERVAL: Duration = Duration::from_secs(30);
const KEEPALIVE_POLL_INTERVAL: Duration = Duration::from_secs(30);
const KEEPALIVE_DAY_FILE: &str = "keepalive_last_day.json";

/// 整组任务在同一个锁守卫内运行。候补进程定期重试，持锁进程退出时由系统释放锁。
pub async fn run(notify_rotate: impl Fn(&Value), notify_keepalive: impl Fn()) {
    let store = config::store_dir();
    let keepalive_path = store.join(KEEPALIVE_DAY_FILE);
    run_as_owner(&store, OWNER_RETRY_INTERVAL, || async {
        // 各循环独立捕获 panic。整组关闭时与锁一起取消，不留下脱离锁的任务。
        tokio::join!(
            isolate_loop("签到", checkin_loop()),
            isolate_loop("旅行派发", travel_loop()),
            isolate_loop("旅行领取", travel_claim_loop()),
            isolate_loop("轮换", rotate_loop(notify_rotate)),
            isolate_loop("保活", keepalive_loop(&keepalive_path, notify_keepalive)),
        );
    })
    .await;
}

async fn isolate_loop(name: &str, task: impl Future<Output = ()>) {
    let mut task = pin!(task);
    poll_fn(
        |cx| match catch_unwind(AssertUnwindSafe(|| task.as_mut().poll(cx))) {
            Ok(status) => status,
            Err(_) => {
                eprintln!("[后台任务] {name}循环发生 panic，已停止");
                Poll::Ready(())
            }
        },
    )
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

async fn rotate_loop(notify_rotate: impl Fn(&Value)) {
    let mut last_rotate_at = 0;
    loop {
        let cfg = config::load_auto_rotate_config();
        if cfg.get("enabled").and_then(Value::as_bool) == Some(true) {
            let interval_minutes = cfg
                .get("check_interval_minutes")
                .and_then(Value::as_i64)
                .unwrap_or(5)
                .max(1);
            let now = config::now_ms();
            if now - last_rotate_at >= interval_minutes.saturating_mul(60_000) {
                last_rotate_at = now;
                notify_rotate(&rotate::run_rotate_cycle().await);
            }
        }
        sleep(ROTATE_POLL_INTERVAL).await;
    }
}

async fn keepalive_loop(path: &Path, notify_keepalive: impl Fn()) {
    let mut last_day = load_keepalive_day(path);
    loop {
        let ran = run_keepalive_if_due(path, &mut last_day, checkin::date_str(None), || async {
            notify_keepalive();
            let _ = refresh::run_keepalive_cycle().await;
        })
        .await;
        if ran {
            notify_keepalive();
        }
        sleep(KEEPALIVE_POLL_INTERVAL).await;
    }
}

fn load_keepalive_day(path: &Path) -> String {
    fs::read_to_string(path)
        .ok()
        .and_then(|text| serde_json::from_str(&text).ok())
        .unwrap_or_default()
}

async fn run_keepalive_if_due<Fut: Future<Output = ()>>(
    path: &Path,
    last_day: &mut String,
    today: String,
    run_cycle: impl FnOnce() -> Fut,
) -> bool {
    if *last_day == today {
        return false;
    }
    run_cycle().await;
    if let Err(error) = config::atomic_write(path, &serde_json::json!(&today).to_string()) {
        eprintln!("[保活] 保存上次执行日期失败: {error}");
    }
    *last_day = today;
    true
}

#[cfg(test)]
mod tests {
    use std::future::pending;

    use super::*;

    #[tokio::test]
    async fn follower_skips_tasks_until_the_owner_releases_the_lock() {
        let store = std::env::temp_dir().join(uuid::Uuid::new_v4().to_string());
        let lock_path = store.join("locks/background-owner.lock");
        let retry = Duration::from_millis(5);
        let mut leader = Box::pin(run_as_owner(&store, retry, || pending()));
        poll_fn(|cx| {
            assert!(leader.as_mut().poll(cx).is_pending());
            Poll::Ready(())
        })
        .await;
        assert!(matches!(try_lock_file(&lock_path), Err(LockError::Busy)));

        let mut follower = Box::pin(run_as_owner(&store, retry, || async {
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

    #[tokio::test]
    async fn a_loop_panic_keeps_other_tasks_running_under_the_owner_lock() {
        let store = std::env::temp_dir().join(uuid::Uuid::new_v4().to_string());
        let lock_path = store.join("locks/background-owner.lock");
        let (started, other_started) = tokio::sync::oneshot::channel();
        let (finish, other_finish) = tokio::sync::oneshot::channel();
        let mut owner = Box::pin(run_as_owner(&store, Duration::from_millis(5), || async {
            tokio::join!(
                isolate_loop("测试故障", async { panic!("测试循环 panic") }),
                isolate_loop("测试正常", async {
                    started.send(()).unwrap();
                    other_finish.await.unwrap();
                }),
            );
        }));

        tokio::select! {
            _ = &mut owner => panic!("还有循环运行时不能释放选主锁"),
            result = other_started => result.unwrap(),
        }
        assert!(matches!(try_lock_file(&lock_path), Err(LockError::Busy)));
        finish.send(()).unwrap();
        owner.await;
        assert!(try_lock_file(&lock_path).is_ok());
        fs::remove_dir_all(store).unwrap();
    }

    #[tokio::test]
    async fn takeover_skips_keepalive_until_the_next_day() {
        let store = std::env::temp_dir().join(uuid::Uuid::new_v4().to_string());
        fs::create_dir(&store).unwrap();
        let path = store.join(KEEPALIVE_DAY_FILE);
        let first_day = "2026-11-27";
        let next_day = "2026-11-28";
        let (mut first_runs, mut next_runs) = (0, 0);

        let mut last_day = load_keepalive_day(&path);
        run_keepalive_if_due(&path, &mut last_day, first_day.into(), || async {
            first_runs += 1;
        })
        .await;
        assert_eq!(first_runs, 1);

        let mut successor_day = load_keepalive_day(&path);
        assert_eq!(successor_day, first_day);
        run_keepalive_if_due(&path, &mut successor_day, first_day.into(), || async {
            panic!("接管当天不应重复保活");
        })
        .await;
        run_keepalive_if_due(&path, &mut successor_day, next_day.into(), || async {
            next_runs += 1;
        })
        .await;
        assert_eq!(next_runs, 1);
        assert_eq!(load_keepalive_day(&path), next_day);
        fs::remove_dir_all(store).unwrap();
    }
}
