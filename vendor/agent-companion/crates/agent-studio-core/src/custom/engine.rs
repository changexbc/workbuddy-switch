//! Pure mapping engine: template + raw payload -> standard hub events.
//!
//! The engine owns the lifecycle rules (rounds, waits, dedup) and never touches
//! the hub, the filesystem or the network, so the preview path can reuse it with
//! no side effects.

use super::limits::*;
use super::pointer;
use super::template::{Action, Template, WaitReason};
use serde_json::{json, Value};
use std::collections::{HashMap, HashSet, VecDeque};

/// The single session-level wait slot used when the template does not map a request id.
pub const WAIT_CALL_ID: &str = "custom-wait";
/// A session that keeps sending unmatched events cannot grow without bound.
const PENDING_MAX: usize = 16;

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum OutcomeKind {
    Accepted,
    Ignored,
    Rejected,
}

impl OutcomeKind {
    pub fn label(self) -> &'static str {
        match self {
            Self::Accepted => "accepted",
            Self::Ignored => "ignored",
            Self::Rejected => "rejected",
        }
    }
}

#[derive(Clone, Debug)]
pub struct Outcome {
    pub kind: OutcomeKind,
    /// Action label for accepted events (`start`, `wait`, ...).
    pub action: Option<&'static str>,
    /// Machine-readable reason code; for accepted events the action label.
    pub reason: String,
    pub detail: String,
    pub path: String,
    pub raw_event: Option<String>,
    pub session_id: Option<String>,
    pub round_id: Option<String>,
    pub events: Vec<Value>,
}

impl Outcome {
    fn accepted(action: Action, raw_event: &str, session_id: &str, round_id: &str, events: Vec<Value>) -> Self {
        Self {
            kind: OutcomeKind::Accepted,
            action: Some(action.label()),
            reason: action.label().into(),
            detail: String::new(),
            path: String::new(),
            raw_event: Some(raw_event.into()),
            session_id: Some(session_id.into()),
            round_id: Some(round_id.into()),
            events,
        }
    }
    fn ignored(
        action: Action,
        reason: &'static str,
        detail: impl Into<String>,
        raw_event: &str,
        session_id: &str,
        round_id: Option<String>,
    ) -> Self {
        Self {
            kind: OutcomeKind::Ignored,
            action: Some(action.label()),
            reason: reason.into(),
            detail: detail.into(),
            path: String::new(),
            raw_event: Some(raw_event.into()),
            session_id: Some(session_id.into()),
            round_id,
            events: Vec::new(),
        }
    }
    fn rejected(reason: &'static str, path: &str, detail: impl Into<String>) -> Self {
        Self {
            kind: OutcomeKind::Rejected,
            action: None,
            reason: reason.into(),
            detail: detail.into(),
            path: path.into(),
            raw_event: None,
            session_id: None,
            round_id: None,
            events: Vec::new(),
        }
    }
    pub fn with_event(mut self, raw_event: Option<&str>, session_id: Option<&str>) -> Self {
        if let Some(raw_event) = raw_event {
            self.raw_event = Some(raw_event.into());
        }
        if let Some(session_id) = session_id {
            self.session_id = Some(session_id.into());
        }
        self
    }
    pub fn is_accepted(&self) -> bool {
        self.kind == OutcomeKind::Accepted
    }
    pub fn to_value(&self) -> Value {
        json!({
            "outcome": self.kind.label(),
            "action": self.action,
            "reason": self.reason,
            "detail": self.detail.chars().take(DETAIL_MAX).collect::<String>(),
            "path": self.path,
            "event": self.raw_event,
            "sessionId": self.session_id,
            "roundId": self.round_id,
            "events": self.events,
        })
    }
}

#[derive(Clone, Debug)]
struct SessionState {
    round: String,
    active: bool,
    seen: VecDeque<String>,
    seen_set: HashSet<String>,
    pending: VecDeque<String>,
}

