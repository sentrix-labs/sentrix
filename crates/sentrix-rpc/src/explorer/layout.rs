// All the page-shell bits — site CSS, the <html>/<head>/<header> wrapper
// every explorer route returns, the tabs bar, and the JS that draws the
// two homepage charts. None of it is per-page logic, so it sits here
// instead of cluttering each handler in explorer.rs.

use axum::response::Html;

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

pub(super) const CHART_SECTION: &str = r#"
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

pub(super) fn page(title: &str, body: &str) -> Html<String> {
    let chain_id = sentrix_core::blockchain::get_chain_id();
    Html(format!(
        r#"<!DOCTYPE html>
<html><head><meta charset="utf-8"><title>{title} — Sentrix Explorer</title>
<meta name="viewport" content="width=device-width, initial-scale=1">
<style>{CSS}</style></head><body>
<header><div class="container">
<h1>⬡ Sentrix Explorer</h1>
<span>Chain ID: {chain_id} &nbsp;|&nbsp; Sentrix</span>
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
</body></html>"#
    ))
}

pub(super) fn nav_tabs(active: &str) -> String {
    let tabs = [
        ("Home", "/explorer", "home"),
        ("Blocks", "/explorer/blocks", "blocks"),
        ("Transactions", "/explorer/transactions", "transactions"),
        ("Validators", "/explorer/validators", "validators"),
        ("Tokens", "/explorer/tokens", "tokens"),
        ("Rich List", "/explorer/richlist", "richlist"),
        ("Mempool", "/explorer/mempool", "mempool"),
    ];
    let mut html = String::from(r#"<div class="tabs">"#);
    for (label, href, key) in &tabs {
        let cls = if *key == active { "tab active" } else { "tab" };
        html.push_str(&format!(r#"<a class="{cls}" href="{href}">{label}</a>"#));
    }
    html.push_str("</div>");
    html
}
