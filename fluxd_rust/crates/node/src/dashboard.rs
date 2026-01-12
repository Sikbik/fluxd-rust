use std::collections::BTreeMap;
use std::net::SocketAddr;
use std::sync::{Arc, Mutex};
use std::time::{Instant, SystemTime, UNIX_EPOCH};

use fluxd_chainstate::metrics::ConnectMetrics;
use fluxd_chainstate::state::ChainState;
use fluxd_consensus::params::Network;
use fluxd_storage::KeyValueStore;
use tokio::io::{AsyncReadExt, AsyncWriteExt};
use tokio::net::TcpListener;

use fluxd_chainstate::validation::ValidationMetrics;

use crate::p2p::{NetTotals, PeerKind, PeerRegistry};
use crate::stats::{snapshot_stats, HeaderMetrics, SyncMetrics};
use crate::Backend;
use crate::Store;
use crate::{mempool::Mempool, stats::MempoolMetrics};
use serde_json;

const MAX_REQUEST_BYTES: usize = 8192;

#[allow(clippy::too_many_arguments)]
pub async fn serve_dashboard<S: KeyValueStore + Send + Sync + 'static>(
    addr: SocketAddr,
    chainstate: Arc<ChainState<S>>,
    store: Arc<Store>,
    metrics: Arc<SyncMetrics>,
    header_metrics: Arc<HeaderMetrics>,
    validation_metrics: Arc<ValidationMetrics>,
    connect_metrics: Arc<ConnectMetrics>,
    mempool: Arc<Mutex<Mempool>>,
    mempool_metrics: Arc<MempoolMetrics>,
    net_totals: Arc<NetTotals>,
    peer_registry: Arc<PeerRegistry>,
    network: Network,
    backend: Backend,
    start_time: Instant,
) -> Result<(), String> {
    let listener = TcpListener::bind(addr)
        .await
        .map_err(|err| format!("dashboard bind failed: {err}"))?;
    log_info!("Dashboard listening on http://{addr}");

    loop {
        let (stream, _) = listener
            .accept()
            .await
            .map_err(|err| format!("dashboard accept failed: {err}"))?;
        let chainstate = Arc::clone(&chainstate);
        let store = Arc::clone(&store);
        let metrics = Arc::clone(&metrics);
        let header_metrics = Arc::clone(&header_metrics);
        let validation_metrics = Arc::clone(&validation_metrics);
        let connect_metrics = Arc::clone(&connect_metrics);
        let mempool = Arc::clone(&mempool);
        let mempool_metrics = Arc::clone(&mempool_metrics);
        let net_totals = Arc::clone(&net_totals);
        let peer_registry = Arc::clone(&peer_registry);
        tokio::spawn(async move {
            if let Err(err) = handle_connection(
                stream,
                chainstate,
                store,
                metrics,
                header_metrics,
                validation_metrics,
                connect_metrics,
                mempool,
                mempool_metrics,
                net_totals,
                peer_registry,
                network,
                backend,
                start_time,
            )
            .await
            {
                log_warn!("dashboard error: {err}");
            }
        });
    }
}

