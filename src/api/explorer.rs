// explorer.rs - Sentrix — Block Explorer Web UI

use axum::{
    extract::{State, Path},
    response::Html,
    Json,
};
use serde::Serialize;
use crate::api::routes::SharedState;
use std::sync::OnceLock;
use std::time::{Duration, Instant};
use tokio::sync::Mutex as TokioMutex;

struct CachedPage { html: String, at: Instant }
static HOME_CACHE: OnceLock<TokioMutex<Option<CachedPage>>> = OnceLock::new();
static RICHLIST_CACHE: OnceLock<TokioMutex<Option<CachedPage>>> = OnceLock::new();

#[derive(Serialize, Clone)]
pub struct DailyStat { pub date: String, pub blocks: u64, pub transactions: u64 }
struct DailyCache { data: Vec<DailyStat>, at: Instant }
static DAILY_CACHE: OnceLock<TokioMutex<Option<DailyCache>>> = OnceLock::new();

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

/// Known address labels
fn address_label(addr: &str) -> Option<&'static str> {
    match addr {
        a if a.starts_with("0x753f2f") => Some("Sentrix Foundation"),
        a if a.starts_with("0x0804a0") => Some("Sentrix Treasury"),
        a if a.starts_with("0xdd3cd7") => Some("Nusantara Node"),
        a if a.starts_with("0x7be6d0") => Some("BlockForge Asia"),
        a if a.starts_with("0x7dcc4f") => Some("PacificStake"),
        a if a.starts_with("0xd2116b") => Some("Archipelago Network"),
        a if a.starts_with("0xeb70fd") => Some("Ecosystem Fund"),
        _ => None,
    }
}

/// Render address with optional label badge
fn addr_with_label(addr: &str) -> String {
    match address_label(addr) {
        Some(label) => format!(
            r#"{} <span style="background:#1a2a1a;color:#4ade80;font-size:11px;padding:1px 6px;border-radius:4px;margin-left:4px">{}</span>"#,
            html_escape(addr), label
        ),
        None => html_escape(addr).to_string(),
    }
}

