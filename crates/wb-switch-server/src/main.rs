//! workbuddy-switch CLI：npm 安装形态的入口。
//!
//! ```bash
//! workbuddy-switch              # 启动本地服务 + 打开浏览器 webui
//! workbuddy-switch serve        # 只起服务不开浏览器（--host / --port / --no-open）
//! workbuddy-switch status       # 终端输出当前账号
//! workbuddy-switch version      # 版本号
//! ```
//!
//! 监听地址可用 `--host <IP>` / `--port <端口>` 自定义（支持 `--key=value` 写法），
//! 也可用环境变量 `WB_SWITCH_HOST` / `WB_SWITCH_PORT` 兜底；默认 127.0.0.1:57890。

mod api;

use std::net::IpAddr;

use serde_json::json;

use wb_switch_core::modules::{
    account, auth_file, checkin, config, process, rotate, travel, update, variant::WbVariant,
};

fn default_host() -> IpAddr {
    IpAddr::from([127, 0, 0, 1])
}

fn default_port() -> u16 {
    57890
}

/// 后台任务：自动签到启动即核验，之后按 core 计算的下一轮延迟睡眠（未设置
/// 签到时间段时固定 30 分钟）；自动轮换按配置间隔执行；
/// 限额 hook 信号每秒轮询一次、启动时后台默认接入。
fn spawn_background_loops() {
    tokio::spawn(async move {
        if let Err(error) = config::compact_checkin_logs() {
            eprintln!("[签到] 历史日志整理失败: {error}");
        }
        let _ = checkin::run_checkin_cycle(checkin::CheckinCycleMode::StartupVerify).await;
        loop {
            tokio::time::sleep(checkin::next_cycle_delay()).await;
            let _ = checkin::run_checkin_cycle(checkin::CheckinCycleMode::PeriodicRecovery).await;
        }
    });

    tokio::spawn(async move {
        let mut last_cycle_at: i64 = 0;
        loop {
            let cfg = config::load_auto_rotate_config();
            if cfg.get("enabled").and_then(|v| v.as_bool()) == Some(true) {
                let interval_minutes = cfg
                    .get("check_interval_minutes")
                    .and_then(|v| v.as_i64())
                    .unwrap_or(5)
                    .max(1);
                let now = config::now_ms();
                if now - last_cycle_at >= interval_minutes * 60_000 {
                    last_cycle_at = now;
                    let _ = rotate::run_rotate_cycle().await;
                }
            }
            tokio::time::sleep(std::time::Duration::from_secs(30)).await;
        }
    });

    // 派猫猫旅行：启动即派发，之后周期性补派（并重试 no-buddy / 瞬时错误）。
    tokio::spawn(async move {
        let _ = travel::run_travel_cycle().await;
        loop {
            tokio::time::sleep(travel::TRAVEL_RETRY_INTERVAL).await;
            let _ = travel::run_travel_cycle().await;
        }
    });

    // 旅行领取：启动立刻查一轮（避免重启后空等 15 分钟漏领），之后按周期检查。
    tokio::spawn(async move {
        let _ = travel::run_travel_claim_cycle().await;
        loop {
            tokio::time::sleep(travel::TRAVEL_CLAIM_INTERVAL).await;
            let _ = travel::run_travel_claim_cycle().await;
        }
    });

    // 限额 hook 信号：轮询 `~/.wb-switch/hook-events.jsonl`，入账后由前端下次拉取可见。
    // webui 没有 Tauri 事件通道，因此不需要推送回调（桌面端见 src-tauri/src/lib.rs）。
    wb_switch_core::modules::rate_limit_events::spawn_watcher(|| {});

    // 默认接入：后台线程自动安装 hook（幂等、非阻塞、失败静默）；
    // 装上了就作废扫描缓存——扫描范围从全量收窄到「未注册的来源」。
    std::thread::spawn(|| {
        if wb_switch_core::modules::rate_limit_hook::auto_install_on_startup() {
            wb_switch_core::modules::limits::invalidate_scan_cache();
        }
    });
}

