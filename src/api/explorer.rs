// explorer.rs - Sentrix — Block Explorer Web UI

use axum::{
    extract::{State, Path},
    response::Html,
};
use crate::api::routes::SharedState;

// C-04 FIX: HTML escape to prevent XSS
pub fn html_escape(s: &str) -> String {
    s.replace('&', "&amp;")
     .replace('<', "&lt;")
     .replace('>', "&gt;")
     .replace('"', "&quot;")
     .replace('\'', "&#x27;")
}

fn truncate(s: &str, n: usize) -> String {
    if s.len() <= n { s.to_string() }
    else { format!("{}…", &s[..n]) }
}

fn srx(sentri: u64) -> f64 { sentri as f64 / 100_000_000.0 }

const CSS: &str = r#"
* { margin: 0; padding: 0; box-sizing: border-box; }
body { font-family: 'Segoe UI', system-ui, sans-serif; background: #0a0e17; color: #e1e5ee; }
.container { max-width: 1200px; margin: 0 auto; padding: 20px; }
header { background: linear-gradient(135deg, #1a1f35, #0d1225); padding: 20px 0; border-bottom: 1px solid #2a3050; }
header h1 { font-size: 24px; color: #7c8aff; }
header span { color: #5a6380; font-size: 14px; }
.stats { display: grid; grid-template-columns: repeat(auto-fit, minmax(200px, 1fr)); gap: 16px; margin: 24px 0; }
.stat-card { background: #111827; border: 1px solid #1f2937; border-radius: 12px; padding: 20px; }
.stat-card .label { color: #6b7280; font-size: 13px; text-transform: uppercase; }
.stat-card .value { color: #f9fafb; font-size: 22px; font-weight: 600; margin-top: 4px; }
table { width: 100%; border-collapse: collapse; margin-top: 16px; }
th { background: #111827; color: #9ca3af; font-size: 12px; text-transform: uppercase; padding: 12px 16px; text-align: left; }
td { padding: 12px 16px; border-bottom: 1px solid #1f2937; font-size: 14px; }
tr:hover td { background: #111827; }
a { color: #7c8aff; text-decoration: none; }
a:hover { text-decoration: underline; }
.hash { font-family: 'Consolas', monospace; font-size: 13px; color: #9ca3af; }
.badge { display: inline-block; padding: 2px 8px; border-radius: 4px; font-size: 12px; }
.badge-green { background: #064e3b; color: #34d399; }
.badge-blue { background: #1e3a5f; color: #60a5fa; }
.badge-yellow { background: #3d2b00; color: #fbbf24; }
.tabs { display: flex; gap: 8px; margin: 20px 0; flex-wrap: wrap; }
.tab { padding: 8px 16px; border-radius: 8px; background: #111827; color: #9ca3af; border: 1px solid #1f2937; }
.tab.active { background: #1e3a5f; color: #60a5fa; border-color: #3b82f6; }
.mono { font-family: 'Consolas', monospace; }
.detail-table td:first-child { color: #6b7280; width: 160px; font-size: 13px; }
h2 { margin: 20px 0; font-size: 20px; color: #f9fafb; }
h3 { margin: 24px 0 12px; font-size: 16px; color: #9ca3af; }
"#;

fn page(title: &str, body: &str) -> Html<String> {
    Html(format!(r#"<!DOCTYPE html>
<html><head><meta charset="utf-8"><title>{title} — Sentrix Explorer</title>
<meta name="viewport" content="width=device-width, initial-scale=1">
<style>{CSS}</style></head><body>
<header><div class="container">
<h1>⬡ Sentrix Explorer</h1>
<span>Chain ID: 7119 &nbsp;|&nbsp; PoA Blockchain</span>
</div></header>
<div class="container">{body}</div>
</body></html>"#))
}

fn nav_tabs(active: &str) -> String {
    let tabs = [
        ("Home",         "/explorer",              "home"),
        ("Blocks",       "/explorer/blocks",       "blocks"),
        ("Transactions", "/explorer/transactions", "transactions"),
        ("Validators",   "/explorer/validators",   "validators"),
        ("Tokens",       "/explorer/tokens",       "tokens"),
    ];
    let mut html = String::from(r#"<div class="tabs">"#);
    for (label, href, key) in &tabs {
        let cls = if *key == active { "tab active" } else { "tab" };
        html.push_str(&format!(r#"<a class="{cls}" href="{href}">{label}</a>"#));
    }
    html.push_str("</div>");
    html
}

// ── Explorer home ────────────────────────────────────────
pub async fn explorer_home(State(state): State<SharedState>) -> Html<String> {
    let bc = state.read().await;
    let stats = bc.chain_stats();
    let height = bc.height();

    let mut blocks_html = String::new();
    let start = height.saturating_sub(19);
    for i in (start..=height).rev() {
        if let Some(block) = bc.get_block(i) {
            blocks_html.push_str(&format!(
                r#"<tr>
                <td><a href="/explorer/block/{}">{}</a></td>
                <td class="hash"><a href="/explorer/block/{}">{}</a></td>
                <td>{}</td>
                <td>{}</td>
                <td class="mono"><a href="/explorer/address/{}">{}</a></td>
                </tr>"#,
                block.index, block.index,
                block.index, html_escape(&truncate(&block.hash, 16)),
                block.timestamp,
                block.tx_count(),
                html_escape(&block.validator), html_escape(&truncate(&block.validator, 20)),
            ));
        }
    }

    let body = format!(r#"
    <div class="stats">
        <div class="stat-card"><div class="label">Height</div><div class="value">{}</div></div>
        <div class="stat-card"><div class="label">Total Minted</div><div class="value">{} SRX</div></div>
        <div class="stat-card"><div class="label">Total Burned</div><div class="value">{} SRX</div></div>
        <div class="stat-card"><div class="label">Validators</div><div class="value">{}</div></div>
        <div class="stat-card"><div class="label">Tokens</div><div class="value">{}</div></div>
        <div class="stat-card"><div class="label">Mempool</div><div class="value">{}</div></div>
    </div>
    {}
    <h3>Latest Blocks</h3>
    <table>
    <tr><th>Height</th><th>Hash</th><th>Timestamp</th><th>Txs</th><th>Validator</th></tr>
    {}
    </table>"#,
        stats["height"],
        stats["total_minted_srx"],
        stats["total_burned_srx"],
        stats["active_validators"],
        stats["deployed_tokens"],
        stats["mempool_size"],
        nav_tabs("home"),
        blocks_html,
    );

    page("Home", &body)
}

// ── Blocks list ──────────────────────────────────────────
pub async fn explorer_blocks(State(state): State<SharedState>) -> Html<String> {
    let bc = state.read().await;
    let height = bc.height();

    let mut rows = String::new();
    let start = height.saturating_sub(49);
    for i in (start..=height).rev() {
        if let Some(block) = bc.get_block(i) {
            rows.push_str(&format!(
                r#"<tr>
                <td><a href="/explorer/block/{}">{}</a></td>
                <td class="hash"><a href="/explorer/block/{}">{}</a></td>
                <td>{}</td>
                <td>{}</td>
                <td class="mono"><a href="/explorer/address/{}">{}</a></td>
                </tr>"#,
                block.index, block.index,
                block.index, html_escape(&truncate(&block.hash, 20)),
                block.timestamp,
                block.tx_count(),
                html_escape(&block.validator), html_escape(&truncate(&block.validator, 20)),
            ));
        }
    }

    let body = format!(r#"
    {}
    <h3>Latest 50 Blocks</h3>
    <table>
    <tr><th>Height</th><th>Hash</th><th>Timestamp</th><th>Txs</th><th>Validator</th></tr>
    {}
    </table>"#,
        nav_tabs("blocks"),
        rows,
    );

    page("Blocks", &body)
}

// ── Transactions list ────────────────────────────────────
pub async fn explorer_transactions(State(state): State<SharedState>) -> Html<String> {
    let bc = state.read().await;
    let txs = bc.get_latest_transactions(50, 0);

    let mut coinbase_rows = String::new();
    let mut regular_rows = String::new();

    for tx in &txs {
        let txid   = tx["txid"].as_str().unwrap_or("");
        let from   = tx["from"].as_str().unwrap_or("");
        let to     = tx["to"].as_str().unwrap_or("");
        let amount = tx["amount"].as_u64().unwrap_or(0);
        let fee    = tx["fee"].as_u64().unwrap_or(0);
        let is_cb  = tx["is_coinbase"].as_bool().unwrap_or(false);
        let blk    = tx["block_index"].as_u64().unwrap_or(0);
        let ts     = tx["block_timestamp"].as_u64().unwrap_or(0);

        let from_disp = if from == "COINBASE" {
            "COINBASE".to_string()
        } else {
            html_escape(&truncate(from, 14))
        };
        let to_disp = html_escape(&truncate(to, 14));

        let row = format!(
            r#"<tr>
            <td class="hash"><a href="/explorer/tx/{}">{}</a></td>
            <td class="mono">{}</td>
            <td class="mono">{}</td>
            <td>{:.4} SRX</td>
            <td>{} sentri</td>
            <td><a href="/explorer/block/{}">#{}</a></td>
            <td>{}</td>
            </tr>"#,
            html_escape(txid), html_escape(&truncate(txid, 16)),
            from_disp, to_disp,
            srx(amount), fee,
            blk, blk, ts,
        );

        if is_cb { coinbase_rows.push_str(&row); }
        else { regular_rows.push_str(&row); }
    }

    let body = format!(r#"
    {}
    <h3>Regular Transactions</h3>
    <table>
    <tr><th>TxID</th><th>From</th><th>To</th><th>Amount</th><th>Fee</th><th>Block</th><th>Timestamp</th></tr>
    {regular_rows}
    </table>
    <h3 style="margin-top:32px">Coinbase Rewards</h3>
    <table>
    <tr><th>TxID</th><th>From</th><th>To (Validator)</th><th>Amount</th><th>Fee</th><th>Block</th><th>Timestamp</th></tr>
    {coinbase_rows}
    </table>"#,
        nav_tabs("transactions"),
    );

    page("Transactions", &body)
}

// ── Block detail ─────────────────────────────────────────
pub async fn explorer_block(
    State(state): State<SharedState>,
    Path(index): Path<u64>,
) -> Html<String> {
    let bc = state.read().await;
    match bc.get_block(index) {
        Some(block) => {
            let mut txs_html = String::new();
            for tx in &block.transactions {
                let badge = if tx.is_coinbase() {
                    r#"<span class="badge badge-green">COINBASE</span>"#
                } else {
                    r#"<span class="badge badge-blue">TX</span>"#
                };
                txs_html.push_str(&format!(
                    r#"<tr>
                    <td>{}</td>
                    <td class="hash"><a href="/explorer/tx/{}">{}</a></td>
                    <td class="mono">{}</td>
                    <td class="mono">{}</td>
                    <td>{:.4} SRX</td>
                    <td>{} sentri</td>
                    </tr>"#,
                    badge,
                    html_escape(&tx.txid), html_escape(&truncate(&tx.txid, 16)),
                    html_escape(&tx.from_address),
                    html_escape(&tx.to_address),
                    srx(tx.amount), tx.fee,
                ));
            }

            let body = format!(r#"
            {}
            <h2>Block #{}</h2>
            <table class="detail-table">
            <tr><td>Hash</td><td class="hash">{}</td></tr>
            <tr><td>Previous Hash</td><td class="hash">{}</td></tr>
            <tr><td>Merkle Root</td><td class="hash">{}</td></tr>
            <tr><td>Timestamp</td><td>{}</td></tr>
            <tr><td>Validator</td><td class="mono"><a href="/explorer/address/{}">{}</a></td></tr>
            <tr><td>Transactions</td><td>{}</td></tr>
            </table>
            <h3>Transactions</h3>
            <table>
            <tr><th>Type</th><th>TxID</th><th>From</th><th>To</th><th>Amount</th><th>Fee</th></tr>
            {}
            </table>"#,
                nav_tabs("blocks"),
                block.index,
                html_escape(&block.hash),
                html_escape(&block.previous_hash),
                html_escape(&block.merkle_root),
                block.timestamp,
                html_escape(&block.validator), html_escape(&block.validator),
                block.tx_count(),
                txs_html,
            );
            page(&format!("Block #{}", block.index), &body)
        }
        None => page("Not Found", "<h2>Block not found</h2>"),
    }
}

// ── Address detail ───────────────────────────────────────
pub async fn explorer_address(
    State(state): State<SharedState>,
    Path(address): Path<String>,
) -> Html<String> {
    let bc = state.read().await;
    let balance = bc.accounts.get_balance(&address);
    let nonce = bc.accounts.get_nonce(&address);
    let history = bc.get_address_history(&address, 50, 0);

    let mut txs_html = String::new();
    for tx in history.iter().rev().take(50) {
        let dir = tx["direction"].as_str().unwrap_or("?");
        let badge = match dir {
            "in"     => r#"<span class="badge badge-green">IN</span>"#,
            "out"    => r#"<span class="badge badge-blue">OUT</span>"#,
            "reward" => r#"<span class="badge badge-yellow">REWARD</span>"#,
            _        => r#"<span class="badge">?</span>"#,
        };
        let txid = tx["txid"].as_str().unwrap_or("");
        let amount = tx["amount"].as_u64().unwrap_or(0);
        let fee    = tx["fee"].as_u64().unwrap_or(0);
        let blk    = tx["block_index"].as_u64().unwrap_or(0);
        txs_html.push_str(&format!(
            r#"<tr>
            <td>{}</td>
            <td class="hash"><a href="/explorer/tx/{}">{}</a></td>
            <td>{:.4} SRX</td>
            <td>{} sentri</td>
            <td><a href="/explorer/block/{}">#{}</a></td>
            </tr>"#,
            badge,
            html_escape(txid), html_escape(&truncate(txid, 16)),
            srx(amount), fee,
            blk, blk,
        ));
    }

    let body = format!(r#"
    {}
    <h2>Address</h2>
    <table class="detail-table">
    <tr><td>Address</td><td class="mono">{}</td></tr>
    <tr><td>Balance</td><td>{:.8} SRX <span style="color:#6b7280">({} sentri)</span></td></tr>
    <tr><td>Nonce</td><td>{}</td></tr>
    <tr><td>Transactions</td><td>{}</td></tr>
    </table>
    <h3>Transaction History</h3>
    <table>
    <tr><th>Direction</th><th>TxID</th><th>Amount</th><th>Fee</th><th>Block</th></tr>
    {}
    </table>"#,
        nav_tabs(""),
        html_escape(&address),
        srx(balance), balance,
        nonce,
        history.len(),
        txs_html,
    );

    page(&format!("Address {}", &address[..address.len().min(10)]), &body)
}

// ── Transaction detail ───────────────────────────────────
pub async fn explorer_tx(
    State(state): State<SharedState>,
    Path(txid): Path<String>,
) -> Html<String> {
    let bc = state.read().await;
    match bc.get_transaction(&txid) {
        Some(tx_data) => {
            let tx = &tx_data["transaction"];
            let tx_from = tx["from_address"].as_str().unwrap_or("");
            let tx_to   = tx["to_address"].as_str().unwrap_or("");
            let amount  = tx["amount"].as_u64().unwrap_or(0);
            let fee     = tx["fee"].as_u64().unwrap_or(0);
            let blk     = tx_data["block_index"].as_u64().unwrap_or(0);

            let from_link = if tx_from == "COINBASE" {
                "COINBASE".to_string()
            } else {
                format!(r#"<a href="/explorer/address/{}">{}</a>"#,
                    html_escape(tx_from), html_escape(tx_from))
            };

            let body = format!(r#"
            {}
            <h2>Transaction</h2>
            <table class="detail-table">
            <tr><td>TxID</td><td class="hash">{}</td></tr>
            <tr><td>Status</td><td><span class="badge badge-green">CONFIRMED</span></td></tr>
            <tr><td>From</td><td class="mono">{}</td></tr>
            <tr><td>To</td><td class="mono"><a href="/explorer/address/{}">{}</a></td></tr>
            <tr><td>Amount</td><td>{:.8} SRX <span style="color:#6b7280">({} sentri)</span></td></tr>
            <tr><td>Fee</td><td>{} sentri</td></tr>
            <tr><td>Nonce</td><td>{}</td></tr>
            <tr><td>Block</td><td><a href="/explorer/block/{}">#{}</a></td></tr>
            <tr><td>Timestamp</td><td>{}</td></tr>
            </table>"#,
                nav_tabs("transactions"),
                html_escape(tx["txid"].as_str().unwrap_or("")),
                from_link,
                html_escape(tx_to), html_escape(tx_to),
                srx(amount), amount,
                fee,
                tx["nonce"],
                blk, blk,
                tx["timestamp"],
            );
            page("Transaction", &body)
        }
        None => page("Not Found", "<h2>Transaction not found</h2>"),
    }
}

// ── Validators page ──────────────────────────────────────
pub async fn explorer_validators(State(state): State<SharedState>) -> Html<String> {
    let bc = state.read().await;

    let mut rows = String::new();
    for v in bc.authority.validators.values() {
        let status = if v.is_active {
            r#"<span class="badge badge-green">ACTIVE</span>"#
        } else {
            r#"<span class="badge" style="background:#4a1c1c;color:#f87171">INACTIVE</span>"#
        };
        rows.push_str(&format!(
            r#"<tr>
            <td>{}</td>
            <td class="mono"><a href="/explorer/address/{}">{}</a></td>
            <td>{}</td>
            <td>{}</td>
            </tr>"#,
            html_escape(&v.name),
            html_escape(&v.address), html_escape(&v.address),
            status,
            v.blocks_produced,
        ));
    }

    let body = format!(r#"
    {}
    <table>
    <tr><th>Name</th><th>Address</th><th>Status</th><th>Blocks Produced</th></tr>
    {}
    </table>"#,
        nav_tabs("validators"),
        rows,
    );

    page("Validators", &body)
}

// ── Tokens page ──────────────────────────────────────────
pub async fn explorer_tokens(State(state): State<SharedState>) -> Html<String> {
    let bc = state.read().await;
    let tokens = bc.list_tokens();

    let mut rows = String::new();
    for t in &tokens {
        let contract = t["contract_address"].as_str().unwrap_or("");
        rows.push_str(&format!(
            r#"<tr>
            <td><strong>{}</strong></td>
            <td>{}</td>
            <td class="hash">{}</td>
            <td>{}</td>
            <td>{}</td>
            <td class="mono"><a href="/explorer/address/{}">{}</a></td>
            </tr>"#,
            html_escape(t["symbol"].as_str().unwrap_or("")),
            html_escape(t["name"].as_str().unwrap_or("")),
            html_escape(&truncate(contract, 24)),
            t["total_supply"],
            t["holders"],
            html_escape(t["owner"].as_str().unwrap_or("")),
            html_escape(&truncate(t["owner"].as_str().unwrap_or(""), 20)),
        ));
    }

    let body = format!(r#"
    {}
    <table>
    <tr><th>Symbol</th><th>Name</th><th>Contract</th><th>Supply</th><th>Holders</th><th>Owner</th></tr>
    {}
    </table>"#,
        nav_tabs("tokens"),
        rows,
    );

    page("Tokens", &body)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_c04_html_escape_xss_payloads() {
        assert_eq!(
            html_escape("<script>alert('xss')</script>"),
            "&lt;script&gt;alert(&#x27;xss&#x27;)&lt;/script&gt;"
        );
        assert_eq!(
            html_escape("normal text"),
            "normal text"
        );
        assert_eq!(
            html_escape(r#"<img src=x onerror="alert(1)">"#),
            "&lt;img src=x onerror=&quot;alert(1)&quot;&gt;"
        );
        assert_eq!(
            html_escape("&amp; already escaped"),
            "&amp;amp; already escaped"
        );
        assert_eq!(
            html_escape("0x4f3319a747fd564136209cd5d9e7d1a1e4d142be"),
            "0x4f3319a747fd564136209cd5d9e7d1a1e4d142be"
        );
    }

    #[test]
    fn test_truncate() {
        assert_eq!(truncate("hello", 10), "hello");
        assert_eq!(truncate("hello world long string", 5), "hello…");
        assert_eq!(truncate("", 5), "");
    }

    #[test]
    fn test_srx_conversion() {
        assert!((srx(100_000_000) - 1.0).abs() < 1e-9);
        assert!((srx(0) - 0.0).abs() < 1e-9);
    }
}
