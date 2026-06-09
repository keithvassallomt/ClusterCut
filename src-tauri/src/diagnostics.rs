//! In-memory, never-persisted diagnostics channel. Sensitive pairing/mTLS
//! detail (PINs, fingerprints) flows ONLY here — never to the `tracing` file
//! log. A bounded ring buffer holds recent events; each is also emitted to the
//! UI as `diagnostic-event`. See the PIN-safe-diagnostics spec.

use serde::Serialize;
use std::collections::VecDeque;
use tauri::Emitter;

/// Max events retained in memory (FIFO eviction). Memory-only; cleared on restart.
pub const DIAG_CAP: usize = 1000;

#[derive(Serialize, Clone, Copy, Debug, PartialEq)]
#[serde(rename_all = "snake_case")]
pub enum DiagLevel {
    Minimal,
    Detailed,
    Debug,
}

#[derive(Serialize, Clone, Debug)]
pub struct DiagnosticEvent {
    pub ts_ms: u64,
    pub level: DiagLevel,
    pub kind: String,
    pub peer: Option<String>,
    pub message: String,
}

/// Map a transport-level mTLS event kind to its diagnostics level + display
/// message. Routine connection churn (`connect`/`drop`) is **Detailed**, not
/// Minimal: with the per-message connection model these fire on every 5 s
/// heartbeat and anti-entropy cluster-name push, so surfacing them at Minimal
/// buried the Event Log in continuous connect/drop scroll. Handshake failures
/// stay Detailed and meaningful.
pub fn classify_mtls_event(kind: &str, detail: Option<String>) -> (DiagLevel, String) {
    match kind {
        "connect" => (DiagLevel::Detailed, "mTLS connection established".to_string()),
        "drop" => (DiagLevel::Detailed, "mTLS connection dropped".to_string()),
        "handshake_failed" => (
            DiagLevel::Detailed,
            format!("mTLS handshake failed: {}", detail.unwrap_or_default()),
        ),
        other => (DiagLevel::Detailed, other.to_string()),
    }
}

/// Push an event into the buffer, evicting the oldest when at capacity. Pure
/// (no I/O) so it is unit-testable.
pub(crate) fn push_capped(buf: &mut VecDeque<DiagnosticEvent>, ev: DiagnosticEvent, cap: usize) {
    while buf.len() >= cap {
        buf.pop_front();
    }
    buf.push_back(ev);
}

/// Record a diagnostic event: append to the in-memory buffer and emit it to the
/// UI. Always-on (no gating); the panel's level dropdown filters the display.
pub fn push_diagnostic(
    state: &crate::state::AppState,
    app_handle: &tauri::AppHandle,
    level: DiagLevel,
    kind: &str,
    peer: Option<String>,
    message: String,
) {
    let ts_ms = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|d| d.as_millis() as u64)
        .unwrap_or(0);
    let ev = DiagnosticEvent { ts_ms, level, kind: kind.to_string(), peer, message };
    {
        let mut buf = state.diagnostics.lock().unwrap();
        push_capped(&mut buf, ev.clone(), DIAG_CAP);
    }
    let _ = app_handle.emit("diagnostic-event", &ev);
}

#[cfg(test)]
mod tests {
    use super::*;

    fn ev(msg: &str) -> DiagnosticEvent {
        DiagnosticEvent { ts_ms: 0, level: DiagLevel::Minimal, kind: "test".into(), peer: None, message: msg.into() }
    }

    #[test]
    fn evicts_oldest_at_capacity() {
        let mut buf = VecDeque::new();
        for i in 0..5 {
            push_capped(&mut buf, ev(&i.to_string()), 3);
        }
        assert_eq!(buf.len(), 3);
        assert_eq!(buf.front().unwrap().message, "2"); // 0,1 evicted
        assert_eq!(buf.back().unwrap().message, "4");
    }

    #[test]
    fn routine_mtls_connect_drop_are_not_minimal() {
        // Heartbeat/anti-entropy churn must not surface on the Minimal filter
        // (the Event Log dropdown shows events with level <= selected level, so
        // Minimal must be reserved for meaningful events like pairing).
        assert_eq!(classify_mtls_event("connect", None).0, DiagLevel::Detailed);
        assert_eq!(classify_mtls_event("drop", None).0, DiagLevel::Detailed);
    }

    #[test]
    fn mtls_handshake_failure_stays_detailed_with_reason() {
        let (level, msg) = classify_mtls_event("handshake_failed", Some("bad cert".into()));
        assert_eq!(level, DiagLevel::Detailed);
        assert!(msg.contains("bad cert"), "message should carry the reason: {msg}");
    }

    #[test]
    fn mtls_connect_drop_messages_preserved() {
        assert_eq!(classify_mtls_event("connect", None).1, "mTLS connection established");
        assert_eq!(classify_mtls_event("drop", None).1, "mTLS connection dropped");
    }

    #[test]
    fn level_serializes_snake_case() {
        assert_eq!(serde_json::to_string(&DiagLevel::Minimal).unwrap(), "\"minimal\"");
        assert_eq!(serde_json::to_string(&DiagLevel::Detailed).unwrap(), "\"detailed\"");
        assert_eq!(serde_json::to_string(&DiagLevel::Debug).unwrap(), "\"debug\"");
    }
}
