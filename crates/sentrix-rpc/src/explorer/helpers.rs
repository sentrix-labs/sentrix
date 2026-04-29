// The little formatting + escaping helpers every explorer page reaches
// for. Lives over here so the per-page handlers in explorer.rs aren't
// 30% string-massaging boilerplate.

use sentrix_core::blockchain::Blockchain;

/// HTML-escape user-facing values so explorer pages don't render
/// attacker-supplied strings as live markup.
pub fn html_escape(s: &str) -> String {
    s.replace('&', "&amp;")
        .replace('<', "&lt;")
        .replace('>', "&gt;")
        .replace('"', "&quot;")
        .replace('\'', "&#x27;")
}

/// Truncate a string at byte boundary `n` and append an ellipsis when
/// truncation actually happened. Inputs are ASCII-only (hex/addr) so
/// byte-boundary cuts are safe.
pub(super) fn truncate(s: &str, n: usize) -> String {
    if s.len() <= n {
        s.to_string()
    } else {
        format!("{}…", &s[..n])
    }
}

/// Sentri → SRX with 8 decimal places.
pub(super) fn srx(sentri: u64) -> f64 {
    sentri as f64 / 100_000_000.0
}

/// Render an address with an optional "label badge" sourced from the
/// on-chain validator registry (`bc.authority.validators[addr].name`).
/// Adding/renaming/removing a validator via `sentrix validator add`
/// propagates here without a binary rebuild — no `address_label` fork.
/// Non-validator addresses render as plain monospace hex.
pub(super) fn addr_with_label(addr: &str, bc: &Blockchain) -> String {
    let label = bc
        .authority
        .validators
        .get(addr)
        .map(|v| v.name.as_str())
        .filter(|n| !n.is_empty());
    match label {
        Some(name) => format!(
            r#"{} <span style="background:#1a2a1a;color:#4ade80;font-size:11px;padding:1px 6px;border-radius:4px;margin-left:4px">{}</span>"#,
            html_escape(addr),
            html_escape(name)
        ),
        None => html_escape(addr).to_string(),
    }
}

/// Format a unix timestamp as `DD Mon YYYY, HH:MM UTC` via chrono.
/// Pre-1970 / overflow falls back to the raw seconds count so an
/// out-of-range value never poisons a page render with a panic.
pub(super) fn fmt_ts(unix: u64) -> String {
    use chrono::{DateTime, Utc};
    match DateTime::<Utc>::from_timestamp(unix as i64, 0) {
        Some(dt) => dt.format("%d %b %Y, %H:%M UTC").to_string(),
        None => format!("{unix} (invalid ts)"),
    }
}

/// Format `day_key` (days since 1970-01-01) as `dd/mm` via chrono.
/// Same overflow safety as `fmt_ts`.
pub(super) fn fmt_day(day_key: u64) -> String {
    use chrono::{DateTime, Utc};
    let secs = day_key.saturating_mul(86400) as i64;
    match DateTime::<Utc>::from_timestamp(secs, 0) {
        Some(dt) => dt.format("%d/%m").to_string(),
        None => format!("{day_key}"),
    }
}
