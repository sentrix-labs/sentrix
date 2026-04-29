// explorer.rs - Sentrix — Block Explorer Web UI
//
// Per-page handlers live in this file. Pure helpers (html_escape,
// truncate, srx, addr_with_label, fmt_ts, fmt_day) are in
// `explorer/helpers.rs`. Shared chrome (CSS, page wrapper, nav tabs,
// chart-section script) is in `explorer/layout.rs`.

mod helpers;
mod layout;

use crate::routes::SharedState;
use axum::{
    Json,
    extract::{Path, State},
    response::Html,
};
use serde::Serialize;
use std::sync::OnceLock;
use std::time::{Duration, Instant};
use tokio::sync::Mutex as TokioMutex;

pub use helpers::html_escape;
use helpers::{addr_with_label, fmt_day, fmt_ts, srx, truncate};
use layout::{CHART_SECTION, nav_tabs, page};

struct CachedPage {
    html: String,
    at: Instant,
}
static HOME_CACHE: OnceLock<TokioMutex<Option<CachedPage>>> = OnceLock::new();
static RICHLIST_CACHE: OnceLock<TokioMutex<Option<CachedPage>>> = OnceLock::new();

#[derive(Serialize, Clone)]
pub struct DailyStat {
    pub date: String,
    pub blocks: u64,
    pub transactions: u64,
}
struct DailyCache {
    data: Vec<DailyStat>,
    at: Instant,
}
static DAILY_CACHE: OnceLock<TokioMutex<Option<DailyCache>>> = OnceLock::new();


// ── Explorer home ────────────────────────────────────────
pub async fn explorer_home(State(state): State<SharedState>) -> Html<String> {
    const HOME_TTL: Duration = Duration::from_secs(10);
    let cache = HOME_CACHE.get_or_init(|| TokioMutex::new(None));
    {
        let guard = cache.lock().await;
        if let Some(ref c) = *guard
            && c.at.elapsed() < HOME_TTL
        {
            return Html(c.html.clone());
        }
    }
    let bc = state.read().await;
    let stats = bc.chain_stats();
    let height = bc.height();

    // ── Compute extended network stats ────────────────────
    // One pass over all blocks: count regular TXs and gather sample timestamps
    let sample_start = height.saturating_sub(99); // last 100 blocks
    let mut total_regular_txs: u64 = 0;
    let mut sample_regular_txs: u64 = 0;
    let mut sample_oldest_ts: u64 = 0;
    let mut sample_newest_ts: u64 = 0;

    for i in 0..=height {
        if let Some(block) = bc.get_block_any(i) {
            let non_cb = block
                .transactions
                .iter()
                .filter(|t| !t.is_coinbase())
                .count() as u64;
            total_regular_txs += non_cb;
            if i >= sample_start {
                sample_regular_txs += non_cb;
                if i == sample_start {
                    sample_oldest_ts = block.timestamp;
                }
                if i == height {
                    sample_newest_ts = block.timestamp;
                }
            }
        }
    }

    let sample_span = sample_newest_ts.saturating_sub(sample_oldest_ts);
    let sample_blocks = height.saturating_sub(sample_start); // number of intervals

    let tps = if sample_span > 0 {
        format!("{:.4}", sample_regular_txs as f64 / sample_span as f64)
    } else {
        "0.0000".to_string()
    };
    let avg_block_time = if sample_blocks > 0 && sample_span > 0 {
        format!("{:.1}s", sample_span as f64 / sample_blocks as f64)
    } else {
        "—".to_string()
    };

    // ── Latest blocks table ───────────────────────────────
    let mut blocks_html = String::new();
    let start = height.saturating_sub(19);
    for i in (start..=height).rev() {
        if let Some(block) = bc.get_block_any(i) {
            blocks_html.push_str(&format!(
                r#"<tr>
                <td><a href="/explorer/block/{}">{}</a></td>
                <td class="hash"><a href="/explorer/block/{}">{}</a></td>
                <td>{}</td>
                <td>{}</td>
                <td class="mono"><a href="/explorer/address/{}">{}</a></td>
                </tr>"#,
                block.index,
                block.index,
                block.index,
                html_escape(&truncate(&block.hash, 16)),
                html_escape(&fmt_ts(block.timestamp)),
                block.tx_count(),
                html_escape(&block.validator),
                html_escape(&truncate(&block.validator, 20)),
            ));
        }
    }

    let body = format!(
        r#"
    <div class="stats">
        <div class="stat-card">
            <div class="label">Block Height</div>
            <div class="value">{}</div>
        </div>
        <div class="stat-card">
            <div class="label">Total Transactions</div>
            <div class="value">{}</div>
            <div class="sub">non-coinbase</div>
        </div>
        <div class="stat-card">
            <div class="label">TPS</div>
            <div class="value">{}</div>
            <div class="sub">last 100 blocks</div>
        </div>
        <div class="stat-card">
            <div class="label">Avg Block Time</div>
            <div class="value">{}</div>
            <div class="sub">last 100 blocks</div>
        </div>
        <div class="stat-card">
            <div class="label">Active Validators</div>
            <div class="value">{}</div>
        </div>
        <div class="stat-card">
            <div class="label">Total Minted</div>
            <div class="value">{:.2} SRX</div>
        </div>
        <div class="stat-card">
            <div class="label">Total Burned</div>
            <div class="value">{:.4} SRX</div>
        </div>
        <div class="stat-card">
            <div class="label">Tokens Deployed</div>
            <div class="value">{}</div>
        </div>
    </div>
    {}
    {}
    <h3>Latest Blocks</h3>
    <table>
    <tr><th>Height</th><th>Hash</th><th>Timestamp</th><th>Txs</th><th>Validator</th></tr>
    {}
    </table>"#,
        height,
        total_regular_txs,
        tps,
        avg_block_time,
        stats["active_validators"],
        stats["total_minted_srx"].as_f64().unwrap_or(0.0),
        stats["total_burned_srx"].as_f64().unwrap_or(0.0),
        stats["deployed_tokens"],
        CHART_SECTION,
        nav_tabs("home"),
        blocks_html,
    );

    let result = page("Home", &body);
    {
        let mut guard = HOME_CACHE
            .get_or_init(|| TokioMutex::new(None))
            .lock()
            .await;
        *guard = Some(CachedPage {
            html: result.0.clone(),
            at: Instant::now(),
        });
    }
    result
}

