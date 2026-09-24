//! Read-only Codeg stream transport. Timers reconnect failed sockets, never scan sessions.
use base64::{engine::general_purpose::URL_SAFE_NO_PAD, Engine};
use serde_json::{json, Value};
use std::{
    net::{Shutdown, TcpStream},
    sync::{
        atomic::{AtomicBool, AtomicU64, Ordering},
        Arc,
    },
    time::Duration,
};
use tungstenite::{client::IntoClientRequest, Message};
pub type Sink = Arc<dyn Fn(Value) + Send + Sync>;
pub struct Stream {
    pub sid: String,
    pub subscription: String,
    pub seq: Arc<AtomicU64>,
    stop: Arc<AtomicBool>,
    socket: Arc<std::sync::Mutex<Option<TcpStream>>>,
}
impl Drop for Stream {
    fn drop(&mut self) {
        self.stop.store(true, Ordering::Release);
        if let Ok(socket) = self.socket.lock() {
            if let Some(socket) = socket.as_ref() {
                let _ = socket.shutdown(Shutdown::Both);
            }
        }
    }
}
impl Stream {
    pub fn new(
        conn: String,
        sid: String,
        subscription: String,
        auth: (u16, String),
        sink: Sink,
    ) -> Self {
        let seq = Arc::new(AtomicU64::new(u64::MAX));
        let stop = Arc::new(AtomicBool::new(false));
        let socket = Arc::new(std::sync::Mutex::new(None));
        let result = Self {
            sid,
            subscription: subscription.clone(),
            seq: seq.clone(),
            stop: stop.clone(),
            socket: socket.clone(),
        };
        std::thread::spawn(move || {
            let mut attempt = 0;
            while !stop.load(Ordering::Acquire) {
                let run = || -> Result<(), Box<dyn std::error::Error>> {
                    let tcp = TcpStream::connect_timeout(
                        &format!("127.0.0.1:{}", auth.0).parse()?,
                        Duration::from_millis(1500),
                    )?;
                    tcp.set_read_timeout(Some(Duration::from_millis(1500)))?;
                    tcp.set_write_timeout(Some(Duration::from_millis(1500)))?;
                    *socket.lock().unwrap() = Some(tcp.try_clone()?);
                    let mut request =
                        format!("ws://127.0.0.1:{}/ws/events", auth.0).into_client_request()?;
                    request.headers_mut().insert(
                        "Sec-WebSocket-Protocol",
                        format!(
                            "codeg-events, codeg-token.{}",
                            URL_SAFE_NO_PAD.encode(auth.1.trim())
                        )
                        .parse()?,
                    );
                    let (mut ws, _) = tungstenite::client(request, tcp)?;
                    let last = seq.load(Ordering::Acquire);
                    ws.send(Message::Text(json!({"action":"attach","subscription_id":subscription,"connection_id":conn,"since_seq":if last==u64::MAX {Value::Null}else{json!(last)}}).to_string().into()))?;
                    while !stop.load(Ordering::Acquire) {
                        match ws.read() {
                            Ok(Message::Text(text)) => {
                                if let Ok(mut frame) = serde_json::from_str::<Value>(&text) {
                                    if frame["subscription_id"] != subscription
                                        || (!frame["connection_id"].is_null()
                                            && frame["connection_id"] != conn)
                                    {
                                        continue;
                                    }
                                    if frame["type"] == "detached"
                                        && matches!(
                                            frame["reason"].as_str(),
                                            Some("lagged" | "server_shutdown")
                                        )
                                    {
                                        break;
                                    }
                                    frame["connection_id"] = json!(conn);
                                    sink(frame);
                                }
                            }
                            Ok(Message::Close(_)) => break,
                            Ok(_) => {}
                            Err(tungstenite::Error::Io(e))
                                if matches!(
                                    e.kind(),
                                    std::io::ErrorKind::WouldBlock | std::io::ErrorKind::TimedOut
                                ) => {}
                            Err(e) => return Err(e.into()),
                        }
                    }
                    Ok(())
                };
                let _ = run(); // Never expose URLs or authentication headers in errors.
                *socket.lock().unwrap() = None;
                let delay = std::cmp::min(300, 5 * (1u64 << std::cmp::min(attempt, 6)));
                attempt += 1;
                for _ in 0..delay {
                    if stop.load(Ordering::Acquire) {
                        return;
                    }
                    std::thread::sleep(Duration::from_millis(100));
                }
            }
        });
        result
    }
}