/// CLI 档位参数：`--variant ai` / `--variant=ai`；缺省国内版。
///
/// 与 Tauri 命令的可选 `variant` 参数、HTTP 路由的 query/body 字段同义。
fn variant_arg(args: &[String]) -> WbVariant {
    let raw = args.iter().enumerate().find_map(|(index, arg)| {
        if let Some(value) = arg.strip_prefix("--variant=") {
            return Some(value.to_string());
        }
        arg.eq("--variant")
            .then(|| args.get(index + 1).cloned().unwrap_or_default())
    });
    WbVariant::parse(raw.as_deref())
}

/// 通用开关取值：`--key value` / `--key=value`，与 `variant_arg` 同款写法。
/// 值缺失（开关是最后一个参数）时返回空串，由调用方按未提供处理。
fn flag_value(args: &[String], key: &str) -> Option<String> {
    let prefix = format!("{key}=");
    args.iter().enumerate().find_map(|(index, arg)| {
        if let Some(value) = arg.strip_prefix(&prefix) {
            return Some(value.to_string());
        }
        arg.eq(key)
            .then(|| args.get(index + 1).cloned().unwrap_or_default())
    })
}

/// serve 的监听参数：CLI `--host` / `--port` 优先，其次环境变量
/// `WB_SWITCH_HOST` / `WB_SWITCH_PORT`，最后默认 127.0.0.1:57890。
/// 显式提供的值非法时直接报错，避免静默回落默认值让用户误以为生效。
fn resolve_listen_args(args: &[String]) -> Result<(IpAddr, u16), String> {
    let host = match flag_value(args, "--host") {
        Some(raw) if !raw.is_empty() => raw.parse::<IpAddr>().map_err(|_| {
            format!("--host 值无效: {raw}。请使用 IP 地址（如 127.0.0.1 / 0.0.0.0 / ::）")
        })?,
        _ => match std::env::var("WB_SWITCH_HOST") {
            Ok(raw) if !raw.trim().is_empty() => raw.trim().parse::<IpAddr>().map_err(|_| {
                format!("环境变量 WB_SWITCH_HOST 值无效: {raw}。请使用 IP 地址（如 127.0.0.1 / 0.0.0.0 / ::）")
            })?,
            _ => default_host(),
        },
    };

    let port = match flag_value(args, "--port") {
        Some(raw) if !raw.is_empty() => {
            let port = raw
                .parse::<u16>()
                .map_err(|_| format!("--port 值无效: {raw}。端口范围 1-65535"))?;
            if port == 0 {
                return Err("--port 值无效: 0。端口范围 1-65535".to_string());
            }
            port
        }
        _ => match std::env::var("WB_SWITCH_PORT") {
            Ok(raw) if !raw.trim().is_empty() => {
                let port = raw.trim().parse::<u16>().map_err(|_| {
                    format!("环境变量 WB_SWITCH_PORT 值无效: {raw}。端口范围 1-65535")
                })?;
                if port == 0 {
                    return Err("环境变量 WB_SWITCH_PORT 值无效: 0。端口范围 1-65535".to_string());
                }
                port
            }
            _ => default_port(),
        },
    };

    Ok((host, port))
}

/// 地址展示串：IPv6 需要方括号（如 `[::1]:57890`），同时兼容 TcpListener::bind 的解析。
fn socket_display(host: IpAddr, port: u16) -> String {
    if host.is_ipv6() {
        format!("[{host}]:{port}")
    } else {
        format!("{host}:{port}")
    }
}

/// 浏览器打开用的地址：绑定通配地址（0.0.0.0 / ::）时换成回环地址才能访问。
fn browser_host(host: IpAddr) -> IpAddr {
    if !host.is_unspecified() {
        return host;
    }
    if host.is_ipv4() {
        IpAddr::from([127, 0, 0, 1])
    } else {
        IpAddr::from([0, 0, 0, 0, 0, 0, 0, 1])
    }
}