impl SessionState {
    fn new(round: String) -> Self {
        Self {
            round,
            active: true,
            seen: VecDeque::new(),
            seen_set: HashSet::new(),
            pending: VecDeque::new(),
        }
    }
    fn key(round: &str, event_id: &str) -> String {
        format!("{round}|{event_id}")
    }
    fn seen(&self, round: &str, event_id: &str) -> bool {
        self.seen_set.contains(&Self::key(round, event_id))
    }
    fn remember(&mut self, round: &str, event_id: &str) {
        let key = Self::key(round, event_id);
        if self.seen_set.insert(key.clone()) {
            self.seen.push_back(key);
        }
        while self.seen.len() > DEDUP_PER_SESSION {
            if let Some(old) = self.seen.pop_front() {
                self.seen_set.remove(&old);
            }
        }
    }
    fn push_pending(&mut self, call_id: &str) {
        if self.pending.iter().any(|id| id == call_id) {
            return;
        }
        self.pending.push_back(call_id.to_owned());
        while self.pending.len() > PENDING_MAX {
            self.pending.pop_front();
        }
    }
    fn take_pending(&mut self, call_id: &str) -> bool {
        let found = self.pending.iter().position(|id| id == call_id);
        match found {
            Some(index) => {
                self.pending.remove(index);
                true
            }
            None => false,
        }
    }
}

#[derive(Default)]
pub struct Engine {
    sessions: HashMap<String, SessionState>,
    order: VecDeque<String>,
}

impl Engine {
    pub fn new() -> Self {
        Self::default()
    }

    pub fn session_count(&self) -> usize {
        self.sessions.len()
    }

    /// Visible state for tests and diagnostics; `active` is true while a round is
    /// still open, `pending` counts unresolved waits.
    pub fn session_state(&self, source: &str, session_id: &str) -> Option<(String, bool, usize)> {
        self.sessions
            .get(&format!("{source}:{session_id}"))
            .map(|state| (state.round.clone(), state.active, state.pending.len()))
    }

    fn store(&mut self, key: String, state: SessionState) {
        if !self.sessions.contains_key(&key) {
            self.order.push_back(key.clone());
        }
        self.sessions.insert(key, state);
        while self.sessions.len() > SESSIONS_MAX {
            let Some(old) = self.order.pop_front() else { break };
            self.sessions.remove(&old);
        }
    }

    /// Removes one session; returns true when it existed.
    pub fn forget(&mut self, key: &str) -> bool {
        self.order.retain(|entry| entry != key);
        self.sessions.remove(key).is_some()
    }

    pub fn forget_source(&mut self, source: &str) -> usize {
        let prefix = format!("{source}:");
        let keys: Vec<String> = self
            .sessions
            .keys()
            .filter(|key| key.starts_with(&prefix))
            .cloned()
            .collect();
        let count = keys.len();
        for key in keys {
            self.forget(&key);
        }
        count
    }

