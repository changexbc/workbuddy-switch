//! WebDAV 备份 / 同步端到端测试：本进程起一个最小 WebDAV 服务器
//! （PROPFIND / MKCOL / PUT(ETag + If-Match) / GET），把两台「设备」
//! （两个 `WB_SWITCH_HOME` 临时家目录）指向同一服务器，验证
//! 测试连接 → 备份 → 云端信息 → 跨设备同步 → 恢复 全链路。

use std::collections::HashMap;
use std::io::{BufRead, Read, Write};
use std::net::{TcpListener, TcpStream};
use std::sync::Arc;

use serde_json::{json, Value};

/// 最小 WebDAV 服务器状态：目录集合 + 文件（内容, ETag 版本）。
#[derive(Default)]
struct DavState {
    dirs: Vec<String>,
    files: HashMap<String, (Vec<u8>, u64)>,
    etag_seq: u64,
}

fn canonical_dir(path: &str) -> String {
    let trimmed = path.trim_end_matches('/');
    if trimmed.is_empty() {
        "/".to_string()
    } else {
        format!("{trimmed}/")
    }
}

fn ensure_dirs_up_to(state: &mut DavState, path: &str) {
    // 逐级登记绝对路径目录（"/a/b/c/" → ["/a/", "/a/b/", "/a/b/c/"]）。
    let mut acc = String::new();
    for seg in path.split('/').filter(|s| !s.is_empty()) {
        if !acc.is_empty() {
            acc.push('/');
        }
        acc.push_str(seg);
        let dir = format!("/{acc}/");
        if !state.dirs.contains(&dir) {
            state.dirs.push(dir);
        }
    }
}

fn handle_request(
    state: &mut DavState,
    method: &str,
    path: &str,
    headers: &HashMap<String, String>,
    body: &[u8],
) -> (u16, Vec<(String, String)>, Vec<u8>) {
    let file_path = canonical_dir(path);
    match method {
        "PROPFIND" => {
            if state.dirs.contains(&file_path) {
                (207, vec![("Depth".into(), "0".into())], b"<multistatus/>".to_vec())
            } else {
                (404, vec![], b"not found".to_vec())
            }
        }
        "MKCOL" => {
            if state.dirs.contains(&file_path) {
                return (405, vec![], b"already exists".to_vec());
            }
            // 父目录：剥掉末尾斜杠再找上一段，"/" 无父目录。
            let p = file_path.trim_end_matches('/');
            let parent = p
                .rfind('/')
                .map(|i| canonical_dir(&p[..i + 1]))
                .filter(|d| d != "/");
            if let Some(parent) = parent {
                if !state.dirs.contains(&parent) {
                    return (409, vec![], b"parent missing".to_vec());
                }
            }
            ensure_dirs_up_to(state, &file_path);
            (201, vec![], Vec::new())
        }
        "PUT" => {
            let dir = file_path
                .trim_end_matches('/')
                .rfind('/')
                .map(|i| canonical_dir(&file_path[..i + 1]))
                .filter(|d| d != "/");
            if let Some(dir) = dir {
                if !state.dirs.contains(&dir) {
                    return (409, vec![], b"parent missing".to_vec());
                }
            }
            // If-Match：与服务端当前 ETag 不一致 → 412（条件写冲突）。
            if let Some(tag) = headers.get("if-match") {
                let current = state.files.get(&file_path).and_then(|(content, v)| {
                    let _ = content;
                    if *v > 0 {
                        Some(format!("\"v{v}\""))
                    } else {
                        None
                    }
                });
                match current {
                    Some(c) if c != *tag => return (412, vec![], b"precondition failed".to_vec()),
                    None => return (412, vec![], b"no such file".to_vec()),
                    _ => {}
                }
            }
            let existed = state.files.contains_key(&file_path);
            state.etag_seq += 1;
            let version = state.etag_seq;
            state.files.insert(file_path.clone(), (body.to_vec(), version));
            let status = if existed { 204 } else { 201 };
            (status, vec![("etag".into(), format!("\"v{version}\""))], Vec::new())
        }
        "GET" => {
            let entry = match state.files.get(&file_path) {
                Some(entry) => entry.clone(),
                None => return (404, vec![], b"not found".to_vec()),
            };
            (
                200,
                vec![
                    ("etag".into(), format!("\"v{}\"", entry.1)),
                    ("content-length".into(), entry.0.len().to_string()),
                ],
                entry.0,
            )
        }
        other => (501, vec![], format!("unsupported method {other}").into_bytes()),
    }
}