fn print_status(variant: WbVariant) {
    let auth = auth_file::read_auth_file(variant);
    let current = auth.as_ref().map(|a| {
        let acct = a.get("account").cloned().unwrap_or_else(|| json!({}));
        json!({
            "uid": account::display_value(&acct, "uid"),
            "nickname": account::display_value(&acct, "nickname"),
            "email": account::display_value(&acct, "email"),
        })
    });
    let running = process::is_workbuddy_running(variant);
    println!("workbuddy-switch v{}", update::APP_VERSION);
    println!("WorkBuddy 运行中: {}", if running { "是" } else { "否" });
    match current {
        Some(c) => {
            let name = c
                .get("nickname")
                .and_then(|v| v.as_str())
                .or_else(|| c.get("email").and_then(|v| v.as_str()))
                .unwrap_or("未知");
            println!("当前账号: {name}");
        }
        None => println!("当前账号: 未登录"),
    }
    println!("账号数: {}", account::load_accounts().len());
}

#[tokio::main]
async fn main() {
    let args: Vec<String> = std::env::args().collect();
    let cmd = args.get(1).map(|s| s.as_str()).unwrap_or("serve");
    match cmd {
        "status" => print_status(variant_arg(&args)),
        "version" | "--version" | "-V" => {
            println!("workbuddy-switch {}", env!("CARGO_PKG_VERSION"));
        }
        _ => serve(&args).await,
    }
}

async fn serve(args: &[String]) {
    let (host, port) = match resolve_listen_args(args) {
        Ok(pair) => pair,
        Err(message) => {
            eprintln!("启动失败: {message}");
            std::process::exit(1);
        }
    };

    let app = api::router();
    let addr = socket_display(host, port);
    let listener = match tokio::net::TcpListener::bind(&addr).await {
        Ok(l) => l,
        Err(e) => {
            eprintln!("启动失败: {addr} 绑定失败，端口可能被占用或地址不可用（{e}）。可用 --host / --port 指定其他地址与端口。");
            std::process::exit(1);
        }
    };

    println!("workbuddy-switch v{}", update::APP_VERSION);
    println!("webui: http://{addr}");
    if !host.is_loopback() {
        eprintln!(
            "警告: 服务绑定在非回环地址 {host}，账号 Token 等敏感数据将可被局域网 / 外网访问，且接口无鉴权。\
             请确认网络安全，或改用 SSH 端口转发。"
        );
    }
    println!("按 Ctrl+C 停止服务。");

    let no_open = args.iter().any(|a| a == "--no-open");
    if !no_open {
        open_browser(&format!(
            "http://{}",
            socket_display(browser_host(host), port)
        ));
    }

    spawn_background_loops();

    axum::serve(listener, app).await.unwrap();
}

fn open_browser(addr: &str) {
    let url = format!("http://{addr}");
    #[cfg(target_os = "macos")]
    {
        let _ = std::process::Command::new("open").arg(&url).spawn();
    }
    #[cfg(target_os = "windows")]
    {
        let mut c = std::process::Command::new("cmd");
        #[cfg(target_os = "windows")]
        {
            use std::os::windows::process::CommandExt;
            c.creation_flags(0x0800_0000); // CREATE_NO_WINDOW：开浏览器不闪 cmd 窗
        }
        let _ = c.args(["/C", "start", &url]).spawn();
    }
    #[cfg(target_os = "linux")]
    {
        let _ = std::process::Command::new("xdg-open").arg(&url).spawn();
    }
}

#[cfg(test)]
mod tests {
    use super::{browser_host, resolve_listen_args, socket_display, variant_arg};
    use std::net::IpAddr;
    use wb_switch_core::modules::variant::WbVariant;

    fn args(items: &[&str]) -> Vec<String> {
        items.iter().map(|s| s.to_string()).collect()
    }

    /// 不传档位时必须仍是国内版（改造前行为）。
    #[test]
    fn cli_variant_defaults_to_cn() {
        assert_eq!(variant_arg(&args(&["status"])), WbVariant::Cn);
        assert_eq!(variant_arg(&args(&["status", "--debug"])), WbVariant::Cn);
        assert_eq!(variant_arg(&args(&["status", "--variant"])), WbVariant::Cn);
        assert_eq!(variant_arg(&args(&["status", "cn"])), WbVariant::Cn);
        assert_eq!(
            variant_arg(&args(&["status", "--variant=cn"])),
            WbVariant::Cn
        );
    }