    /// Maps one raw payload. `now` is the receive time, used when the template
    /// does not map a timestamp.
    pub fn apply(&mut self, template: &Template, raw: &Value, now: i64) -> Outcome {
        if !raw.is_object() {
            return Outcome::rejected("invalid_payload", "", "载荷必须是 JSON 对象");
        }
        let raw_event = match required_event(raw, &template.mapping.event) {
            Ok(value) => value,
            Err(outcome) => return outcome,
        };
        let Some(action) = template.action(&raw_event) else {
            return Outcome::ignored(
                Action::Start,
                "unknown_event",
                format!("未映射的事件 {raw_event}"),
                &raw_event,
                "",
                None,
            );
        };
        if template
            .ignore_if_present
            .iter()
            .any(|pointer| present_value(raw, pointer))
        {
            return Outcome::ignored(
                action,
                "ignored_field_present",
                "命中 ignoreIfPresent，已忽略",
                &raw_event,
                "",
                None,
            );
        }
        let session_id = match id_field(
            raw,
            Some(&template.mapping.session_id),
            SESSION_ID_MAX,
            "sessionId",
            "载荷中缺少会话 ID",
        ) {
            Ok(Some(value)) => value,
            Ok(None) => {
                return Outcome::rejected("missing_session_id", "/mapping/sessionId", "载荷中缺少会话 ID")
            }
            Err(outcome) => return outcome.with_event(Some(&raw_event), None),
        };
        let round_id = match id_field(raw, template.mapping.round_id.as_ref(), ID_FIELD_MAX, "roundId", "字段类型无效") {
            Ok(value) => value,
            Err(outcome) => return outcome.with_event(Some(&raw_event), Some(&session_id)),
        };
        let event_id = match id_field(raw, template.mapping.event_id.as_ref(), EVENT_ID_MAX, "eventId", "字段类型无效") {
            Ok(value) => value,
            Err(outcome) => return outcome.with_event(Some(&raw_event), Some(&session_id)),
        };
        let request_id = match id_field(raw, template.mapping.request_id.as_ref(), ID_FIELD_MAX, "requestId", "字段类型无效") {
            Ok(value) => value,
            Err(outcome) => return outcome.with_event(Some(&raw_event), Some(&session_id)),
        };
        let cwd = match text_field(raw, template.mapping.cwd.as_ref(), CWD_MAX, "cwd") {
            Ok(value) => value,
            Err(outcome) => return outcome.with_event(Some(&raw_event), Some(&session_id)),
        };
        let title = match text_field(raw, template.mapping.title.as_ref(), TITLE_MAX, "title") {
            Ok(value) => value,
            Err(outcome) => return outcome.with_event(Some(&raw_event), Some(&session_id)),
        };
        let ts = match timestamp_field(raw, template.mapping.timestamp.as_ref()) {
            Ok(Some(value)) => value,
            Ok(None) => now,
            Err(outcome) => return outcome.with_event(Some(&raw_event), Some(&session_id)),
        };
        let source = super::source_of(&template.id);
        let key = format!("{source}:{session_id}");
        let current = self.sessions.get(&key).cloned();
        match action {
            Action::Start => {
                let round = round_id.clone().unwrap_or_else(|| format!("custom:{ts}"));
                // An explicit event id is checked first: the same payload resent is a
                // duplicate, not a new round.
                if let (Some(state), Some(event_id)) = (&current, &event_id) {
                    if state.seen(&round, event_id) {
                        return Outcome::ignored(
                            action,
                            "duplicate_event",
                            "重复事件已忽略",
                            &raw_event,
                            &session_id,
                            Some(round),
                        );
                    }
                }
                if current.as_ref().is_some_and(|state| state.round == round) {
                    return Outcome::ignored(
                        action,
                        "duplicate_start",
                        "同轮次的重复 start 已忽略",
                        &raw_event,
                        &session_id,
                        Some(round),
                    );
                }
                let mut next = SessionState::new(round.clone());
                if let Some(event_id) = &event_id {
                    next.remember(&round, event_id);
                }
                let events = vec![standard_event(
                    template,
                    &source,
                    &session_id,
                    &round,
                    ts,
                    cwd.as_deref(),
                    title.as_deref(),
                    json!({"type":"start"}),
                )];
                self.store(key, next);
                Outcome::accepted(action, &raw_event, &session_id, &round, events)
            }
            _ => {
                let Some(mut state) = current else {
                    return Outcome::ignored(
                        action,
                        "no_active_round",
                        "没有活跃轮次，事件未创建会话",
                        &raw_event,
                        &session_id,
                        None,
                    );
                };
                if let Some(round) = &round_id {
                    if *round != state.round {
                        return Outcome::ignored(
                            action,
                            "late_round",
                            "事件轮次与当前轮次不一致，已忽略",
                            &raw_event,
                            &session_id,
                            Some(state.round.clone()),
                        );
                    }
                }
                if !state.active {
                    return Outcome::ignored(
                        action,
                        "round_ended",
                        "轮次已结束，事件已忽略",
                        &raw_event,
                        &session_id,
                        Some(state.round.clone()),
                    );
                }
                if let Some(event_id) = &event_id {
                    if state.seen(&state.round.clone(), event_id) {
                        return Outcome::ignored(
                            action,
                            "duplicate_event",
                            "重复事件已忽略",
                            &raw_event,
                            &session_id,
                            Some(state.round.clone()),
                        );
                    }
                }
                let round = state.round.clone();
                let events = match action {
                    Action::Wait(reason) => {
                        let call_id = request_id.clone().unwrap_or_else(|| WAIT_CALL_ID.to_owned());
                        state.push_pending(&call_id);
                        vec![standard_event(
                            template,
                            &source,
                            &session_id,
                            &round,
                            ts,
                            cwd.as_deref(),
                            title.as_deref(),
                            json!({
                                "type":"wait",
                                "callId":call_id,
                                "tool":"custom",
                                "text": if reason == WaitReason::Permission { "等待权限确认" } else { "等待用户输入" },
                            }),
                        )]
                    }
                    Action::Resume => {
                        let call_id = request_id.clone().unwrap_or_else(|| WAIT_CALL_ID.to_owned());
                        if !state.take_pending(&call_id) {
                            return Outcome::ignored(
                                action,
                                "no_pending_wait",
                                "没有匹配的等待项，resume 已忽略",
                                &raw_event,
                                &session_id,
                                Some(round),
                            );
                        }
                        vec![standard_event(
                            template,
                            &source,
                            &session_id,
                            &round,
                            ts,
                            cwd.as_deref(),
                            title.as_deref(),
                            json!({"type":"resolve","callId":call_id}),
                        )]
                    }
                    Action::Finish(status) => {
                        state.active = false;
                        state.pending.clear();
                        vec![standard_event(
                            template,
                            &source,
                            &session_id,
                            &round,
                            ts,
                            cwd.as_deref(),
                            title.as_deref(),
                            json!({"type":"end","status":status.label()}),
                        )]
                    }
                    Action::Close => {
                        state.active = false;
                        state.pending.clear();
                        vec![standard_event(
                            template,
                            &source,
                            &session_id,
                            &round,
                            ts,
                            cwd.as_deref(),
                            title.as_deref(),
                            json!({"type":"end","status":"aborted","endedBy":raw_event}),
                        )]
                    }
                    Action::Start => unreachable!(),
                };
                if let Some(event_id) = &event_id {
                    state.remember(&round, event_id);
                }
                self.store(key, state);
                Outcome::accepted(action, &raw_event, &session_id, &round, events)
            }
        }
    }
}