#[allow(clippy::too_many_arguments)]
async fn handle_connection<S: KeyValueStore + Send + Sync + 'static>(
    mut stream: tokio::net::TcpStream,
    chainstate: Arc<ChainState<S>>,
    store: Arc<Store>,
    metrics: Arc<SyncMetrics>,
    header_metrics: Arc<HeaderMetrics>,
    validation_metrics: Arc<ValidationMetrics>,
    connect_metrics: Arc<ConnectMetrics>,
    mempool: Arc<Mutex<Mempool>>,
    mempool_metrics: Arc<MempoolMetrics>,
    net_totals: Arc<NetTotals>,
    peer_registry: Arc<PeerRegistry>,
    network: Network,
    backend: Backend,
    start_time: Instant,
) -> Result<(), String> {
    let mut buffer = vec![0u8; MAX_REQUEST_BYTES];
    let bytes_read = stream
        .read(&mut buffer)
        .await
        .map_err(|err| err.to_string())?;
    if bytes_read == 0 {
        return Ok(());
    }

    let request = String::from_utf8_lossy(&buffer[..bytes_read]);
    let request_line = request.lines().next().unwrap_or_default();
    let mut parts = request_line.split_whitespace();
    let method = parts.next().unwrap_or_default();
    let path = parts.next().unwrap_or("/");
    let path = path.split('?').next().unwrap_or(path);

    let (status, content_type, body) = match (method, path) {
        ("GET", "/") | ("GET", "/index.html") => {
            ("200 OK", "text/html; charset=utf-8", dashboard_html())
        }
        ("GET", "/stats") => match snapshot_stats(
            &chainstate,
            Some(store.as_ref()),
            network,
            backend,
            start_time,
            Some(&metrics),
            Some(&header_metrics),
            Some(&validation_metrics),
            Some(&connect_metrics),
            Some(mempool.as_ref()),
            Some(mempool_metrics.as_ref()),
        ) {
            Ok(stats) => ("200 OK", "application/json", stats.to_json()),
            Err(err) => (
                "500 Internal Server Error",
                "text/plain; charset=utf-8",
                format!("stats error: {err}"),
            ),
        },
        ("GET", "/metrics") => match snapshot_stats(
            &chainstate,
            Some(store.as_ref()),
            network,
            backend,
            start_time,
            Some(&metrics),
            Some(&header_metrics),
            Some(&validation_metrics),
            Some(&connect_metrics),
            Some(mempool.as_ref()),
            Some(mempool_metrics.as_ref()),
        ) {
            Ok(stats) => (
                "200 OK",
                "text/plain; version=0.0.4; charset=utf-8",
                stats.to_prometheus(),
            ),
            Err(err) => (
                "500 Internal Server Error",
                "text/plain; charset=utf-8",
                format!("metrics error: {err}"),
            ),
        },
        ("GET", "/nettotals") => {
            #[derive(serde::Serialize)]
            struct NetTotalsView {
                bytes_recv: u64,
                bytes_sent: u64,
                connections: usize,
            }
            let totals = net_totals.snapshot();
            match serde_json::to_string(&NetTotalsView {
                bytes_recv: totals.bytes_recv,
                bytes_sent: totals.bytes_sent,
                connections: totals.connections,
            }) {
                Ok(body) => ("200 OK", "application/json", body),
                Err(err) => (
                    "500 Internal Server Error",
                    "text/plain; charset=utf-8",
                    format!("nettotals error: {err}"),
                ),
            }
        }
        ("GET", "/peers") => {
            #[derive(serde::Serialize)]
            struct PeerInfoView {
                addr: String,
                kind: String,
                inbound: bool,
                version: i32,
                start_height: i32,
                user_agent: String,
            }
            let peers = peer_registry.snapshot();
            let view = peers
                .into_iter()
                .map(|peer| PeerInfoView {
                    addr: peer.addr.to_string(),
                    kind: match peer.kind {
                        PeerKind::Block => "block",
                        PeerKind::Header => "header",
                        PeerKind::Relay => "relay",
                    }
                    .to_string(),
                    inbound: peer.inbound,
                    version: peer.version,
                    start_height: peer.start_height,
                    user_agent: peer.user_agent,
                })
                .collect::<Vec<_>>();
            match serde_json::to_string(&view) {
                Ok(body) => ("200 OK", "application/json", body),
                Err(err) => (
                    "500 Internal Server Error",
                    "text/plain; charset=utf-8",
                    format!("peers error: {err}"),
                ),
            }
        }
        ("GET", "/mempool") => match mempool.lock() {
            Err(_) => (
                "500 Internal Server Error",
                "text/plain; charset=utf-8",
                "mempool lock poisoned".to_string(),
            ),
            Ok(guard) => {
                #[derive(serde::Serialize)]
                struct VersionCount {
                    version: i32,
                    count: u64,
                }

                #[derive(serde::Serialize)]
                struct AgeSecs {
                    newest_secs: u64,
                    median_secs: u64,
                    oldest_secs: u64,
                }

                #[derive(serde::Serialize)]
                struct MempoolSummaryView {
                    size: u64,
                    bytes: u64,
                    fee_zero: u64,
                    fee_nonzero: u64,
                    versions: Vec<VersionCount>,
                    age_secs: AgeSecs,
                }

                let now = SystemTime::now()
                    .duration_since(UNIX_EPOCH)
                    .unwrap_or_default()
                    .as_secs();

                let mut fee_zero = 0u64;
                let mut fee_nonzero = 0u64;
                let mut versions: BTreeMap<i32, u64> = BTreeMap::new();
                let mut ages: Vec<u64> = Vec::with_capacity(guard.size());

                for entry in guard.entries() {
                    if entry.fee == 0 {
                        fee_zero = fee_zero.saturating_add(1);
                    } else {
                        fee_nonzero = fee_nonzero.saturating_add(1);
                    }
                    *versions.entry(entry.tx.version).or_insert(0) += 1;
                    ages.push(now.saturating_sub(entry.time));
                }

                ages.sort_unstable();
                let newest_secs = ages.first().copied().unwrap_or(0);
                let oldest_secs = ages.last().copied().unwrap_or(0);
                let median_secs = ages.get(ages.len() / 2).copied().unwrap_or(0);
                let versions = versions
                    .into_iter()
                    .map(|(version, count)| VersionCount { version, count })
                    .collect::<Vec<_>>();

                match serde_json::to_string(&MempoolSummaryView {
                    size: guard.size() as u64,
                    bytes: guard.bytes() as u64,
                    fee_zero,
                    fee_nonzero,
                    versions,
                    age_secs: AgeSecs {
                        newest_secs,
                        median_secs,
                        oldest_secs,
                    },
                }) {
                    Ok(body) => ("200 OK", "application/json", body),
                    Err(err) => (
                        "500 Internal Server Error",
                        "text/plain; charset=utf-8",
                        format!("mempool error: {err}"),
                    ),
                }
            }
        },
        ("GET", "/healthz") => ("200 OK", "text/plain; charset=utf-8", "ok".to_string()),
        _ => (
            "404 Not Found",
            "text/plain; charset=utf-8",
            "not found".to_string(),
        ),
    };

    let response = build_response(status, content_type, &body);
    stream
        .write_all(&response)
        .await
        .map_err(|err| err.to_string())?;
    stream.shutdown().await.map_err(|err| err.to_string())?;
    Ok(())
}