// ── Daily stats endpoint ─────────────────────────────────
pub async fn stats_daily(State(state): State<SharedState>) -> Json<Vec<DailyStat>> {
    const TTL: Duration = Duration::from_secs(300); // 5 min cache
    let cache = DAILY_CACHE.get_or_init(|| TokioMutex::new(None));
    {
        let guard = cache.lock().await;
        if let Some(ref c) = *guard
            && c.at.elapsed() < TTL
        {
            return Json(c.data.clone());
        }
    }

    let bc = state.read().await;
    let height = bc.height();
    // A9: bucket by UTC days so the daily stats line up with the explorer's
    // UTC clock (previously used WIB / UTC+7 which off-by-one'd the chart
    // around 17:00 UTC).
    let today_day = bc
        .get_block_any(height)
        .map(|b| b.timestamp / 86400)
        .unwrap_or(0);

    let mut map: std::collections::HashMap<u64, (u64, u64)> = std::collections::HashMap::new();

    if today_day > 0 {
        let earliest = today_day.saturating_sub(13);
        for i in 0..=height {
            if let Some(block) = bc.get_block_any(i) {
                let day = block.timestamp / 86400;
                if day >= earliest && day <= today_day {
                    let e = map.entry(day).or_insert((0, 0));
                    e.0 += 1;
                    e.1 += block
                        .transactions
                        .iter()
                        .filter(|t| !t.is_coinbase())
                        .count() as u64;
                }
            }
        }
    }

    let earliest = today_day.saturating_sub(13);
    let mut result: Vec<DailyStat> = (0..14u64)
        .map(|i| {
            let day = earliest + i;
            let (blocks, txs) = map.get(&day).copied().unwrap_or((0, 0));
            DailyStat {
                date: fmt_day(day),
                blocks,
                transactions: txs,
            }
        })
        .collect();

    // If chain has no blocks yet, fill with placeholder dates from system time
    if today_day == 0 {
        use std::time::{SystemTime, UNIX_EPOCH};
        let now_day = SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .map(|d| d.as_secs() / 86400)
            .unwrap_or(0);
        result = (0..14u64)
            .map(|i| DailyStat {
                date: fmt_day(now_day.saturating_sub(13) + i),
                blocks: 0,
                transactions: 0,
            })
            .collect();
    }

    let mut guard = cache.lock().await;
    *guard = Some(DailyCache {
        data: result.clone(),
        at: Instant::now(),
    });
    Json(result)
}

