//! Cooperative interruption: never drop an in-flight authentication transaction.
use super::protocol::Failure;
use serde_json::{json, Value};
use tokio::sync::watch;

#[derive(Clone)]
pub(super) struct Control {
    interrupted: watch::Sender<bool>,
}

impl Control {
    pub(super) fn new() -> Self {
        let (interrupted, _) = watch::channel(false);
        Self { interrupted }
    }

    pub(super) fn request(&self) {
        self.interrupted.send_replace(true);
    }

    pub(super) fn is_interrupted(&self) -> bool {
        *self.interrupted.borrow()
    }

    pub(super) async fn cancelled(&self) {
        let mut receiver = self.interrupted.subscribe();
        if !*receiver.borrow_and_update() {
            let _ = receiver.changed().await;
        }
    }

    pub(super) fn check(&self) -> Result<(), Failure> {
        if self.is_interrupted() {
            Err(interrupted())
        } else {
            Ok(())
        }
    }

    pub(super) fn finish(&self, mut value: Value, exit: u8) -> (Value, u8) {
        if self.is_interrupted() {
            value["operationOk"] = value["ok"].clone();
            if !value["error"].is_null() {
                value["operationError"] = value["error"].clone();
            }
            value["ok"] = json!(false);
            value["interrupted"] = json!(true);
            value["error"] = super::protocol::error("INTERRUPTED", &interrupted().message);
            (value, 130)
        } else {
            (value, exit)
        }
    }
}

pub(super) fn interrupted() -> Failure {
    Failure {
        code: "INTERRUPTED",
        message: "Command interrupted; in-flight work finished safely. Completed results and side effects are retained.".into(),
        exit: 130,
    }
}

pub(super) struct Signals {
    #[cfg(unix)]
    interrupt: tokio::signal::unix::Signal,
    #[cfg(unix)]
    terminate: tokio::signal::unix::Signal,
    #[cfg(windows)]
    interrupt: tokio::signal::windows::CtrlC,
    #[cfg(windows)]
    terminate: tokio::signal::windows::CtrlBreak,
}

impl Signals {
    /// Register before starting any work, rather than lazily on the first await.
    pub(super) fn new() -> Result<Self, Failure> {
        #[cfg(unix)]
        let result = (|| {
            use tokio::signal::unix::{signal, SignalKind};
            Ok::<_, std::io::Error>(Self {
                interrupt: signal(SignalKind::interrupt())?,
                terminate: signal(SignalKind::terminate())?,
            })
        })();
        #[cfg(windows)]
        let result = (|| {
            Ok::<_, std::io::Error>(Self {
                interrupt: tokio::signal::windows::ctrl_c()?,
                terminate: tokio::signal::windows::ctrl_break()?,
            })
        })();
        result.map_err(|error| Failure::operation("SIGNAL_SETUP_FAILED", error.to_string()))
    }

    pub(super) async fn recv(&mut self) {
        tokio::select! {
            _ = self.interrupt.recv() => {},
            _ = self.terminate.recv() => {},
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[tokio::test]
    async fn cancellation_notification_cannot_be_lost_before_waiting() {
        let control = Control::new();
        let waiter = control.cancelled();
        control.request();
        tokio::time::timeout(std::time::Duration::from_secs(1), waiter)
            .await
            .unwrap();
        assert_eq!(control.check().err().unwrap().exit, 130);
    }

    #[test]
    fn interrupted_result_preserves_committed_side_effects_and_original_error() {
        let control = Control::new();
        control.request();
        let (value, exit) = control.finish(
            json!({"ok":true,"error":null,"result":{"activeAccountId":"committed","closedProcessCount":2}}),
            0,
        );
        assert_eq!(exit, 130);
        assert_eq!(value["operationOk"], true);
        assert_eq!(value["ok"], false);
        assert_eq!(value["result"]["activeAccountId"], "committed");
        assert_eq!(value["error"]["code"], "INTERRUPTED");
        let (value, _) = control.finish(json!({"ok":false,"error":{"code":"PREVIOUS"}}), 1);
        assert_eq!(value["operationError"]["code"], "PREVIOUS");
    }
}
