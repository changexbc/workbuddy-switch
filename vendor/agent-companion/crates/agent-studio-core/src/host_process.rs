// Hook-driven sources never report their own exit. The poll loop therefore
// probes the macOS host app directly; a missing main process is the only
// reliable signal that a force-quit happened without a SessionEnd hook.
use crate::{hub::Hub, now, text};
use serde_json::json;
use std::time::{Duration, Instant};

// Helper processes live under Contents/Frameworks, and a crashpad handler can
// outlive the app (ppid 1). Requiring the bundle-plus-Contents/MacOS shape
// keeps the probe on the main executable only.
pub fn host_bundles(kind: &str) -> &'static [&'static str] {
    match kind {
        "workbuddy" => &["WorkBuddy.app", "WorkBuddy AI.app"],
        "codebuddy-ide" => &["CodeBuddy.app", "CodeBuddy CN.app"],
        // The VS Code family shares the plugin hook payload client=vscode; any
        // one live member keeps that host kind alive.
        "vscode" => &[
            "Visual Studio Code.app",
            "Code - Insiders.app",
            "VSCodium.app",
            "Cursor.app",
            "Windsurf.app",
        ],
        _ => &[],
    }
}

pub fn match_host(stdout: &str, bundles: &[&str]) -> bool {
    stdout.lines().any(|line| {
        bundles
            .iter()
            .any(|b| line.contains(&format!("/{b}/Contents/MacOS/")))
    })
}

pub fn default_ps() -> Result<String, String> {
    let output = std::process::Command::new("ps")
        .args(["-axo", "comm="])
        .output()
        .map_err(|e| e.to_string())?;
    if !output.status.success() {
        return Err("ps 退出异常".into());
    }
    Ok(String::from_utf8_lossy(&output.stdout).into_owned())
}

#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum Presence {
    Alive,
    Gone,
    Unknown,
}

pub struct HostPresence {
    kind: String,
    seen_alive: bool,
    misses: u32,
    cached: Presence,
    last_check: Option<Instant>,
    ttl: Duration,
    min_misses: u32,
    supported: bool,
    runner: Box<dyn Fn() -> Result<String, String> + Send>,
}

impl HostPresence {
    pub fn for_host(kind: &str) -> Self {
        Self::with_runner(
            kind,
            Duration::from_secs(5),
            2,
            Box::new(default_ps),
        )
    }
    pub fn with_runner(
        kind: &str,
        ttl: Duration,
        min_misses: u32,
        runner: Box<dyn Fn() -> Result<String, String> + Send>,
    ) -> Self {
        Self {
            kind: kind.into(),
            seen_alive: false,
            misses: 0,
            cached: Presence::Unknown,
            last_check: None,
            ttl,
            min_misses,
            supported: cfg!(any(target_os = "macos", target_os = "linux")),
            runner,
        }
    }
    // A hook proves the host was alive; it also invalidates a cached "gone" so
    // the next poll reports ok immediately instead of one beat later.
    pub fn note_hook(&mut self) {
        self.seen_alive = true;
        self.misses = 0;
        if self.cached == Presence::Gone {
            self.last_check = None;
        }
    }
    pub fn observe(&mut self) -> Presence {
        if !self.supported {
            return Presence::Unknown;
        }
        if let Some(last) = self.last_check {
            if last.elapsed() < self.ttl {
                return self.cached;
            }
        }
        self.last_check = Some(Instant::now());
        let output = match (self.runner)() {
            Ok(output) => output,
            Err(_) => {
                self.cached = Presence::Unknown;
                return Presence::Unknown;
            }
        };
        if match_host(&output, host_bundles(&self.kind)) {
            self.seen_alive = true;
            self.misses = 0;
            self.cached = Presence::Alive;
            return Presence::Alive;
        }
        // Never claim an exit before the host was ever observed; a user who has
        // not launched the app should not see the source marked as exited.
        if !self.seen_alive {
            self.cached = Presence::Unknown;
            return Presence::Unknown;
        }
        self.misses += 1;
        if self.misses >= self.min_misses {
            self.cached = Presence::Gone;
            return Presence::Gone;
        }
        self.cached = Presence::Alive;
        Presence::Alive
    }
}