// ── Blocks list ──────────────────────────────────────────
pub async fn explorer_blocks(State(state): State<SharedState>) -> Html<String> {
    let bc = state.read().await;
    let height = bc.height();

    let mut rows = String::new();
    let start = height.saturating_sub(49);
    for i in (start..=height).rev() {
        if let Some(block) = bc.get_block_any(i) {
            rows.push_str(&format!(
                r#"<tr>
                <td><a href="/explorer/block/{}">{}</a></td>
                <td class="hash"><a href="/explorer/block/{}">{}</a></td>
                <td>{}</td>
                <td>{}</td>
                <td class="mono"><a href="/explorer/address/{}">{}</a></td>
                </tr>"#,
                block.index,
                block.index,
                block.index,
                html_escape(&truncate(&block.hash, 20)),
                html_escape(&fmt_ts(block.timestamp)),
                block.tx_count(),
                html_escape(&block.validator),
                html_escape(&truncate(&block.validator, 20)),
            ));
        }
    }

    let body = format!(
        r#"
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
    // Scan only last 1000 blocks for performance
    let height = bc.height();
    let scan_start = height.saturating_sub(999);

    let mut coinbase_rows = String::new();
    let mut regular_rows = String::new();

    for i in (scan_start..=height).rev() {
        if let Some(block) = bc.get_block_any(i) {
            for tx in &block.transactions {
                let is_cb = tx.is_coinbase();
                let data_str = &tx.data;
                let is_evm = data_str.starts_with("EVM:");
                let is_token = !is_evm
                    && !data_str.is_empty()
                    && sentrix_primitives::transaction::TokenOp::decode(data_str).is_some();
                let is_create =
                    is_evm && tx.to_address == sentrix_primitives::transaction::TOKEN_OP_ADDRESS;

                let type_badge = if is_cb {
                    r#"<span class="badge badge-blue">COINBASE</span>"#
                } else if is_create {
                    r#"<span class="badge badge-yellow">EVM CREATE</span>"#
                } else if is_evm {
                    r#"<span class="badge badge-yellow">EVM CALL</span>"#
                } else if is_token {
                    r#"<span class="badge badge-blue">TOKEN</span>"#
                } else {
                    r#"<span class="badge badge-blue">TRANSFER</span>"#
                };

                let from_disp = if tx.from_address == "COINBASE" {
                    "COINBASE".to_string()
                } else {
                    html_escape(&truncate(&tx.from_address, 14))
                };
                let to_disp = html_escape(&truncate(&tx.to_address, 14));

                let row = format!(
                    r#"<tr>
                    <td class="hash"><a href="/explorer/tx/{}">{}</a></td>
                    <td>{}</td>
                    <td class="mono">{}</td>
                    <td class="mono">{}</td>
                    <td>{:.8} SRX</td>
                    <td>{} sentri</td>
                    <td><a href="/explorer/block/{}">#{}</a></td>
                    <td>{}</td>
                    </tr>"#,
                    html_escape(&tx.txid),
                    html_escape(&truncate(&tx.txid, 16)),
                    type_badge,
                    from_disp,
                    to_disp,
                    srx(tx.amount),
                    tx.fee,
                    block.index,
                    block.index,
                    html_escape(&fmt_ts(block.timestamp)),
                );

                if is_cb {
                    coinbase_rows.push_str(&row);
                } else {
                    regular_rows.push_str(&row);
                }
            }
        }
    }

    let regular_content = if regular_rows.is_empty() {
        r#"<p style="color:#6b7280;padding:24px 0;text-align:center">No regular transactions yet</p>"#.to_string()
    } else {
        format!(
            r#"<table>
        <tr><th>TxID</th><th>Type</th><th>From</th><th>To</th><th>Amount</th><th>Fee</th><th>Block</th><th>Time (UTC)</th></tr>
        {regular_rows}
        </table>"#
        )
    };

    let coinbase_content = format!(
        r#"<table>
    <tr><th>TxID</th><th>Type</th><th>From</th><th>To (Validator)</th><th>Amount</th><th>Fee</th><th>Block</th><th>Time (UTC)</th></tr>
    {coinbase_rows}
    </table>"#
    );

    let body = format!(
        r#"
    {nav}
    <div class="tabs" style="margin-top:0">
        <button class="tab active" onclick="showTab('regular',this)">Regular Transactions</button>
        <button class="tab" onclick="showTab('coinbase',this)">Coinbase Rewards</button>
    </div>
    <div id="tab-regular">{regular_content}</div>
    <div id="tab-coinbase" style="display:none">{coinbase_content}</div>
    <script>
    function showTab(name,btn){{
        document.getElementById('tab-regular').style.display='none';
        document.getElementById('tab-coinbase').style.display='none';
        document.querySelectorAll('.tabs button').forEach(b=>b.classList.remove('active'));
        document.getElementById('tab-'+name).style.display='';
        btn.classList.add('active');
    }}
    </script>"#,
        nav = nav_tabs("transactions"),
    );

    page("Transactions", &body)
}

// ── Block detail ─────────────────────────────────────────
pub async fn explorer_block(
    State(state): State<SharedState>,
    Path(index): Path<u64>,
) -> Html<String> {
    let bc = state.read().await;
    match bc.get_block_any(index) {
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
                    <td>{:.8} SRX</td>
                    <td>{} sentri</td>
                    </tr>"#,
                    badge,
                    html_escape(&tx.txid),
                    html_escape(&truncate(&tx.txid, 16)),
                    html_escape(&tx.from_address),
                    html_escape(&tx.to_address),
                    srx(tx.amount),
                    tx.fee,
                ));
            }

            let body = format!(
                r#"
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
                html_escape(&fmt_ts(block.timestamp)),
                html_escape(&block.validator),
                html_escape(&block.validator),
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
            "in" => r#"<span class="badge badge-green">IN</span>"#,
            "out" => r#"<span class="badge badge-blue">OUT</span>"#,
            "reward" => r#"<span class="badge badge-yellow">REWARD</span>"#,
            _ => r#"<span class="badge">?</span>"#,
        };
        let txid = tx["txid"].as_str().unwrap_or("");
        let amount = tx["amount"].as_u64().unwrap_or(0);
        let fee = tx["fee"].as_u64().unwrap_or(0);
        let blk = tx["block_index"].as_u64().unwrap_or(0);
        txs_html.push_str(&format!(
            r#"<tr>
            <td>{}</td>
            <td class="hash"><a href="/explorer/tx/{}">{}</a></td>
            <td>{:.8} SRX</td>
            <td>{} sentri</td>
            <td><a href="/explorer/block/{}">#{}</a></td>
            </tr>"#,
            badge,
            html_escape(txid),
            html_escape(&truncate(txid, 16)),
            srx(amount),
            fee,
            blk,
            blk,
        ));
    }

    let body = format!(
        r#"
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
        addr_with_label(&address, &bc),
        srx(balance),
        balance,
        nonce,
        history.len(),
        txs_html,
    );

    page(
        &format!("Address {}", &address[..address.len().min(10)]),
        &body,
    )
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
            let tx_to = tx["to_address"].as_str().unwrap_or("");
            let amount = tx["amount"].as_u64().unwrap_or(0);
            let fee = tx["fee"].as_u64().unwrap_or(0);
            let blk = tx_data["block_index"].as_u64().unwrap_or(0);
            let data_str = tx["data"].as_str().unwrap_or("");
            let txid_str = tx["txid"].as_str().unwrap_or("");

            let from_link = if tx_from == "COINBASE" {
                "COINBASE".to_string()
            } else {
                format!(
                    r#"<a href="/explorer/address/{}">{}</a>"#,
                    html_escape(tx_from),
                    html_escape(tx_from)
                )
            };

            // B1: classify tx and surface EVM-specific details. EVM txs are
            // tagged "EVM:gas:hex_data" in the data field by the
            // eth_sendRawTransaction relayer. Token ops use TokenOp::is_token_op.
            let is_evm = data_str.starts_with("EVM:");
            let is_token = !is_evm
                && !data_str.is_empty()
                && sentrix_primitives::transaction::TokenOp::decode(data_str).is_some();
            let is_coinbase = tx_from == "COINBASE";
            let is_create = is_evm && tx_to == sentrix_primitives::transaction::TOKEN_OP_ADDRESS;

            let (type_badge_class, type_label) = if is_coinbase {
                ("badge-blue", "COINBASE")
            } else if is_create {
                ("badge-yellow", "EVM CREATE")
            } else if is_evm {
                ("badge-yellow", "EVM CALL")
            } else if is_token {
                ("badge-blue", "TOKEN OP")
            } else {
                ("badge-blue", "NATIVE TRANSFER")
            };

            // B1: status reflects EVM revert state. failed_evm_txs is populated
            // by execute_evm_tx_in_block when revm reports !receipt.success.
            let (status_badge_class, status_label) =
                if is_evm && bc.accounts.is_evm_tx_failed(txid_str) {
                    ("badge", "REVERTED")
                } else {
                    ("badge-green", "CONFIRMED")
                };
            let status_style = if status_label == "REVERTED" {
                r#"style="background:#4a1c1c;color:#f87171""#
            } else {
                ""
            };

            // EVM-specific row block: gas_limit + raw calldata preview. For
            // CREATE we hint that the receipt holds the deployed address.
            let evm_rows = if is_evm {
                let parts: Vec<&str> = data_str.splitn(3, ':').collect();
                let gas_limit = parts.get(1).copied().unwrap_or("?");
                let calldata = parts.get(2).copied().unwrap_or("");
                let calldata_preview = if calldata.len() > 200 {
                    format!("{}…", &calldata[..200])
                } else {
                    calldata.to_string()
                };
                let create_hint = if is_create {
                    "<tr><td>Contract</td><td><span class=\"badge badge-yellow\">CREATE</span> \
                     — call <code>eth_getTransactionReceipt</code> for the deployed address</td></tr>".to_string()
                } else {
                    String::new()
                };
                format!(
                    r#"<tr><td>Gas limit</td><td class="mono">{}</td></tr>
                       <tr><td>Calldata</td><td class="hash" style="word-break:break-all">{}</td></tr>
                       {}"#,
                    html_escape(gas_limit),
                    html_escape(&calldata_preview),
                    create_hint,
                )
            } else {
                String::new()
            };

            let body = format!(
                r#"
            {}
            <h2>Transaction</h2>
            <table class="detail-table">
            <tr><td>TxID</td><td class="hash">{}</td></tr>
            <tr><td>Type</td><td><span class="badge {}">{}</span></td></tr>
            <tr><td>Status</td><td><span class="badge {}" {}>{}</span></td></tr>
            <tr><td>From</td><td class="mono">{}</td></tr>
            <tr><td>To</td><td class="mono"><a href="/explorer/address/{}">{}</a></td></tr>
            <tr><td>Amount</td><td>{:.8} SRX <span style="color:#6b7280">({} sentri)</span></td></tr>
            <tr><td>Fee</td><td>{} sentri</td></tr>
            <tr><td>Nonce</td><td>{}</td></tr>
            <tr><td>Block</td><td><a href="/explorer/block/{}">#{}</a></td></tr>
            <tr><td>Timestamp</td><td>{}</td></tr>
            {}
            </table>"#,
                nav_tabs("transactions"),
                html_escape(txid_str),
                type_badge_class,
                type_label,
                status_badge_class,
                status_style,
                status_label,
                from_link,
                html_escape(tx_to),
                html_escape(tx_to),
                srx(amount),
                amount,
                fee,
                tx["nonce"],
                blk,
                blk,
                html_escape(&fmt_ts(tx["timestamp"].as_u64().unwrap_or(0))),
                evm_rows,
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
            <td><a href="/explorer/validator/{}">{}</a></td>
            <td class="mono"><a href="/explorer/address/{}">{}</a></td>
            <td>{}</td>
            <td>{}</td>
            </tr>"#,
            html_escape(&v.address),
            html_escape(&v.name),
            html_escape(&v.address),
            html_escape(&v.address),
            status,
            v.blocks_produced,
        ));
    }

    let body = format!(
        r#"
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
            <td><a href="/explorer/token/{}"><strong>{}</strong></a></td>
            <td>{}</td>
            <td class="hash"><a href="/explorer/token/{}">{}</a></td>
            <td>{}</td>
            <td>{}</td>
            <td class="mono"><a href="/explorer/address/{}">{}</a></td>
            </tr>"#,
            html_escape(contract),
            html_escape(t["symbol"].as_str().unwrap_or("")),
            html_escape(t["name"].as_str().unwrap_or("")),
            html_escape(contract),
            html_escape(&truncate(contract, 24)),
            t["total_supply"],
            t["holders"],
            html_escape(t["owner"].as_str().unwrap_or("")),
            html_escape(&truncate(t["owner"].as_str().unwrap_or(""), 20)),
        ));
    }

    let body = format!(
        r#"
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

// ── Rich List ─────────────────────────────────────────────
pub async fn explorer_richlist(State(state): State<SharedState>) -> Html<String> {
    const RICHLIST_TTL: Duration = Duration::from_secs(30);
    let cache = RICHLIST_CACHE.get_or_init(|| TokioMutex::new(None));
    {
        let guard = cache.lock().await;
        if let Some(ref c) = *guard
            && c.at.elapsed() < RICHLIST_TTL
        {
            return Html(c.html.clone());
        }
    }
    let bc = state.read().await;

    // Collect all non-zero balances, sort descending
    let mut holders: Vec<(&String, u64)> = bc
        .accounts
        .accounts
        .iter()
        .filter(|(_, a)| a.balance > 0)
        .map(|(addr, a)| (addr, a.balance))
        .collect();
    holders.sort_by_key(|h| std::cmp::Reverse(h.1));
    let holders = &holders[..holders.len().min(50)];

    let mut rows = String::new();
    for (rank, (address, balance_sentri)) in holders.iter().enumerate() {
        let balance_srx = *balance_sentri as f64 / 100_000_000.0;
        let pct = balance_srx / (bc.max_supply_for(bc.height()) as f64 / 100_000_000.0) * 100.0;
        rows.push_str(&format!(
            r#"<tr>
            <td style="color:#6b7280">#{}</td>
            <td class="mono"><a href="/explorer/address/{}">{}</a></td>
            <td>{:.8} SRX</td>
            <td style="color:#9ca3af">{:.6}%</td>
            </tr>"#,
            rank + 1,
            html_escape(address),
            addr_with_label(address, &bc),
            balance_srx,
            pct,
        ));
    }

    let empty = if rows.is_empty() {
        r#"<p style="color:#6b7280;padding:24px 0;text-align:center">No accounts found</p>"#
    } else {
        ""
    };

    let max_supply_display = format!(
        "{:.0}",
        bc.max_supply_for(bc.height()) as f64 / 100_000_000.0
    );
    let body = format!(
        r#"
    {}
    <h2>Rich List — Top SRX Holders</h2>
    <p style="color:#6b7280;font-size:13px;margin-bottom:16px">Top 50 addresses by SRX balance &nbsp;|&nbsp; Total supply: {} SRX</p>
    {}
    <table>
    <tr><th>Rank</th><th>Address</th><th>Balance</th><th>% of Supply</th></tr>
    {}
    </table>"#,
        nav_tabs("richlist"),
        max_supply_display,
        empty,
        rows,
    );

    let result = page("Rich List", &body);
    {
        let mut guard = RICHLIST_CACHE
            .get_or_init(|| TokioMutex::new(None))
            .lock()
            .await;
        *guard = Some(CachedPage {
            html: result.0.clone(),
            at: Instant::now(),
        });
    }
    result
}

// ── Validator detail ──────────────────────────────────────
pub async fn explorer_validator(
    State(state): State<SharedState>,
    Path(address): Path<String>,
) -> Html<String> {
    let bc = state.read().await;
    let v = match bc.authority.validators.get(&address) {
        Some(v) => v,
        None => return page("Not Found", "<h2>Validator not found</h2>"),
    };

    let status = if v.is_active {
        r#"<span class="badge badge-green">ACTIVE</span>"#
    } else {
        r#"<span class="badge" style="background:#4a1c1c;color:#f87171">INACTIVE</span>"#
    };

    // Slot in round-robin (position among active validators sorted by address)
    let active_validators = bc.authority.active_validators();
    let slot = active_validators
        .iter()
        .position(|av| av.address == v.address)
        .map(|i| format!("#{}", i + 1))
        .unwrap_or_else(|| "—".to_string());

    let earned_srx = v.blocks_produced as f64 * 1.0; // 1 SRX per block

    // Last 20 blocks produced by this validator
    let mut block_rows = String::new();
    let mut found = 0u32;
    for block in bc.chain.iter().rev() {
        if block.validator == v.address {
            block_rows.push_str(&format!(
                r#"<tr>
                <td><a href="/explorer/block/{}">{}</a></td>
                <td class="hash"><a href="/explorer/block/{}">{}</a></td>
                <td>{}</td>
                <td>{}</td>
                </tr>"#,
                block.index,
                block.index,
                block.index,
                html_escape(&truncate(&block.hash, 20)),
                html_escape(&fmt_ts(block.timestamp)),
                block.tx_count(),
            ));
            found += 1;
            if found >= 20 {
                break;
            }
        }
    }

    let body = format!(
        r#"
    {}
    <h2>Validator: {}</h2>
    <table class="detail-table">
    <tr><td>Name</td><td>{}</td></tr>
    <tr><td>Address</td><td class="mono"><a href="/explorer/address/{}">{}</a></td></tr>
    <tr><td>Status</td><td>{}</td></tr>
    <tr><td>Blocks Produced</td><td>{}</td></tr>
    <tr><td>Total Earned</td><td>{:.2} SRX</td></tr>
    <tr><td>Round-Robin Slot</td><td>{}</td></tr>
    <tr><td>Registered At</td><td>{}</td></tr>
    <tr><td>Last Block Time</td><td>{}</td></tr>
    </table>
    <h3>Last 20 Blocks Produced</h3>
    <table>
    <tr><th>Height</th><th>Hash</th><th>Timestamp</th><th>Txs</th></tr>
    {}
    </table>"#,
        nav_tabs("validators"),
        html_escape(&v.name),
        html_escape(&v.name),
        html_escape(&v.address),
        html_escape(&v.address),
        status,
        v.blocks_produced,
        earned_srx,
        slot,
        html_escape(&fmt_ts(v.registered_at)),
        if v.last_block_time > 0 {
            html_escape(&fmt_ts(v.last_block_time))
        } else {
            "—".to_string()
        },
        block_rows,
    );

    page(&format!("Validator {}", &v.name), &body)
}

// ── Token detail ──────────────────────────────────────────
pub async fn explorer_token(
    State(state): State<SharedState>,
    Path(contract): Path<String>,
) -> Html<String> {
    let bc = state.read().await;
    let info = match bc.token_info(&contract) {
        Ok(i) => i,
        Err(_) => return page("Not Found", "<h2>Token not found</h2>"),
    };

    let holders_list = bc.get_token_holders(&contract).unwrap_or_default();
    let trades = bc.get_token_trades(&contract, 20, 0);

    // Top holders table (up to 20)
    let mut holder_rows = String::new();
    for (i, h) in holders_list.iter().take(20).enumerate() {
        let addr = h["address"].as_str().unwrap_or("");
        let bal = h["balance"].as_u64().unwrap_or(0);
        holder_rows.push_str(&format!(
            r#"<tr>
            <td style="color:#6b7280">#{}</td>
            <td class="mono"><a href="/explorer/address/{}">{}</a></td>
            <td>{}</td>
            </tr>"#,
            i + 1,
            html_escape(addr),
            html_escape(addr),
            bal,
        ));
    }

    // Recent transfers table
    let mut trade_rows = String::new();
    for t in &trades {
        let txid = t["txid"].as_str().unwrap_or("");
        let from = t["from"].as_str().unwrap_or("");
        let to = t["to"].as_str().unwrap_or("");
        let amt = t["amount"].as_u64().unwrap_or(0);
        let blk = t["block_index"].as_u64().unwrap_or(0);
        trade_rows.push_str(&format!(
            r#"<tr>
            <td class="hash"><a href="/explorer/tx/{}">{}</a></td>
            <td class="mono"><a href="/explorer/address/{}">{}</a></td>
            <td class="mono"><a href="/explorer/address/{}">{}</a></td>
            <td>{}</td>
            <td><a href="/explorer/block/{}">#{}</a></td>
            </tr>"#,
            html_escape(txid),
            html_escape(&truncate(txid, 16)),
            html_escape(from),
            html_escape(&truncate(from, 14)),
            html_escape(to),
            html_escape(&truncate(to, 14)),
            amt,
            blk,
            blk,
        ));
    }

    let owner = info["owner"].as_str().unwrap_or("");
    let body = format!(
        r#"
    {}
    <h2>Token: {} ({})</h2>
    <table class="detail-table">
    <tr><td>Name</td><td>{}</td></tr>
    <tr><td>Symbol</td><td><strong>{}</strong></td></tr>
    <tr><td>Contract</td><td class="hash">{}</td></tr>
    <tr><td>Total Supply</td><td>{}</td></tr>
    <tr><td>Decimals</td><td>{}</td></tr>
    <tr><td>Holders</td><td>{}</td></tr>
    <tr><td>Owner</td><td class="mono"><a href="/explorer/address/{}">{}</a></td></tr>
    </table>
    <h3>Top Holders</h3>
    {}
    <table>
    <tr><th>Rank</th><th>Address</th><th>Balance</th></tr>
    {}
    </table>
    <h3>Recent Transfers</h3>
    {}
    <table>
    <tr><th>TxID</th><th>From</th><th>To</th><th>Amount</th><th>Block</th></tr>
    {}
    </table>"#,
        nav_tabs("tokens"),
        html_escape(info["name"].as_str().unwrap_or("")),
        html_escape(info["symbol"].as_str().unwrap_or("")),
        html_escape(info["name"].as_str().unwrap_or("")),
        html_escape(info["symbol"].as_str().unwrap_or("")),
        html_escape(&contract),
        info["total_supply"],
        info["decimals"],
        info["holders"],
        html_escape(owner),
        html_escape(owner),
        if holder_rows.is_empty() {
            r#"<p style="color:#6b7280;padding:12px 0">No holders yet</p>"#
        } else {
            ""
        },
        holder_rows,
        if trade_rows.is_empty() {
            r#"<p style="color:#6b7280;padding:12px 0">No transfers yet</p>"#
        } else {
            ""
        },
        trade_rows,
    );

    page(
        &format!("Token {}", info["symbol"].as_str().unwrap_or("")),
        &body,
    )
}

// ── Mempool page ──────────────────────────────────────────
pub async fn explorer_mempool(State(state): State<SharedState>) -> Html<String> {
    let bc = state.read().await;
    let mempool: Vec<_> = bc.mempool.iter().collect();

    let content = if mempool.is_empty() {
        r#"<p style="color:#6b7280;padding:48px 0;text-align:center;font-size:16px">No pending transactions</p>"#.to_string()
    } else {
        let mut rows = String::new();
        for tx in &mempool {
            rows.push_str(&format!(
                r#"<tr>
                <td class="hash"><a href="/explorer/tx/{}">{}</a></td>
                <td class="mono">{}</td>
                <td class="mono">{}</td>
                <td>{:.8} SRX</td>
                <td>{} sentri</td>
                </tr>"#,
                html_escape(&tx.txid),
                html_escape(&truncate(&tx.txid, 16)),
                html_escape(&truncate(&tx.from_address, 16)),
                html_escape(&truncate(&tx.to_address, 16)),
                srx(tx.amount),
                tx.fee,
            ));
        }
        format!(
            r#"<table>
        <tr><th>TxID</th><th>From</th><th>To</th><th>Amount</th><th>Fee</th></tr>
        {rows}
        </table>"#
        )
    };

    let body = format!(
        r#"
    {}
    <h2>Mempool <span style="color:#6b7280;font-size:14px;font-weight:400">({} pending)</span></h2>
    {}
    <script>setTimeout(function(){{location.reload();}}, 5000);</script>"#,
        nav_tabs("mempool"),
        mempool.len(),
        content,
    );

    page("Mempool", &body)
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
        assert_eq!(html_escape("normal text"), "normal text");
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

    #[test]
    fn test_fmt_ts() {
        // A9: explorer now displays UTC, not WIB.
        // 2026-04-12 11:05:00 UTC, Unix: 1775991900.
        let s = fmt_ts(1775991900);
        assert!(s.contains("UTC"), "should contain UTC: {s}");
        assert!(s.contains("Apr"), "should contain Apr: {s}");
        assert!(s.contains("2026"), "should contain 2026: {s}");
        assert!(s.contains("12"), "should contain day 12: {s}");
        assert!(s.contains("11:05"), "should contain 11:05 UTC: {s}");
        // epoch 0 = 1 Jan 1970 00:00 UTC
        let epoch = fmt_ts(0);
        assert!(epoch.contains("UTC"), "epoch should contain UTC: {epoch}");
        assert!(epoch.contains("1970"), "epoch should contain 1970: {epoch}");
        assert!(
            epoch.contains("00:00"),
            "epoch should be 00:00 UTC: {epoch}"
        );
    }
}