fn serve_connection(stream: TcpStream, state: Arc<std::sync::Mutex<DavState>>) {
    // 读用 BufReader（带内层读缓冲），写用底层 stream。
    let mut reader = std::io::BufReader::new(stream);
    let mut out_stream = reader.get_ref().try_clone().expect("clone stream");
    loop {
        let mut line = String::new();
        if reader.read_line(&mut line).unwrap_or(0) == 0 {
            return;
        }
        let parts: Vec<&str> = line.split_whitespace().collect();
        if parts.len() < 2 {
            continue;
        }
        let method = parts[0].to_uppercase();
        let path = parts[1].to_string();
        let mut headers: HashMap<String, String> = HashMap::new();
        loop {
            let mut h = String::new();
            if reader.read_line(&mut h).unwrap_or(0) == 0 {
                return;
            }
            let h = h.trim_end();
            if h.is_empty() {
                break;
            }
            if let Some((k, v)) = h.split_once(':') {
                headers.insert(k.trim().to_ascii_lowercase(), v.trim().to_string());
            }
        }
        let body_len = headers
            .get("content-length")
            .and_then(|l| l.parse::<usize>().ok())
            .unwrap_or(0);
        // 必须从 BufReader 本身读 body（先排空内部缓冲再读 socket）；
        // 直接用 `get_mut()` 读会漏掉预读缓冲里的字节，请求体不完整时永久挂起。
        let mut body = vec![0u8; body_len];
        let mut got = 0;
        while got < body.len() {
            match reader.read(&mut body[got..]) {
                Ok(0) | Err(_) => break,
                Ok(n) => got += n,
            }
        }

        let (status, resp_headers, resp_body) = {
            let mut state = state.lock().unwrap();
            handle_request(&mut state, &method, &path, &headers, &body)
        };

        let mut out = format!("HTTP/1.1 {status} X\r\n");
        for (k, v) in &resp_headers {
            out.push_str(&format!("{k}: {v}\r\n"));
        }
        // 一律 close 后退出本连接：reqwest 共享客户端的 keep-alive 连接池不会主动
        // 断开，若服务端保持连接，serve 线程会永久阻塞在 read_line，作为非
        // daemon 线程挂住测试进程退出。close 让 reqwest 按需重建连接，语义等价。
        out.push_str(&format!(
            "Content-Length: {}\r\nConnection: close\r\n\r\n",
            resp_body.len()
        ));
        if out_stream
            .write_all(out.as_bytes())
            .and_then(|_| out_stream.write_all(&resp_body))
            .is_err()
        {
            return;
        }
        break;
    }
}

/// 启动最小 WebDAV 服务器（线程常驻），返回 base URL 与 stop 标志。
///
/// 测试结束时置 `stop=true` 让 accept 循环退出：否则非 daemon 线程会让进程
/// 正常结束路径永久挂起（测试全部通过后 `cargo test` 永不返回）。
fn start_server() -> (String, Arc<std::sync::atomic::AtomicBool>) {
    use std::sync::atomic::AtomicBool;
    let listener = TcpListener::bind("127.0.0.1:0").expect("bind 127.0.0.1:0");
    listener
        .set_nonblocking(true)
        .expect("set_nonblocking");
    let addr = listener.local_addr().unwrap();
    let state = Arc::new(std::sync::Mutex::new(DavState::default()));
    let stop = Arc::new(AtomicBool::new(false));
    let stop_flag = Arc::clone(&stop);
    std::thread::spawn(move || {
        state.lock().unwrap().dirs.push("/".to_string());
        loop {
            match listener.accept() {
                Ok((stream, _)) => {
                    stream
                        .set_nonblocking(false)
                        .expect("restore blocking for connection");
                    let state = Arc::clone(&state);
                    std::thread::spawn(move || serve_connection(stream, state));
                }
                Err(ref e)
                    if e.kind() == std::io::ErrorKind::WouldBlock => {
                        if stop_flag.load(std::sync::atomic::Ordering::SeqCst) {
                            break;
                        }
                        std::thread::sleep(std::time::Duration::from_millis(50));
                    }
                Err(_) => break,
            }
        }
    });
    (format!("http://{addr}"), stop)
}