fn standard_event(
    template: &Template,
    source: &str,
    session_id: &str,
    round: &str,
    ts: i64,
    cwd: Option<&str>,
    title: Option<&str>,
    extra: Value,
) -> Value {
    let mut value = json!({
        "source": source,
        "sessionId": session_id,
        "roundId": round,
        "ts": ts,
        "sourceLabel": template.name,
    });
    if let Some(cwd) = cwd.filter(|value| !value.is_empty()) {
        value["cwd"] = json!(cwd);
    }
    if let Some(title) = title.filter(|value| !value.is_empty()) {
        value["title"] = json!(title);
    }
    if let (Some(target), Some(extra)) = (value.as_object_mut(), extra.as_object()) {
        for (key, item) in extra {
            target.insert(key.clone(), item.clone());
        }
    }
    value
}

fn required_event(raw: &Value, pointer_text: &str) -> Result<String, Outcome> {
    match pointer::resolve(raw, pointer_text) {
        None | Some(Value::Null) => Err(Outcome::rejected(
            "missing_event",
            "/mapping/event",
            "载荷中缺少事件名",
        )),
        Some(Value::String(text)) => {
            if text.is_empty() {
                return Err(Outcome::rejected(
                    "type_mismatch",
                    "/mapping/event",
                    "事件名必须为非空字符串",
                ));
            }
            if text.chars().count() > EVENT_NAME_MAX {
                return Err(Outcome::rejected("too_long", "/mapping/event", "字段超出长度上限"));
            }
            Ok(text.clone())
        }
        Some(_) => Err(Outcome::rejected(
            "type_mismatch",
            "/mapping/event",
            "事件名必须为非空字符串",
        )),
    }
}