    #[test]
    fn cli_variant_reads_space_and_equals_forms() {
        assert_eq!(
            variant_arg(&args(&["status", "--variant", "ai"])),
            WbVariant::Ai
        );
        assert_eq!(
            variant_arg(&args(&["status", "--variant=ai"])),
            WbVariant::Ai
        );
        assert_eq!(
            variant_arg(&args(&["status", "--variant", "AI", "--no-open"])),
            WbVariant::Ai
        );
        assert_eq!(
            variant_arg(&args(&["--variant=ai", "status"])),
            WbVariant::Ai
        );
    }

    /// 不传 --host / --port 时回落默认 127.0.0.1:57890。
    #[test]
    fn listen_args_default_to_loopback() {
        let (host, port) = resolve_listen_args(&args(&[])).unwrap();
        assert_eq!(host, IpAddr::from([127, 0, 0, 1]));
        assert_eq!(port, 57890);
        let (host, port) = resolve_listen_args(&args(&["serve", "--no-open"])).unwrap();
        assert_eq!(host, IpAddr::from([127, 0, 0, 1]));
        assert_eq!(port, 57890);
    }

    /// --host / --port 支持空格与等号两种写法，并可任意组合。
    #[test]
    fn listen_args_read_space_and_equals_forms() {
        let (host, port) =
            resolve_listen_args(&args(&["serve", "--host", "0.0.0.0", "--port", "8080"])).unwrap();
        assert_eq!(host, IpAddr::from([0, 0, 0, 0]));
        assert_eq!(port, 8080);

        let (host, port) =
            resolve_listen_args(&args(&["--host=192.168.1.10", "--port=6100"])).unwrap();
        assert_eq!(host, IpAddr::from([192, 168, 1, 10]));
        assert_eq!(port, 6100);

        let (host, _) = resolve_listen_args(&args(&["--host", "::1"])).unwrap();
        assert_eq!(host, IpAddr::from([0, 0, 0, 0, 0, 0, 0, 1]));

        // 只给一个参数时另一个仍回落默认值。
        let (host, port) = resolve_listen_args(&args(&["--port", "7000"])).unwrap();
        assert_eq!(host, IpAddr::from([127, 0, 0, 1]));
        assert_eq!(port, 7000);
    }

    /// 显式提供的非法值必须报错，而不是静默回落默认值。
    #[test]
    fn listen_args_reject_invalid_values() {
        // 域名不支持：绑定需要 IP 地址。
        assert!(resolve_listen_args(&args(&["--host", "localhost"])).is_err());
        assert!(resolve_listen_args(&args(&["--host", "192.168.1.256"])).is_err());
        assert!(resolve_listen_args(&args(&["--port", "99999"])).is_err());
        assert!(resolve_listen_args(&args(&["--port", "abc"])).is_err());
        assert!(resolve_listen_args(&args(&["--port", "0"])).is_err());
    }

    #[test]
    fn socket_display_brackets_ipv6() {
        assert_eq!(
            socket_display(IpAddr::from([127, 0, 0, 1]), 57890),
            "127.0.0.1:57890"
        );
        assert_eq!(
            socket_display(IpAddr::from([0, 0, 0, 0, 0, 0, 0, 1]), 80),
            "[::1]:80"
        );
    }

    /// 通配地址开浏览器时换回环地址；其余原样返回。
    #[test]
    fn browser_host_maps_unspecified_to_loopback() {
        assert_eq!(
            browser_host(IpAddr::from([0, 0, 0, 0])),
            IpAddr::from([127, 0, 0, 1])
        );
        assert_eq!(
            browser_host(IpAddr::from([0, 0, 0, 0, 0, 0, 0, 0])),
            IpAddr::from([0, 0, 0, 0, 0, 0, 0, 1])
        );
        assert_eq!(
            browser_host(IpAddr::from([192, 168, 1, 10])),
            IpAddr::from([192, 168, 1, 10])
        );
    }
}