/// 原始 HTTP（单连接单请求）：PUT 带 If-Match，返回状态码。
fn raw_put(url: &str, body: &[u8], if_match: Option<&str>) -> u16 {
    let without_scheme = url.trim_start_matches("http://");
    // 分离出 host 与端口（只取首段，路径部分不含在 host:port 内）。
    let host_port = without_scheme.split('/').next().expect("host:port");
    let (host, port_s) = host_port.split_once(':').expect("http URL");
    let port: u16 = port_s.parse().expect("port");
    let prefix = format!("http://{host}:{port}");
    let path = url.strip_prefix(&prefix).unwrap_or("/");
    let path = path.split('?').next().unwrap_or("/");
    let mut stream = TcpStream::connect((host, port)).expect("connect");
    let mut req = format!(
        "PUT {path} HTTP/1.1\r\nHost: {host}:{port}\r\nContent-Length: {}\r\n",
        body.len()
    );
    if let Some(tag) = if_match {
        req.push_str(&format!("If-Match: {tag}\r\n"));
    }
    req.push_str("Connection: close\r\n\r\n");
    stream.write_all(req.as_bytes()).unwrap();
    stream.write_all(body).unwrap();
    let mut buf = Vec::new();
    let _ = stream.read_to_end(&mut buf);
    let first_line = String::from_utf8_lossy(&buf);
    first_line
        .split_whitespace()
        .nth(1)
        .and_then(|s| s.parse::<u16>().ok())
        .unwrap_or(0)
}

/// 切换某台「设备」的家目录（core 按调用读 `WB_SWITCH_HOME`）。
fn switch_home(home: &std::path::Path) {
    std::env::set_var(wb_switch_core::modules::config::HOME_ENV_VAR, home);
}

fn make_device(name: &str, accounts: &Value) -> std::path::PathBuf {
    let home = std::env::temp_dir().join(format!("wbsync-e2e-{name}-{}", uuid::Uuid::new_v4().simple()));
    let store = home.join(".wb-switch");
    std::fs::create_dir_all(&store).unwrap();
    std::fs::write(store.join("accounts.json"), accounts.to_string()).unwrap();
    home
}