/// Format unix timestamp as "DD Mon YYYY, HH:MM WIB" (UTC+7)
fn fmt_ts(unix: u64) -> String {
    const WIB: u64 = 7 * 3600;
    let t = unix + WIB;
    let secs_in_day = t % 86400;
    let days_total  = t / 86400;                        // days since 1970-01-01

    let hh = secs_in_day / 3600;
    let mm = (secs_in_day % 3600) / 60;

    // Calendar calculation (Gregorian)
    let mut y = 1970u64;
    let mut d = days_total;
    loop {
        let dy = if (y.is_multiple_of(4) && !y.is_multiple_of(100)) || y.is_multiple_of(400) { 366 } else { 365 };
        if d < dy { break; }
        d -= dy;
        y += 1;
    }
    let leap = (y.is_multiple_of(4) && !y.is_multiple_of(100)) || y.is_multiple_of(400);
    let days_in_month = [31u64, if leap { 29 } else { 28 }, 31, 30, 31, 30, 31, 31, 30, 31, 30, 31];
    let mut m = 0usize;
    for &dim in &days_in_month {
        if d < dim { break; }
        d -= dim;
        m += 1;
    }
    let month = ["Jan","Feb","Mar","Apr","May","Jun","Jul","Aug","Sep","Oct","Nov","Dec"][m];
    format!("{} {} {}, {:02}:{:02} WIB", d + 1, month, y, hh, mm)
}

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
.stat-card .sub { color: #4b5563; font-size: 11px; margin-top: 2px; }
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
.search-bar { display: flex; gap: 8px; margin-top: 14px; }
.search-bar input { flex: 1; padding: 9px 14px; border-radius: 8px; background: #1a1f35; border: 1px solid #2a3050; color: #e1e5ee; font-size: 14px; outline: none; }
.search-bar input:focus { border-color: #7c8aff; }
.search-bar button { padding: 9px 18px; border-radius: 8px; background: #1e3a5f; color: #60a5fa; border: 1px solid #3b82f6; font-size: 14px; cursor: pointer; }
.search-bar button:hover { background: #2a4f7f; }
.search-error { color: #f87171; font-size: 13px; margin-top: 6px; min-height: 18px; }
.charts { display: grid; grid-template-columns: 1fr 1fr; gap: 16px; margin: 24px 0; }
.chart-card { background: #111827; border: 1px solid #1f2937; border-radius: 12px; padding: 16px; }
.chart-title { color: #9ca3af; font-size: 13px; text-transform: uppercase; letter-spacing: .05em; margin-bottom: 12px; }
canvas { display: block; max-width: 100%; }
@media (max-width: 700px) { .charts { grid-template-columns: 1fr; } }
"#;

/// Format day_key (days since 1970-01-01) as "dd/mm"
fn fmt_day(day_key: u64) -> String {
    let mut d = day_key;
    let mut y = 1970u64;
    loop {
        let dy = if (y.is_multiple_of(4) && !y.is_multiple_of(100)) || y.is_multiple_of(400) { 366 } else { 365 };
        if d < dy { break; }
        d -= dy;
        y += 1;
    }
    let leap = (y.is_multiple_of(4) && !y.is_multiple_of(100)) || y.is_multiple_of(400);
    let dims = [31u64, if leap { 29 } else { 28 }, 31, 30, 31, 30, 31, 31, 30, 31, 30, 31];
    let mut m = 0usize;
    for &dim in &dims {
        if d < dim { break; }
        d -= dim;
        m += 1;
    }
    format!("{:02}/{:02}", d + 1, m + 1)
}

const CHART_SECTION: &str = r#"
<div class="charts">
  <div class="chart-card">
    <div class="chart-title">Daily Transactions — last 14 days</div>
    <canvas id="chart-tx"></canvas>
  </div>
  <div class="chart-card">
    <div class="chart-title">Daily Blocks — last 14 days</div>
    <canvas id="chart-blk"></canvas>
  </div>
</div>
<script>
(function(){
  var c1=document.getElementById('chart-tx');
  var c2=document.getElementById('chart-blk');
  if(!c1||!c2)return;
  function sz(c){c.width=c.parentElement.clientWidth-32;c.height=200;}
  function drawLine(c,data,key,color){
    sz(c);
    var w=c.width,h=c.height,ctx=c.getContext('2d');
    var vals=data.map(function(d){return+(d[key])||0;});
    var labs=data.map(function(d){return d.date;});
    var mv=Math.max.apply(null,vals)||1;
    var p={t:20,r:16,b:36,l:52};
    var iw=w-p.l-p.r,ih=h-p.t-p.b,n=vals.length;
    var st=n>1?iw/(n-1):iw;
    ctx.fillStyle='#111827';ctx.fillRect(0,0,w,h);
    for(var g=0;g<=4;g++){
      var gy=p.t+ih*(1-g/4);
      ctx.strokeStyle='#1f2937';ctx.lineWidth=1;
      ctx.beginPath();ctx.moveTo(p.l,gy);ctx.lineTo(w-p.r,gy);ctx.stroke();
      ctx.fillStyle='#6b7280';ctx.font='10px Segoe UI';ctx.textAlign='right';
      ctx.fillText(Math.round(mv*g/4),p.l-4,gy+4);
    }
    ctx.fillStyle='#6b7280';ctx.font='10px Segoe UI';ctx.textAlign='center';
    for(var li=0;li<n;li++)if(li%2===0||n<=7)ctx.fillText(labs[li],p.l+li*st,h-p.b+14);
    ctx.fillStyle=color+'22';ctx.beginPath();ctx.moveTo(p.l,p.t+ih);
    for(var fi=0;fi<n;fi++)ctx.lineTo(p.l+fi*st,p.t+ih*(1-vals[fi]/mv));
    ctx.lineTo(p.l+(n-1)*st,p.t+ih);ctx.closePath();ctx.fill();
    ctx.strokeStyle=color;ctx.lineWidth=2;ctx.beginPath();
    for(var pi=0;pi<n;pi++){var px=p.l+pi*st,py=p.t+ih*(1-vals[pi]/mv);if(pi===0)ctx.moveTo(px,py);else ctx.lineTo(px,py);}
    ctx.stroke();
    ctx.fillStyle=color;
    for(var di=0;di<n;di++){ctx.beginPath();ctx.arc(p.l+di*st,p.t+ih*(1-vals[di]/mv),3,0,Math.PI*2);ctx.fill();}
  }
  function drawBar(c,data,key,color){
    sz(c);
    var w=c.width,h=c.height,ctx=c.getContext('2d');
    var vals=data.map(function(d){return+(d[key])||0;});
    var labs=data.map(function(d){return d.date;});
    var mv=Math.max.apply(null,vals)||1;
    var p={t:20,r:16,b:36,l:52};
    var iw=w-p.l-p.r,ih=h-p.t-p.b,n=vals.length;
    var gap=iw/n,bw=gap*0.65;
    ctx.fillStyle='#111827';ctx.fillRect(0,0,w,h);
    for(var g=0;g<=4;g++){
      var gy=p.t+ih*(1-g/4);
      ctx.strokeStyle='#1f2937';ctx.lineWidth=1;
      ctx.beginPath();ctx.moveTo(p.l,gy);ctx.lineTo(w-p.r,gy);ctx.stroke();
      ctx.fillStyle='#6b7280';ctx.font='10px Segoe UI';ctx.textAlign='right';
      ctx.fillText(Math.round(mv*g/4),p.l-4,gy+4);
    }
    for(var bi=0;bi<n;bi++){
      var bh=ih*vals[bi]/mv;
      ctx.fillStyle=color+'bb';ctx.fillRect(p.l+bi*gap+(gap-bw)/2,p.t+ih-bh,bw,bh);
      if(bi%2===0||n<=7){ctx.fillStyle='#6b7280';ctx.font='10px Segoe UI';ctx.textAlign='center';ctx.fillText(labs[bi],p.l+bi*gap+gap/2,h-p.b+14);}
    }
  }
  fetch('/stats/daily').then(function(r){return r.json();}).then(function(data){
    drawLine(c1,data,'transactions','#3b82f6');
    drawBar(c2,data,'blocks','#f59e0b');
  }).catch(function(){});
})();
</script>"#;

fn page(title: &str, body: &str) -> Html<String> {
    Html(format!(r#"<!DOCTYPE html>
<html><head><meta charset="utf-8"><title>{title} — Sentrix Explorer</title>
<meta name="viewport" content="width=device-width, initial-scale=1">
<style>{CSS}</style></head><body>
<header><div class="container">
<h1>⬡ Sentrix Explorer</h1>
<span>Chain ID: 7119 &nbsp;|&nbsp; PoA Blockchain</span>
<div class="search-bar">
  <input id="srx-search" type="text" placeholder="Search by TX hash, block height, or address" autocomplete="off" />
  <button onclick="srxSearch()">Search</button>
</div>
<div class="search-error" id="srx-search-err"></div>
<script>
function srxSearch(){{
  var q=(document.getElementById('srx-search').value||'').trim();
  var err=document.getElementById('srx-search-err');
  err.textContent='';
  if(!q){{err.textContent='Please enter a search term.';return;}}
  if(/^[0-9]+$/.test(q)){{window.location='/explorer/block/'+q;return;}}
  if(/^(0x)?[0-9a-fA-F]{{64}}$/.test(q)){{window.location='/explorer/tx/'+q.replace(/^0x/,'');return;}}
  if(/^0x[0-9a-fA-F]{{40}}$/.test(q)){{window.location='/explorer/address/'+q;return;}}
  err.textContent='Unrecognized format. Enter a block height (number), TX hash (64 hex), or address (0x + 40 hex).';
}}
document.getElementById('srx-search').addEventListener('keydown',function(e){{if(e.key==='Enter')srxSearch();}});
</script>
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
        ("Rich List",    "/explorer/richlist",     "richlist"),
        ("Mempool",      "/explorer/mempool",      "mempool"),
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
    const HOME_TTL: Duration = Duration::from_secs(10);
    let cache = HOME_CACHE.get_or_init(|| TokioMutex::new(None));
    {
        let guard = cache.lock().await;
        if let Some(ref c) = *guard && c.at.elapsed() < HOME_TTL {
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
        if let Some(block) = bc.get_block(i) {
            let non_cb = block.transactions.iter().filter(|t| !t.is_coinbase()).count() as u64;
            total_regular_txs += non_cb;
            if i >= sample_start {
                sample_regular_txs += non_cb;
                if i == sample_start { sample_oldest_ts = block.timestamp; }
                if i == height       { sample_newest_ts = block.timestamp; }
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
                html_escape(&fmt_ts(block.timestamp)),
                block.tx_count(),
                html_escape(&block.validator), html_escape(&truncate(&block.validator, 20)),
            ));
        }
    }

    let body = format!(r#"
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
        let mut guard = HOME_CACHE.get_or_init(|| TokioMutex::new(None)).lock().await;
        *guard = Some(CachedPage { html: result.0.clone(), at: Instant::now() });
    }
    result
}

// ── Daily stats endpoint ─────────────────────────────────
pub async fn stats_daily(State(state): State<SharedState>) -> Json<Vec<DailyStat>> {
    const TTL: Duration = Duration::from_secs(300); // 5 min cache
    let cache = DAILY_CACHE.get_or_init(|| TokioMutex::new(None));
    {
        let guard = cache.lock().await;
        if let Some(ref c) = *guard && c.at.elapsed() < TTL {
            return Json(c.data.clone());
        }
    }

    let bc = state.read().await;
    let height = bc.height();
    const WIB: u64 = 7 * 3600;

    let today_day = bc.get_block(height)
        .map(|b| (b.timestamp + WIB) / 86400)
        .unwrap_or(0);

    let mut map: std::collections::HashMap<u64, (u64, u64)> = std::collections::HashMap::new();

    if today_day > 0 {
        let earliest = today_day.saturating_sub(13);
        for i in 0..=height {
            if let Some(block) = bc.get_block(i) {
                let day = (block.timestamp + WIB) / 86400;
                if day >= earliest && day <= today_day {
                    let e = map.entry(day).or_insert((0, 0));
                    e.0 += 1;
                    e.1 += block.transactions.iter().filter(|t| !t.is_coinbase()).count() as u64;
                }
            }
        }
    }

    let earliest = today_day.saturating_sub(13);
    let mut result: Vec<DailyStat> = (0..14u64).map(|i| {
        let day = earliest + i;
        let (blocks, txs) = map.get(&day).copied().unwrap_or((0, 0));
        DailyStat { date: fmt_day(day), blocks, transactions: txs }
    }).collect();

    // If chain has no blocks yet, fill with placeholder dates from system time
    if today_day == 0 {
        use std::time::{SystemTime, UNIX_EPOCH};
        let now_day = SystemTime::now().duration_since(UNIX_EPOCH)
            .map(|d| (d.as_secs() + WIB) / 86400).unwrap_or(0);
        result = (0..14u64).map(|i| DailyStat {
            date: fmt_day(now_day.saturating_sub(13) + i),
            blocks: 0, transactions: 0,
        }).collect();
    }

    let mut guard = cache.lock().await;
    *guard = Some(DailyCache { data: result.clone(), at: Instant::now() });
    Json(result)
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
                html_escape(&fmt_ts(block.timestamp)),
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
    // Scan only last 1000 blocks for performance
    let height = bc.height();
    let scan_start = height.saturating_sub(999);

    let mut coinbase_rows = String::new();
    let mut regular_rows = String::new();

    for i in (scan_start..=height).rev() {
        if let Some(block) = bc.get_block(i) {
            for tx in &block.transactions {
                let is_cb = tx.is_coinbase();
                let from_disp = if tx.from_address == "COINBASE" {
                    "COINBASE".to_string()
                } else {
                    html_escape(&truncate(&tx.from_address, 14))
                };
                let to_disp = html_escape(&truncate(&tx.to_address, 14));

                let row = format!(
                    r#"<tr>
                    <td class="hash"><a href="/explorer/tx/{}">{}</a></td>
                    <td class="mono">{}</td>
                    <td class="mono">{}</td>
                    <td>{:.8} SRX</td>
                    <td>{} sentri</td>
                    <td><a href="/explorer/block/{}">#{}</a></td>
                    <td>{}</td>
                    </tr>"#,
                    html_escape(&tx.txid), html_escape(&truncate(&tx.txid, 16)),
                    from_disp, to_disp,
                    srx(tx.amount), tx.fee,
                    block.index, block.index,
                    html_escape(&fmt_ts(block.timestamp)),
                );

                if is_cb { coinbase_rows.push_str(&row); }
                else { regular_rows.push_str(&row); }
            }
        }
    }

    let regular_content = if regular_rows.is_empty() {
        r#"<p style="color:#6b7280;padding:24px 0;text-align:center">No regular transactions yet</p>"#.to_string()
    } else {
        format!(r#"<table>
        <tr><th>TxID</th><th>From</th><th>To</th><th>Amount</th><th>Fee</th><th>Block</th><th>Time (WIB)</th></tr>
        {regular_rows}
        </table>"#)
    };

    let coinbase_content = format!(r#"<table>
    <tr><th>TxID</th><th>From</th><th>To (Validator)</th><th>Amount</th><th>Fee</th><th>Block</th><th>Time (WIB)</th></tr>
    {coinbase_rows}
    </table>"#);

    let body = format!(r#"
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
                    <td>{:.8} SRX</td>
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
                html_escape(&fmt_ts(block.timestamp)),
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
            <td>{:.8} SRX</td>
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
        addr_with_label(&address),
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
                html_escape(&fmt_ts(tx["timestamp"].as_u64().unwrap_or(0))),
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
            html_escape(&v.address), html_escape(&v.name),
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
            <td><a href="/explorer/token/{}"><strong>{}</strong></a></td>
            <td>{}</td>
            <td class="hash"><a href="/explorer/token/{}">{}</a></td>
            <td>{}</td>
            <td>{}</td>
            <td class="mono"><a href="/explorer/address/{}">{}</a></td>
            </tr>"#,
            html_escape(contract), html_escape(t["symbol"].as_str().unwrap_or("")),
            html_escape(t["name"].as_str().unwrap_or("")),
            html_escape(contract), html_escape(&truncate(contract, 24)),
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

// ── Rich List ─────────────────────────────────────────────
pub async fn explorer_richlist(State(state): State<SharedState>) -> Html<String> {
    const TOTAL_SUPPLY: f64 = 210_000_000.0; // SRX
    const RICHLIST_TTL: Duration = Duration::from_secs(30);
    let cache = RICHLIST_CACHE.get_or_init(|| TokioMutex::new(None));
    {
        let guard = cache.lock().await;
        if let Some(ref c) = *guard && c.at.elapsed() < RICHLIST_TTL {
            return Html(c.html.clone());
        }
    }
    let bc = state.read().await;

    // Collect all non-zero balances, sort descending
    let mut holders: Vec<(&String, u64)> = bc.accounts.accounts
        .iter()
        .filter(|(_, a)| a.balance > 0)
        .map(|(addr, a)| (addr, a.balance))
        .collect();
    holders.sort_by(|a, b| b.1.cmp(&a.1));
    let holders = &holders[..holders.len().min(50)];

    let mut rows = String::new();
    for (rank, (address, balance_sentri)) in holders.iter().enumerate() {
        let balance_srx = *balance_sentri as f64 / 100_000_000.0;
        let pct = balance_srx / TOTAL_SUPPLY * 100.0;
        rows.push_str(&format!(
            r#"<tr>
            <td style="color:#6b7280">#{}</td>
            <td class="mono"><a href="/explorer/address/{}">{}</a></td>
            <td>{:.8} SRX</td>
            <td style="color:#9ca3af">{:.6}%</td>
            </tr>"#,
            rank + 1,
            html_escape(address), addr_with_label(address),
            balance_srx,
            pct,
        ));
    }

    let empty = if rows.is_empty() {
        r#"<p style="color:#6b7280;padding:24px 0;text-align:center">No accounts found</p>"#
    } else { "" };

    let body = format!(r#"
    {}
    <h2>Rich List — Top SRX Holders</h2>
    <p style="color:#6b7280;font-size:13px;margin-bottom:16px">Top 50 addresses by SRX balance &nbsp;|&nbsp; Total supply: 210,000,000 SRX</p>
    {}
    <table>
    <tr><th>Rank</th><th>Address</th><th>Balance</th><th>% of Supply</th></tr>
    {}
    </table>"#,
        nav_tabs("richlist"),
        empty,
        rows,
    );

    let result = page("Rich List", &body);
    {
        let mut guard = RICHLIST_CACHE.get_or_init(|| TokioMutex::new(None)).lock().await;
        *guard = Some(CachedPage { html: result.0.clone(), at: Instant::now() });
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
    let slot = active_validators.iter().position(|av| av.address == v.address)
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
                block.index, block.index,
                block.index, html_escape(&truncate(&block.hash, 20)),
                html_escape(&fmt_ts(block.timestamp)),
                block.tx_count(),
            ));
            found += 1;
            if found >= 20 { break; }
        }
    }

    let body = format!(r#"
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
        html_escape(&v.address), html_escape(&v.address),
        status,
        v.blocks_produced,
        earned_srx,
        slot,
        html_escape(&fmt_ts(v.registered_at)),
        if v.last_block_time > 0 { html_escape(&fmt_ts(v.last_block_time)) } else { "—".to_string() },
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
        let bal  = h["balance"].as_u64().unwrap_or(0);
        holder_rows.push_str(&format!(
            r#"<tr>
            <td style="color:#6b7280">#{}</td>
            <td class="mono"><a href="/explorer/address/{}">{}</a></td>
            <td>{}</td>
            </tr>"#,
            i + 1,
            html_escape(addr), html_escape(addr),
            bal,
        ));
    }

    // Recent transfers table
    let mut trade_rows = String::new();
    for t in &trades {
        let txid = t["txid"].as_str().unwrap_or("");
        let from = t["from"].as_str().unwrap_or("");
        let to   = t["to"].as_str().unwrap_or("");
        let amt  = t["amount"].as_u64().unwrap_or(0);
        let blk  = t["block_index"].as_u64().unwrap_or(0);
        trade_rows.push_str(&format!(
            r#"<tr>
            <td class="hash"><a href="/explorer/tx/{}">{}</a></td>
            <td class="mono"><a href="/explorer/address/{}">{}</a></td>
            <td class="mono"><a href="/explorer/address/{}">{}</a></td>
            <td>{}</td>
            <td><a href="/explorer/block/{}">#{}</a></td>
            </tr>"#,
            html_escape(txid), html_escape(&truncate(txid, 16)),
            html_escape(from), html_escape(&truncate(from, 14)),
            html_escape(to),   html_escape(&truncate(to,   14)),
            amt,
            blk, blk,
        ));
    }

    let owner = info["owner"].as_str().unwrap_or("");
    let body = format!(r#"
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
        html_escape(owner), html_escape(owner),
        if holder_rows.is_empty() { r#"<p style="color:#6b7280;padding:12px 0">No holders yet</p>"# } else { "" },
        holder_rows,
        if trade_rows.is_empty() { r#"<p style="color:#6b7280;padding:12px 0">No transfers yet</p>"# } else { "" },
        trade_rows,
    );

    page(&format!("Token {}", info["symbol"].as_str().unwrap_or("")), &body)
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
                html_escape(&tx.txid), html_escape(&truncate(&tx.txid, 16)),
                html_escape(&truncate(&tx.from_address, 16)),
                html_escape(&truncate(&tx.to_address,   16)),
                srx(tx.amount), tx.fee,
            ));
        }
        format!(r#"<table>
        <tr><th>TxID</th><th>From</th><th>To</th><th>Amount</th><th>Fee</th></tr>
        {rows}
        </table>"#)
    };

    let body = format!(r#"
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

    #[test]
    fn test_fmt_ts() {
        // 2026-04-12 11:05:00 UTC = 2026-04-12 18:05:00 WIB
        // Unix: 1775991900 (from actual chain block timestamps)
        let s = fmt_ts(1775991900);
        assert!(s.contains("WIB"), "should contain WIB: {s}");
        assert!(s.contains("Apr"), "should contain Apr: {s}");
        assert!(s.contains("2026"), "should contain 2026: {s}");
        assert!(s.contains("12"), "should contain day 12: {s}");
        assert!(s.contains("18:05"), "should contain 18:05 WIB: {s}");
        // epoch 0 = 1 Jan 1970 00:00 UTC = 07:00 WIB
        let epoch = fmt_ts(0);
        assert!(epoch.contains("WIB"), "epoch should contain WIB: {epoch}");
        assert!(epoch.contains("1970"), "epoch should contain 1970: {epoch}");
    }
}