// Ending sessions keeps the record honest: the round did not finish, the app
// went away, and the front-end shows "已退出" for a short grace then drops it.
// Hook timestamps may run ahead of the collector clock, so the synthetic end
// must never look older than the round it closes.
pub fn end_host_sessions(hub: &mut Hub, source: &str, host_kind: Option<&str>) -> usize {
    let targets: Vec<(String, String, i64)> = hub
        .sessions
        .values()
        .filter(|s| {
            text(&s["source"]) == source
                && !crate::hub::terminal(&text(&s["status"]))
                && host_kind.map_or(true, |kind| text(&s["hostKind"]) == kind)
        })
        .map(|s| {
            (
                text(&s["sessionId"]),
                text(&s["roundId"]),
                s["updatedAt"].as_i64().unwrap_or(0),
            )
        })
        .collect();
    for (session, round, updated) in &targets {
        hub.ingest(json!({
            "source": source,
            "sessionId": session,
            "roundId": round,
            "type": "end",
            "status": "aborted",
            "endedBy": "host",
            "ts": now().max(updated + 1)
        }));
    }
    targets.len()
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::sync::atomic::{AtomicUsize, Ordering};
    use std::sync::Arc;
    use serde_json::json;

    const PS_ALIVE: &str =
        "/sbin/launchd\n/Applications/WorkBuddy.app/Contents/MacOS/Electron\n/Applications/WorkBuddy.app/Contents/Frameworks/WorkBuddy Helper.app/Contents/MacOS/WorkBuddy Helper\n";

    #[test]
    fn matches_main_bundle_only() {
        assert!(match_host(PS_ALIVE, host_bundles("workbuddy")));
        assert!(match_host(
            "/Applications/WorkBuddy AI.app/Contents/MacOS/Electron\n",
            host_bundles("workbuddy")
        ));
        assert!(match_host(
            "/Applications/CodeBuddy CN.app/Contents/MacOS/Electron\n",
            host_bundles("codebuddy-ide")
        ));
        // A helper or a crashpad handler must not count as the host.
        assert!(!match_host(
            "/Applications/WorkBuddy.app/Contents/Frameworks/WorkBuddy Helper.app/Contents/MacOS/WorkBuddy Helper\n",
            host_bundles("workbuddy")
        ));
        assert!(!match_host(
            "/Applications/WorkBuddy.app/Contents/Frameworks/Electron Framework.framework/Helpers/chrome_crashpad_handler\n",
            host_bundles("workbuddy")
        ));
    }

    #[test]
    fn vscode_family_bundles_match_main_executables_only() {
        let bundles = host_bundles("vscode");
        for line in [
            "/Applications/Visual Studio Code.app/Contents/MacOS/Electron",
            "/Applications/Code - Insiders.app/Contents/MacOS/Electron",
            "/Applications/VSCodium.app/Contents/MacOS/Electron",
            "/Applications/Cursor.app/Contents/MacOS/Cursor",
            "/Applications/Windsurf.app/Contents/MacOS/Electron",
        ] {
            assert!(match_host(&format!("{line}\n"), bundles), "{line}");
        }
        assert!(!match_host(
            "/Applications/Visual Studio Code.app/Contents/Frameworks/Code Helper (Renderer).app/Contents/MacOS/Code Helper (Renderer)\n",
            bundles
        ));
        assert!(!match_host(
            "/Applications/Visual Studio Code.app/Contents/Frameworks/Electron Framework.framework/Helpers/chrome_crashpad_handler\n",
            bundles
        ));
        // Host kinds never leak into each other.
        assert!(!match_host(
            "/Applications/Visual Studio Code.app/Contents/MacOS/Electron\n",
            host_bundles("codebuddy-ide")
        ));
        assert!(!match_host(
            "/Applications/CodeBuddy.app/Contents/MacOS/CodeBuddy\n",
            bundles
        ));
    }

    // 这批用例验证的是探测行为，而探测本身只在 macOS/Linux 上启用，故仅在有探测能力的平台上运行。
    #[cfg(any(target_os = "macos", target_os = "linux"))]
    #[test]
    fn host_presences_are_independent_per_kind() {
        let mut ide = HostPresence::with_runner(
            "codebuddy-ide",
            Duration::ZERO,
            2,
            Box::new(|| Ok("/sbin/launchd\n".into())),
        );
        let mut vscode = HostPresence::with_runner(
            "vscode",
            Duration::ZERO,
            2,
            Box::new(|| Ok("/Applications/Visual Studio Code.app/Contents/MacOS/Electron\n".into())),
        );
        ide.note_hook();
        vscode.note_hook();
        assert_eq!(ide.observe(), Presence::Alive);
        assert_eq!(vscode.observe(), Presence::Alive);
        assert_eq!(ide.observe(), Presence::Gone);
        // One kind going away must not change the other's verdict.
        assert_eq!(vscode.observe(), Presence::Alive);
    }

    #[cfg(any(target_os = "macos", target_os = "linux"))]
    #[test]
    fn requires_two_misses_and_never_infers_without_a_sighting() {
        let mut quiet = HostPresence::with_runner(
            "workbuddy",
            Duration::ZERO,
            2,
            Box::new(|| Ok("/sbin/launchd\n".into())),
        );
        assert_eq!(quiet.observe(), Presence::Unknown);
        assert_eq!(quiet.observe(), Presence::Unknown);

        let mut gone = HostPresence::with_runner(
            "workbuddy",
            Duration::ZERO,
            2,
            Box::new(|| Ok("/sbin/launchd\n".into())),
        );
        gone.note_hook();
        assert_eq!(gone.observe(), Presence::Alive);
        assert_eq!(gone.observe(), Presence::Gone);
    }

    #[cfg(any(target_os = "macos", target_os = "linux"))]
    #[test]
    fn caches_within_ttl_and_recovers_after_a_hook() {
        let calls = Arc::new(AtomicUsize::new(0));
        let counter = calls.clone();
        let mut presence = HostPresence::with_runner(
            "workbuddy",
            Duration::from_secs(60),
            1,
            Box::new(move || {
                counter.fetch_add(1, Ordering::SeqCst);
                Ok(PS_ALIVE.into())
            }),
        );
        presence.note_hook();
        assert_eq!(presence.observe(), Presence::Alive);
        assert_eq!(presence.observe(), Presence::Alive);
        assert_eq!(calls.load(Ordering::SeqCst), 1);

        let mut errored = HostPresence::with_runner(
            "codebuddy-ide",
            Duration::ZERO,
            1,
            Box::new(|| Err("ps 不可用".into())),
        );
        errored.note_hook();
        assert_eq!(errored.observe(), Presence::Unknown);
    }

    // 不支持的平台必须返回 Unknown 而不是猜——探测不跑，也不能假装看过。
    #[cfg(not(any(target_os = "macos", target_os = "linux")))]
    #[test]
    fn unsupported_platforms_report_unknown_and_never_run_the_probe() {
        let calls = Arc::new(AtomicUsize::new(0));
        let counter = calls.clone();
        let mut presence = HostPresence::with_runner(
            "workbuddy",
            Duration::ZERO,
            1,
            Box::new(move || {
                counter.fetch_add(1, Ordering::SeqCst);
                Ok(PS_ALIVE.into())
            }),
        );
        presence.note_hook();
        assert_eq!(presence.observe(), Presence::Unknown);
        assert_eq!(
            calls.load(Ordering::SeqCst),
            0,
            "unsupported platforms must not shell out"
        );
    }

    #[test]
    fn ends_only_unfinished_sessions_of_that_source() {
        let mut hub = Hub::new();
        hub.ingest(json!({"source":"workbuddy","sessionId":"a","roundId":"r1","type":"start","ts":1}));
        hub.ingest(json!({"source":"workbuddy","sessionId":"b","roundId":"r1","type":"end","status":"done","ts":2}));
        hub.ingest(json!({"source":"codex","sessionId":"c","roundId":"r1","type":"start","ts":3}));
        assert_eq!(end_host_sessions(&mut hub, "workbuddy", None), 1);
        let ended = &hub.sessions["workbuddy:a"];
        assert_eq!(text(&ended["status"]), "aborted");
        assert_eq!(text(&ended["endedBy"]), "host");
        assert_eq!(text(&hub.sessions["workbuddy:b"]["status"]), "done");
        assert_eq!(text(&hub.sessions["codex:c"]["status"]), "running");
    }

    #[test]
    fn ends_only_sessions_of_the_named_host_kind() {
        let mut hub = Hub::new();
        hub.ingest(json!({"source":"codebuddy-ide","hostKind":"codebuddy-ide","sessionId":"ide","roundId":"r1","type":"start","ts":1}));
        hub.ingest(json!({"source":"codebuddy-ide","hostKind":"vscode","sessionId":"code","roundId":"r1","type":"start","ts":2}));
        hub.ingest(json!({"source":"codebuddy-ide","hostKind":"vscode","sessionId":"done","roundId":"r1","type":"end","status":"done","ts":3}));
        assert_eq!(end_host_sessions(&mut hub, "codebuddy-ide", Some("vscode")), 1);
        assert_eq!(text(&hub.sessions["codebuddy-ide:code"]["status"]), "aborted");
        assert_eq!(text(&hub.sessions["codebuddy-ide:ide"]["status"]), "running");
        assert_eq!(text(&hub.sessions["codebuddy-ide:done"]["status"]), "done");
        assert_eq!(end_host_sessions(&mut hub, "codebuddy-ide", Some("codebuddy-ide")), 1);
        assert_eq!(text(&hub.sessions["codebuddy-ide:ide"]["status"]), "aborted");
    }
}