#[tokio::test(flavor = "current_thread")]
async fn webdav_backup_sync_restore_across_devices() {
    use wb_switch_core::modules::webdav_sync;

    let (base_url, stop_server) = start_server();

    let home1 = make_device("dev1", &json!([
        { "id": "a1", "uid": "u1", "nickname": "甲", "access_token": "tok-1" },
        { "id": "a2", "uid": "u2", "nickname": "乙", "access_token": "tok-2" },
    ]));
    let home2 = make_device(
        "dev2",
        &json!([{ "id": "b3", "uid": "u3", "nickname": "丙", "access_token": "tok-3" }]),
    );

    for home in [&home1, &home2] {
        switch_home(home);
        webdav_sync::save_webdav_config(&json!({
            "url": base_url,
            "remotePath": "wb-switch",
        }))
        .unwrap();
    }

    eprintln!("[e2e] stage: device1 setup");
    // ---- 设备 1：首次接入 ----
    switch_home(&home1);
    let r = webdav_sync::test_connection().await.unwrap();
    assert_eq!(r["ok"], true, "测试连接应成功（MKCOL 建目录）：{r}");

    // 显式改一份签到配置（开），随后备份里应带上它，作为跨设备恢复的断言锚点。
    wb_switch_core::modules::config::save_checkin_config(&json!({ "enabled": true })).unwrap();

    let info = webdav_sync::remote_info().await.unwrap();
    assert_eq!(info["exists"], false, "未备份前云端应无文件：{info}");

    let r = webdav_sync::backup_now("passphrase-1").await.unwrap();
    assert_eq!(r["count"], 2, "整库两个账号应全部入备份：{r}");

    // 云端元信息无密码可读。
    let info = webdav_sync::remote_info().await.unwrap();
    assert_eq!(info["exists"], true);
    assert_eq!(info["accountCount"], 2, "明文头应报告账号数：{info}");

    // 预览脱敏：有 hasConfigs、不含 token。
    let prev = webdav_sync::restore_preview("passphrase-1").await.unwrap();
    assert_eq!(prev["total"], 2);
    assert_eq!(prev["hasConfigs"], true, "备份应携带配置快照：{prev}");
    assert!(
        !prev.to_string().contains("tok-1"),
        "预览不得泄露 token：{prev}"
    );
    let wrong = webdav_sync::restore_preview("wrong-pass").await;
    assert!(wrong.is_err(), "错密码应报错");
    assert!(wrong.unwrap_err().contains("加密密码不正确"));

    eprintln!("[e2e] stage: device2 sync");
    // ---- 设备 2：同步（拉设备 1 的备份合并，再推整体）----
    switch_home(&home2);
    let r = webdav_sync::sync_now("passphrase-1").await.unwrap();
    assert_eq!(r["count"], 3, "设备 2 同步后本地应为 3 个账号：{r}");
    let acc2: Value = serde_json::from_str(
        &std::fs::read_to_string(home2.join(".wb-switch/accounts.json")).unwrap(),
    )
    .unwrap();
    let uids: Vec<&str> = acc2
        .as_array()
        .unwrap()
        .iter()
        .map(|a| a["uid"].as_str().unwrap())
        .collect();
    assert!(
        uids.contains(&"u1") && uids.contains(&"u2") && uids.contains(&"u3"),
        "设备 2 应合并到 3 账号：{uids:?}"
    );

    // 云端 WebDAV 配置快照已随备份写回（含服务器地址，新设备免重填）。
    let webdav_cfg = webdav_sync::load_webdav_config();
    assert_eq!(webdav_cfg["url"], base_url, "同步写回的是设备 1 的 WebDAV 配置");

    eprintln!("[e2e] stage: device1 re-sync");
    // ---- 设备 1：再次同步 → 拿到设备 2 的账号 ----
    switch_home(&home1);
    let r = webdav_sync::sync_now("passphrase-1").await.unwrap();
    assert_eq!(r["count"], 3, "设备 1 同步后应也升到 3 账号：{r}");

    eprintln!("[e2e] stage: device3 restore");
    // ---- 全新设备：用户填入 WebDAV 服务器地址 + 加密密码，走「恢复」勾选全部 + 写回配置 ----
    let home3 = make_device("dev3", &json!([]));
    switch_home(&home3);
    // 新设备先手工配置服务器（与真实操作一致），但账号库仍为空。
    webdav_sync::save_webdav_config(&json!({ "url": base_url, "remotePath": "wb-switch" })).unwrap();
    let r = webdav_sync::restore("passphrase-1", &[0, 1, 2], true).await.unwrap();
    assert_eq!(r["imported"], 3, "新设备恢复应全部导入：{r}");
    assert_eq!(r["restoredConfigs"], true, "勾选后应写回配置");
    // 功能配置也随备份写回：设备 1 的签到配置应出现在设备 3 上。
    assert_eq!(
        wb_switch_core::modules::config::load_checkin_config()["enabled"],
        true,
        "新设备恢复后应拿到设备 1 的签到配置"
    );

    eprintln!("[e2e] stage: conflict check");
    // 冲突路径：过期 ETag 的 PUT 必须被 412 拒绝（与服务端 If-Match 同源语义）。
    let stale = raw_put(&format!("{base_url}/wb-switch/wb-switch-backup.wbs"), b"x", Some("\"v0-nonexistent\""));
    assert_eq!(stale, 412, "过期 ETag 必须被拒");

    eprintln!("[e2e] stage: done");
    std::env::remove_var(wb_switch_core::modules::config::HOME_ENV_VAR);
    for home in [&home1, &home2, &home3] {
        let _ = std::fs::remove_dir_all(home);
    }
    // 让 accept 循环退出，避免非 daemon 线程挂住测试进程退出。
    stop_server.store(true, std::sync::atomic::Ordering::SeqCst);
    std::thread::sleep(std::time::Duration::from_millis(120));
}