fn field_path(field: &str) -> String {
    format!("/mapping/{field}")
}

fn id_field(
    raw: &Value,
    pointer_text: Option<&String>,
    max: usize,
    field: &str,
    missing_detail: &str,
) -> Result<Option<String>, Outcome> {
    let Some(pointer_text) = pointer_text else {
        return Ok(None);
    };
    match pointer::resolve(raw, pointer_text) {
        None | Some(Value::Null) => Ok(None),
        Some(Value::String(text)) => {
            if text.is_empty() {
                return Err(Outcome::rejected(
                    "type_mismatch",
                    &field_path(field),
                    missing_detail,
                ));
            }
            if text.chars().count() > max {
                return Err(Outcome::rejected(
                    "too_long",
                    &field_path(field),
                    "字段超出长度上限",
                ));
            }
            Ok(Some(text.clone()))
        }
        Some(_) => Err(Outcome::rejected(
            "type_mismatch",
            &field_path(field),
            missing_detail,
        )),
    }
}

fn text_field(
    raw: &Value,
    pointer_text: Option<&String>,
    max: usize,
    field: &str,
) -> Result<Option<String>, Outcome> {
    let Some(pointer_text) = pointer_text else {
        return Ok(None);
    };
    match pointer::resolve(raw, pointer_text) {
        None | Some(Value::Null) => Ok(None),
        Some(Value::String(text)) => {
            if text.chars().count() > max {
                return Err(Outcome::rejected(
                    "too_long",
                    &field_path(field),
                    "字段超出长度上限",
                ));
            }
            Ok(Some(text.clone()))
        }
        Some(_) => Err(Outcome::rejected(
            "type_mismatch",
            &field_path(field),
            "字段必须为字符串",
        )),
    }
}

fn timestamp_field(raw: &Value, pointer_text: Option<&String>) -> Result<Option<i64>, Outcome> {
    let Some(pointer_text) = pointer_text else {
        return Ok(None);
    };
    match pointer::resolve(raw, pointer_text) {
        None | Some(Value::Null) => Ok(None),
        Some(Value::Number(number)) => number
            .as_i64()
            .filter(|value| *value > 0)
            .map(Some)
            .ok_or_else(|| {
                Outcome::rejected(
                    "type_mismatch",
                    &field_path("timestamp"),
                    "timestamp 必须为整数 Unix 毫秒",
                )
            }),
        Some(_) => Err(Outcome::rejected(
            "type_mismatch",
            &field_path("timestamp"),
            "timestamp 必须为整数 Unix 毫秒",
        )),
    }
}

/// `ignoreIfPresent`: a pointer counts as present unless it resolves to null or
/// an empty string.
fn present_value(raw: &Value, pointer_text: &str) -> bool {
    match pointer::resolve(raw, pointer_text) {
        None | Some(Value::Null) => false,
        Some(Value::String(text)) => !text.is_empty(),
        Some(_) => true,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn bounds_the_tracked_sessions() {
        let mut engine = Engine::new();
        for index in 0..(SESSIONS_MAX + 20) {
            engine.store(format!("custom:x:{index}"), SessionState::new("r".into()));
        }
        assert_eq!(engine.session_count(), SESSIONS_MAX);
    }

    #[test]
    fn dedup_buffer_is_bounded_and_forgets_oldest() {
        let mut state = SessionState::new("r".into());
        for index in 0..(DEDUP_PER_SESSION + 5) {
            state.remember("r", &format!("e{index}"));
        }
        assert_eq!(state.seen.len(), DEDUP_PER_SESSION);
        assert!(!state.seen("r", "e0"));
        assert!(state.seen("r", &format!("e{}", DEDUP_PER_SESSION + 4)));
    }
}