fn build_response(status: &str, content_type: &str, body: &str) -> Vec<u8> {
    let mut response = String::new();
    response.push_str("HTTP/1.1 ");
    response.push_str(status);
    response.push_str("\r\nContent-Type: ");
    response.push_str(content_type);
    response.push_str("\r\nCache-Control: no-store\r\nConnection: close\r\nContent-Length: ");
    response.push_str(&body.len().to_string());
    response.push_str("\r\n\r\n");
    let mut bytes = response.into_bytes();
    bytes.extend_from_slice(body.as_bytes());
    bytes
}

fn dashboard_html() -> String {
    DASHBOARD_HTML.to_string()
}

const DASHBOARD_HTML: &str = r#"<!doctype html>
<html lang="en">
  <head>
    <meta charset="utf-8">
    <meta name="viewport" content="width=device-width, initial-scale=1">
    <title>Fluxd Node Dashboard</title>
    <link href="https://fonts.googleapis.com/css2?family=Inter:wght@300;400;600;700&display=swap" rel="stylesheet">
    <style>
      :root {
        --bg-color: #0b0e14;
        --card-bg: rgba(20, 25, 35, 0.6);
        --card-border: rgba(255, 255, 255, 0.08);
        --text-main: #e2e8f0;
        --text-muted: #94a3b8;
        --accent-primary: #2d7ff9; /* RunOnFlux Blue */
        --accent-glow: rgba(45, 127, 249, 0.4);
        --success: #10b981;
        --gradient-start: #0f172a;
        --gradient-end: #020617;
      }

      * {
        box-sizing: border-box;
      }

      body {
        margin: 0;
        font-family: 'Inter', system-ui, -apple-system, sans-serif;
        background: radial-gradient(circle at top center, #1e293b 0%, var(--bg-color) 40%);
        background-color: var(--bg-color);
        color: var(--text-main);
        min-height: 100vh;
        -webkit-font-smoothing: antialiased;
      }

      main {
        max-width: 1200px;
        margin: 0 auto;
        padding: 40px 20px;
      }

      header {
        display: flex;
        justify-content: space-between;
        align-items: center;
        margin-bottom: 60px;
        position: relative;
      }

      .brand {
        display: flex;
        flex-direction: column;
      }

      h1 {
        font-size: 32px;
        font-weight: 700;
        letter-spacing: -0.03em;
        margin: 0;
        background: linear-gradient(135deg, #fff 0%, #94a3b8 100%);
        -webkit-background-clip: text;
        -webkit-text-fill-color: transparent;
      }

      .subtitle {
        font-size: 14px;
        color: var(--accent-primary);
        font-weight: 600;
        letter-spacing: 0.05em;
        text-transform: uppercase;
        margin-bottom: 4px;
      }

      .status-pill {
        background: rgba(16, 185, 129, 0.1);
        border: 1px solid rgba(16, 185, 129, 0.2);
        color: var(--success);
        padding: 6px 16px;
        border-radius: 99px;
        font-size: 13px;
        font-weight: 500;
        display: flex;
        align-items: center;
        gap: 8px;
        backdrop-filter: blur(4px);
      }

      .status-pill.syncing {
        background: rgba(45, 127, 249, 0.1);
        border: 1px solid rgba(45, 127, 249, 0.2);
        color: var(--accent-primary);
      }

      .status-dot {
        width: 8px;
        height: 8px;
        border-radius: 50%;
        background: currentColor;
        box-shadow: 0 0 10px currentColor;
        animation: pulse 2s infinite;
      }

      .grid {
        display: grid;
        grid-template-columns: repeat(auto-fill, minmax(240px, 1fr));
        gap: 24px;
      }

      .card {
        background: var(--card-bg);
        border: 1px solid var(--card-border);
        border-radius: 16px;
        padding: 24px;
        transition: transform 0.2s, box-shadow 0.2s;
        backdrop-filter: blur(12px);
      }

      .card:hover {
        transform: translateY(-2px);
        box-shadow: 0 8px 30px rgba(0,0,0,0.2);
        border-color: rgba(255,255,255,0.15);
      }

      .card.wide {
        grid-column: span 2;
      }

      .label {
        font-size: 12px;
        font-weight: 600;
        color: var(--text-muted);
        text-transform: uppercase;
        letter-spacing: 0.05em;
        margin-bottom: 12px;
      }

      .value {
        font-size: 24px;
        font-weight: 600;
        color: #fff;
        font-feature-settings: "tnum";
      }

      .value.mono {
        font-family: 'SF Mono', 'Monaco', 'Inconsolata', monospace;
        font-size: 15px;
        color: var(--accent-primary);
        word-break: break-all;
        line-height: 1.4;
      }

      .footer {
        margin-top: 60px;
        padding-top: 20px;
        border-top: 1px solid var(--card-border);
        display: flex;
        justify-content: space-between;
        color: var(--text-muted);
        font-size: 13px;
      }

      @keyframes pulse {
        0% { opacity: 1; transform: scale(1); }
        50% { opacity: 0.5; transform: scale(0.8); }
        100% { opacity: 1; transform: scale(1); }
      }

      @media (max-width: 768px) {
        .card.wide {
          grid-column: span 1;
        }
      }
    </style>
  </head>
  <body>
    <main>
      <header>
        <div class="brand">
          <span class="subtitle">Fluxd Node</span>
          <h1>Dashboard</h1>
        </div>
        <div class="status-pill syncing" id="syncStatus">
          <span class="status-dot"></span>
          <span id="syncState">INITIALIZING</span>
        </div>
      </header>

      <div class="grid">
        <div class="card">
          <div class="label">Network</div>
          <div class="value" id="network">--</div>
        </div>
        <div class="card">
          <div class="label">Backend</div>
          <div class="value" id="backend">--</div>
        </div>
        <div class="card">
          <div class="label">Uptime</div>
          <div class="value" id="uptime">--</div>
        </div>
        
        <div class="card">
          <div class="label">Best Header Height</div>
          <div class="value" id="bestHeaderHeight">0</div>
        </div>
        <div class="card">
          <div class="label">Best Block Height</div>
          <div class="value" id="bestBlockHeight">0</div>
        </div>
        <div class="card">
          <div class="label">Sync Gap</div>
          <div class="value" id="headerGap">0</div>
        </div>

        <div class="card">
          <div class="label">Headers / Sec</div>
          <div class="value" id="headersPerSec">0</div>
        </div>
        <div class="card">
          <div class="label">Blocks / Sec</div>
          <div class="value" id="blocksPerSec">0</div>
        </div>
        <div class="card">
          <div class="label">Header Count</div>
          <div class="value" id="headerCount">0</div>
        </div>

        <div class="card">
          <div class="label">Download (ms)</div>
          <div class="value" id="downloadMs">0</div>
        </div>
        <div class="card">
          <div class="label">Verify (ms)</div>
          <div class="value" id="verifyMs">0</div>
        </div>
        <div class="card">
          <div class="label">Storage (ms)</div>
          <div class="value" id="commitMs">0</div>
        </div>
        
        <div class="card">
          <div class="label">Validation (ms)</div>
          <div class="value" id="validateMs">0</div>
        </div>
        <div class="card">
          <div class="label">Script (ms)</div>
          <div class="value" id="scriptMs">0</div>
        </div>
        <div class="card">
          <div class="label">Shielded (ms)</div>
          <div class="value" id="shieldedMs">0</div>
        </div>

        <div class="card">
          <div class="label">UTXO (ms)</div>
          <div class="value" id="utxoMs">0</div>
        </div>
        <div class="card">
          <div class="label">Index (ms)</div>
          <div class="value" id="indexMs">0</div>
        </div>
        <div class="card">
          <div class="label">Anchor (ms)</div>
          <div class="value" id="anchorMs">0</div>
        </div>
	        <div class="card">
	          <div class="label">Flatfile (ms)</div>
	          <div class="value" id="flatfileMs">0</div>
	        </div>
	        <div class="card">
	          <div class="label">Fluxnode Tx (ms)</div>
	          <div class="value" id="fluxnodeTxMs">0</div>
	        </div>
	        <div class="card">
	          <div class="label">Fluxnode Sig (ms)</div>
	          <div class="value" id="fluxnodeSigMs">0</div>
	        </div>
	        <div class="card">
	          <div class="label">PoN Sig (ms)</div>
	          <div class="value" id="ponSigMs">0</div>
	        </div>
	        <div class="card">
	          <div class="label">Payout (ms)</div>
	          <div class="value" id="payoutMs">0</div>
	        </div>

        <div class="card wide">
          <div class="label">Best Header Hash</div>
          <div class="value mono" id="bestHeaderHash">--</div>
        </div>
        <div class="card wide">
          <div class="label">Best Block Hash</div>
          <div class="value mono" id="bestBlockHash">--</div>
        </div>
      </div>

      <div class="footer">
        <div id="lastUpdate">Connecting...</div>
        <div>Fluxd Rust Node</div>
      </div>
    </main>

    <script>
      const $ = (id) => document.getElementById(id);
      const syncStatus = $("syncStatus");
      let lastSample = null;

      function formatUptime(totalSecs) {
        if (!totalSecs) return "-";
        const d = Math.floor(totalSecs / 86400);
        const h = Math.floor((totalSecs % 86400) / 3600);
        const m = Math.floor((totalSecs % 3600) / 60);
        if (d > 0) return `${d}d ${h}h ${m}m`;
        if (h > 0) return `${h}h ${m}m`;
        return `${m}m ${Math.floor(totalSecs % 60)}s`;
      }

      function updateUI(data) {
        // Calculate rates
        let blocksPerSec = 0;
        let headersPerSec = 0;
        
        // Timings
        let downloadMs = 0;
        let verifyMs = 0;
        let commitMs = 0;
        let validateMs = 0;
        let scriptMs = 0;
        let shieldedMs = 0;
        let utxoMs = 0;
        let indexMs = 0;
        let anchorMs = 0;
        let flatfileMs = 0;
        let fluxnodeTxMs = 0;
        let fluxnodeSigMs = 0;
        let ponSigMs = 0;
        let payoutMs = 0;

        if (lastSample && data.unix_time_secs > lastSample.unix_time_secs) {
          const dt = data.unix_time_secs - lastSample.unix_time_secs;
          
          if (dt > 0) {
            blocksPerSec = ((data.block_count - lastSample.block_count) / dt).toFixed(1);
            headersPerSec = ((data.header_count - lastSample.header_count) / dt).toFixed(1);
          }

          const calcMs = (keyPrefix, countKey) => {
             const deltaUs = data[`${keyPrefix}_us`] - lastSample[`${keyPrefix}_us`];
             const deltaCount = data[countKey] - lastSample[countKey];
             return deltaCount > 0 ? (deltaUs / 1000 / deltaCount).toFixed(2) : 0;
          };

          downloadMs = calcMs('download', 'download_blocks');
          verifyMs = calcMs('verify', 'verify_blocks');
          commitMs = calcMs('commit', 'commit_blocks');
          validateMs = calcMs('validate', 'validate_blocks');
          scriptMs = calcMs('script', 'script_blocks');
          shieldedMs = calcMs('shielded', 'shielded_txs');
          utxoMs = calcMs('utxo', 'utxo_blocks');
          indexMs = calcMs('index', 'index_blocks');
          anchorMs = calcMs('anchor', 'anchor_blocks');
          flatfileMs = calcMs('flatfile', 'flatfile_blocks');
          fluxnodeTxMs = calcMs('fluxnode_tx', 'verify_blocks');
          fluxnodeSigMs = calcMs('fluxnode_sig', 'verify_blocks');
          ponSigMs = calcMs('pon_sig', 'pon_sig_blocks');
          payoutMs = calcMs('payout', 'payout_blocks');
        }

        // Update DOM
        $("network").textContent = data.network;
        $("backend").textContent = data.backend;
        $("uptime").textContent = formatUptime(data.uptime_secs);
        
        $("bestHeaderHeight").textContent = data.best_header_height.toLocaleString();
        $("bestBlockHeight").textContent = data.best_block_height.toLocaleString();
        $("headerGap").textContent = data.header_gap.toLocaleString();
        $("headerCount").textContent = data.header_count.toLocaleString();
        
        $("blocksPerSec").textContent = blocksPerSec;
        $("headersPerSec").textContent = headersPerSec;

        const setMs = (id, val) => $(id).textContent = val > 0 ? val : "-";
        
        setMs("downloadMs", downloadMs);
        setMs("verifyMs", verifyMs);
        setMs("commitMs", commitMs);
        setMs("validateMs", validateMs);
        setMs("scriptMs", scriptMs);
        setMs("shieldedMs", shieldedMs);
        setMs("utxoMs", utxoMs);
        setMs("indexMs", indexMs);
        setMs("anchorMs", anchorMs);
        setMs("flatfileMs", flatfileMs);
        setMs("fluxnodeTxMs", fluxnodeTxMs);
        setMs("fluxnodeSigMs", fluxnodeSigMs);
        setMs("ponSigMs", ponSigMs);
        setMs("payoutMs", payoutMs);

        $("bestHeaderHash").textContent = data.best_header_hash || "-";
        $("bestBlockHash").textContent = data.best_block_hash || "-";

        $("lastUpdate").textContent = "Updated: " + new Date().toLocaleTimeString();
        $("syncState").textContent = data.sync_state.toUpperCase();

        if (data.sync_state === "synced") {
          syncStatus.className = "status-pill";
        } else {
          syncStatus.className = "status-pill syncing";
        }

        lastSample = data;
      }

      async function fetchStats() {
        try {
          const res = await fetch("/stats", { cache: "no-store" });
          if (res.ok) {
            const data = await res.json();
            updateUI(data);
          }
        } catch (e) {
          console.error("Stats fetch failed", e);
        }
      }

      fetchStats();
      setInterval(fetchStats, 2000);
    </script>
  </body>
</html>
"#;
