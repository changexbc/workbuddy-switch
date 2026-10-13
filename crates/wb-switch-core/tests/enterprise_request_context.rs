//! Actual HTTP headers before/after credential reload and a 401 retry. Every
//! case runs in a child with a fresh home and a loopback-only server; no real
//! accounts or official endpoints are used, including token refresh.
use serde_json::{json, Value};
use std::collections::BTreeMap;
use std::io::{Read, Write};
use std::net::{TcpListener, TcpStream};
use std::process::Command;
use std::time::{Duration, Instant};
use wb_switch_core::modules::{config, credits};

fn headers(stream: &mut TcpStream) -> BTreeMap<String, String> {
    stream
        .set_read_timeout(Some(Duration::from_secs(10)))
        .unwrap();
    let mut bytes = Vec::new();
    let end = loop {
        let mut byte = [0];
        assert_eq!(stream.read(&mut byte).unwrap(), 1);
        bytes.push(byte[0]);
        if bytes.ends_with(b"\r\n\r\n") {
            break bytes.len();
        }
        assert!(bytes.len() < 65536);
    };
    let text = std::str::from_utf8(&bytes[..end]).unwrap();
    let values: BTreeMap<_, _> = text
        .lines()
        .skip(1)
        .filter_map(|line| {
            let (name, value) = line.split_once(':')?;
            Some((name.to_ascii_lowercase(), value.trim().to_string()))
        })
        .collect();
    let size = values
        .get("content-length")
        .unwrap()
        .parse::<usize>()
        .unwrap();
    stream.read_exact(&mut vec![0; size]).unwrap();
    values
}

#[test]
fn worker() {
    let Ok(case) = std::env::var("WB_ENTERPRISE_CONTEXT_CASE") else {
        return;
    };
    let root = config::home_dir();
    let store = root.join(".wb-switch/accounts.json");
    std::fs::create_dir_all(store.parent().unwrap()).unwrap();
    let now = config::now_ms();
    let mut stored = json!({"id":"account","uid":"uid","access_token":"latest-token",
        "refresh_token":"latest-refresh","refreshedAt":now,"expiresAt":now+7*86400000,"note":"latest-note"});
    if case == "stored-tenant" {
        stored["enterprise_id"] = json!("stored-tenant");
    }
    std::fs::write(&store, json!([stored.clone()]).to_string()).unwrap();
    let request_context = json!({"id":"account","uid":"uid","access_token":"stale-token",
        "refresh_token":"stale-refresh","enterpriseId":"resolved-tenant","note":"old-note"});
    let listener = TcpListener::bind("127.0.0.1:0").unwrap();
    let url = format!(
        "http://{}/enterprise-fixture",
        listener.local_addr().unwrap()
    );
    listener.set_nonblocking(true).unwrap();
    let retry = case == "retry";
    let server_store = store.clone();
    let server_stored = stored.clone();
    let server = std::thread::spawn(move || {
        let mut requests = Vec::new();
        for index in 0..if retry { 2 } else { 1 } {
            let started = Instant::now();
            let (mut stream, _) = loop {
                match listener.accept() {
                    Ok(connection) => break connection,
                    Err(error) if error.kind() == std::io::ErrorKind::WouldBlock => {
                        assert!(
                            started.elapsed() < Duration::from_secs(10),
                            "No loopback request"
                        );
                        std::thread::sleep(Duration::from_millis(10));
                    }
                    Err(error) => panic!("{error}"),
                }
            };
            stream.set_nonblocking(false).unwrap();
            requests.push(headers(&mut stream));
            let unauthorized = retry && index == 0;
            if unauthorized {
                // Another process has already rotated credentials. Refresh must
                // reload them without calling an official token endpoint.
                let mut newer = server_stored.clone();
                newer["access_token"] = json!("rotated-token");
                newer["refresh_token"] = json!("rotated-refresh");
                newer["note"] = json!("concurrently-edited-note");
                std::fs::write(&server_store, json!([newer]).to_string()).unwrap();
            }
            let status = if unauthorized {
                "401 Unauthorized"
            } else {
                "200 OK"
            };
            let body = if unauthorized {
                json!({"code":401,"message":"expired"})
            } else {
                json!({"code":0,"data":{"fixture":true}})
            }
            .to_string();
            write!(stream, "HTTP/1.1 {status}\r\nContent-Type: application/json\r\nContent-Length: {}\r\nConnection: close\r\n\r\n{body}", body.len()).unwrap();
        }
        requests
    });
    let runtime = tokio::runtime::Builder::new_current_thread()
        .enable_all()
        .build()
        .unwrap();
    let result = runtime.block_on(credits::authenticated_post(
        &request_context,
        &url,
        json!({}),
    ));
    assert_eq!(result["code"], 0);
    let requests = server.join().unwrap();
    let expected_tenant = if case == "stored-tenant" {
        "stored-tenant"
    } else {
        "resolved-tenant"
    };
    for request in &requests {
        assert_eq!(
            request.get("x-enterprise-id").map(String::as_str),
            Some(expected_tenant)
        );
        assert_eq!(
            request.get("x-tenant-id").map(String::as_str),
            Some(expected_tenant)
        );
    }
    assert_eq!(
        requests[0].get("authorization").map(String::as_str),
        Some("Bearer latest-token")
    );
    if retry {
        assert_eq!(
            requests[1].get("authorization").map(String::as_str),
            Some("Bearer rotated-token")
        );
    }
    let persisted: Value = serde_json::from_slice(&std::fs::read(&store).unwrap()).unwrap();
    if case != "stored-tenant" {
        assert!(persisted[0].get("enterpriseId").is_none());
        assert!(persisted[0].get("enterprise_id").is_none());
    }
    assert_eq!(
        persisted[0]["note"],
        if retry {
            "concurrently-edited-note"
        } else {
            "latest-note"
        }
    );
}

fn run_case(case: &str) {
    let root = std::env::temp_dir().join(format!("wb-enterprise-context-{}", uuid::Uuid::new_v4()));
    std::fs::create_dir_all(&root).unwrap();
    let output = Command::new(std::env::current_exe().unwrap())
        .args(["--exact", "worker", "--nocapture"])
        .env("WB_SWITCH_HOME", &root)
        .env("WB_ENTERPRISE_CONTEXT_CASE", case)
        .env("NO_PROXY", "*")
        .env("no_proxy", "*")
        .env_remove("HTTP_PROXY")
        .env_remove("HTTPS_PROXY")
        .env_remove("ALL_PROXY")
        .env_remove("http_proxy")
        .env_remove("https_proxy")
        .env_remove("all_proxy")
        .env_remove("CODEBUDDY_AUTH_TOKEN")
        .output()
        .unwrap();
    // Only this known, freshly generated test directory is removed.
    std::fs::remove_dir_all(root).unwrap();
    assert!(
        output.status.success(),
        "case {case}: {} {}",
        String::from_utf8_lossy(&output.stdout),
        String::from_utf8_lossy(&output.stderr)
    );
}

#[test]
fn resolved_tenant_survives_latest_credential_reload() {
    run_case("reload");
}

#[test]
fn resolved_tenant_survives_a_401_retry_without_stale_token_or_metadata_write() {
    run_case("retry");
}

#[test]
fn latest_stored_tenant_wins_over_request_context() {
    run_case("stored-tenant");
}
