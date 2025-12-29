use std::net::SocketAddr;
use std::sync::{Arc, Mutex};
use std::time::Instant;

use fluxd_chainstate::metrics::ConnectMetrics;
use fluxd_chainstate::state::ChainState;
use fluxd_consensus::params::Network;
use fluxd_storage::KeyValueStore;
use tokio::io::{AsyncReadExt, AsyncWriteExt};
use tokio::net::TcpListener;

use fluxd_chainstate::validation::ValidationMetrics;

use crate::stats::{snapshot_stats, HeaderMetrics, SyncMetrics};
use crate::Backend;
use crate::Store;
use crate::{mempool::Mempool, stats::MempoolMetrics};

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
    network: Network,
    backend: Backend,
    start_time: Instant,
) -> Result<(), String> {
    let listener = TcpListener::bind(addr)
        .await
        .map_err(|err| format!("dashboard bind failed: {err}"))?;
    println!("Dashboard listening on http://{addr}");

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
                network,
                backend,
                start_time,
            )
            .await
            {
                eprintln!("dashboard error: {err}");
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
    <style>
      :root {
        --bg-1: #f6f1e6;
        --bg-2: #e7f2f1;
        --bg-3: #f9e9d1;
        --ink: #1b1b1b;
        --muted: #5f5f5f;
        --accent: #d4572f;
        --accent-2: #1a8b8b;
        --card: #fffaf0;
        --ring: rgba(26, 139, 139, 0.25);
        --shadow: rgba(33, 33, 33, 0.08);
      }

      * {
        box-sizing: border-box;
      }

      body {
        margin: 0;
        font-family: "Space Grotesk", "Satoshi", "Avenir Next", "Trebuchet MS", sans-serif;
        color: var(--ink);
        background:
          radial-gradient(circle at 20% 20%, rgba(212, 87, 47, 0.15), transparent 55%),
          radial-gradient(circle at 80% 10%, rgba(26, 139, 139, 0.18), transparent 45%),
          linear-gradient(135deg, var(--bg-1), var(--bg-2) 55%, var(--bg-3));
        min-height: 100vh;
        overflow-x: hidden;
      }

      .orb {
        position: fixed;
        border-radius: 50%;
        filter: blur(0.5px);
        opacity: 0.6;
        z-index: 0;
        animation: float 12s ease-in-out infinite;
      }

      .orb.one {
        width: 220px;
        height: 220px;
        background: rgba(26, 139, 139, 0.18);
        top: -60px;
        right: -40px;
      }

      .orb.two {
        width: 280px;
        height: 280px;
        background: rgba(212, 87, 47, 0.16);
        bottom: -120px;
        left: -80px;
        animation-delay: -4s;
      }

      main {
        position: relative;
        z-index: 1;
        max-width: 1100px;
        margin: 0 auto;
        padding: 40px 24px 64px;
      }

      header {
        display: flex;
        flex-wrap: wrap;
        align-items: center;
        justify-content: space-between;
        gap: 16px;
        margin-bottom: 24px;
      }

      .title {
        display: flex;
        flex-direction: column;
        gap: 8px;
      }

      h1 {
        margin: 0;
        font-size: clamp(28px, 3.5vw, 42px);
        letter-spacing: -0.02em;
      }

      .subtitle {
        color: var(--muted);
        font-size: 14px;
        letter-spacing: 0.12em;
        text-transform: uppercase;
      }

      .status {
        display: inline-flex;
        align-items: center;
        gap: 10px;
        padding: 8px 14px;
        border-radius: 999px;
        border: 1px solid var(--ring);
        background: rgba(255, 255, 255, 0.6);
        font-size: 14px;
      }

      .dot {
        width: 10px;
        height: 10px;
        border-radius: 50%;
        background: var(--accent-2);
        box-shadow: 0 0 0 6px rgba(26, 139, 139, 0.15);
        animation: pulse 2.5s ease-in-out infinite;
      }

      .status.syncing .dot {
        background: var(--accent);
        box-shadow: 0 0 0 6px rgba(212, 87, 47, 0.15);
      }

      .grid {
        display: grid;
        grid-template-columns: repeat(auto-fit, minmax(220px, 1fr));
        gap: 18px;
      }

      .card {
        background: var(--card);
        border-radius: 16px;
        padding: 18px 20px;
        box-shadow: 0 12px 30px var(--shadow);
        border: 1px solid rgba(255, 255, 255, 0.7);
        position: relative;
        overflow: hidden;
        min-height: 120px;
        animation: rise 0.8s ease both;
      }

      .card::after {
        content: "";
        position: absolute;
        inset: 0;
        background: linear-gradient(130deg, rgba(255, 255, 255, 0.7), transparent 60%);
        opacity: 0.6;
        pointer-events: none;
      }

      .label {
        font-size: 12px;
        text-transform: uppercase;
        letter-spacing: 0.16em;
        color: var(--muted);
      }

      .value {
        font-size: 26px;
        font-weight: 600;
        margin-top: 10px;
      }

      .value.small {
        font-size: 16px;
        word-break: break-all;
        font-family: "IBM Plex Mono", "SFMono-Regular", "Courier New", monospace;
      }

      .wide {
        grid-column: span 2;
      }

      .footer {
        margin-top: 24px;
        display: flex;
        flex-wrap: wrap;
        justify-content: space-between;
        gap: 8px;
        color: var(--muted);
        font-size: 13px;
      }

      @keyframes rise {
        from {
          opacity: 0;
          transform: translateY(14px);
        }
        to {
          opacity: 1;
          transform: translateY(0);
        }
      }

      @keyframes pulse {
        0%, 100% {
          transform: scale(1);
          opacity: 0.9;
        }
        50% {
          transform: scale(1.2);
          opacity: 0.6;
        }
      }

      @keyframes float {
        0%, 100% {
          transform: translateY(0);
        }
        50% {
          transform: translateY(18px);
        }
      }

      @media (max-width: 720px) {
        .wide {
          grid-column: span 1;
        }
      }

      @media (prefers-reduced-motion: reduce) {
        .orb,
        .card,
        .dot {
          animation: none !important;
        }
      }
    </style>
  </head>
  <body>
    <div class="orb one"></div>
    <div class="orb two"></div>
    <main>
      <header>
        <div class="title">
          <div class="subtitle">Fluxd Node Dashboard</div>
          <h1>Chain Sync Overview</h1>
        </div>
        <div class="status syncing" id="syncStatus">
          <span class="dot"></span>
          <span id="syncState">syncing</span>
        </div>
      </header>

      <section class="grid">
        <div class="card">
          <div class="label">Network</div>
          <div class="value" id="network">-</div>
        </div>
        <div class="card">
          <div class="label">Backend</div>
          <div class="value" id="backend">-</div>
        </div>
        <div class="card">
          <div class="label">Best Header Height</div>
          <div class="value" id="bestHeaderHeight">-</div>
        </div>
        <div class="card">
          <div class="label">Best Block Height</div>
          <div class="value" id="bestBlockHeight">-</div>
        </div>
        <div class="card">
          <div class="label">Header Gap</div>
          <div class="value" id="headerGap">-</div>
        </div>
        <div class="card">
          <div class="label">Indexed Blocks</div>
          <div class="value" id="blockCount">-</div>
        </div>
        <div class="card">
          <div class="label">Blocks / Sec</div>
          <div class="value" id="blocksPerSec">-</div>
        </div>
        <div class="card">
          <div class="label">Total Headers</div>
          <div class="value" id="headerCount">-</div>
        </div>
        <div class="card">
          <div class="label">Headers / Sec</div>
          <div class="value" id="headersPerSec">-</div>
        </div>
        <div class="card">
          <div class="label">Download ms / Block</div>
          <div class="value" id="downloadMs">-</div>
        </div>
        <div class="card">
          <div class="label">Verify ms / Block</div>
          <div class="value" id="verifyMs">-</div>
        </div>
        <div class="card">
          <div class="label">DB ms / Block</div>
          <div class="value" id="commitMs">-</div>
        </div>
        <div class="card">
          <div class="label">Validate ms / Block</div>
          <div class="value" id="validateMs">-</div>
        </div>
        <div class="card">
          <div class="label">Script ms / Block</div>
          <div class="value" id="scriptMs">-</div>
        </div>
        <div class="card">
          <div class="label">Shielded ms / Tx</div>
          <div class="value" id="shieldedMs">-</div>
        </div>
        <div class="card">
          <div class="label">UTXO ms / Block</div>
          <div class="value" id="utxoMs">-</div>
        </div>
        <div class="card">
          <div class="label">Index ms / Block</div>
          <div class="value" id="indexMs">-</div>
        </div>
        <div class="card">
          <div class="label">Anchor ms / Block</div>
          <div class="value" id="anchorMs">-</div>
        </div>
        <div class="card">
          <div class="label">Flatfile ms / Block</div>
          <div class="value" id="flatfileMs">-</div>
        </div>
        <div class="card">
          <div class="label">Uptime</div>
          <div class="value" id="uptime">-</div>
        </div>
        <div class="card wide">
          <div class="label">Best Header Hash</div>
          <div class="value small" id="bestHeaderHash">-</div>
        </div>
        <div class="card wide">
          <div class="label">Best Block Hash</div>
          <div class="value small" id="bestBlockHash">-</div>
        </div>
      </section>

      <div class="footer">
        <div>Last update: <span id="lastUpdate">-</span></div>
        <div>Server time: <span id="serverTime">-</span></div>
      </div>
    </main>

    <script>
      const el = (id) => document.getElementById(id);
      const syncStatus = el("syncStatus");
      let lastSample = null;

      function formatUptime(total) {
        const days = Math.floor(total / 86400);
        const hours = Math.floor((total % 86400) / 3600);
        const minutes = Math.floor((total % 3600) / 60);
        const seconds = total % 60;
        const parts = [];
        if (days > 0) parts.push(days + "d");
        parts.push(String(hours).padStart(2, "0") + "h");
        parts.push(String(minutes).padStart(2, "0") + "m");
        parts.push(String(seconds).padStart(2, "0") + "s");
        return parts.join(" ");
      }

      function applyStats(data) {
        let blocksPerSec = "-";
        let headersPerSec = "-";
        let downloadMs = "-";
        let verifyMs = "-";
        let commitMs = "-";
        let validateMs = "-";
        let scriptMs = "-";
        let shieldedMs = "-";
        let utxoMs = "-";
        let indexMs = "-";
        let anchorMs = "-";
        let flatfileMs = "-";
        if (lastSample && data.unix_time_secs > lastSample.unix_time_secs) {
          const dt = data.unix_time_secs - lastSample.unix_time_secs;
          const blocksDelta = data.block_count - lastSample.block_count;
          const headersDelta = data.header_count - lastSample.header_count;
          blocksPerSec = (blocksDelta / dt).toFixed(2);
          headersPerSec = (headersDelta / dt).toFixed(2);
          const downloadBlocks = data.download_blocks - lastSample.download_blocks;
          const verifyBlocks = data.verify_blocks - lastSample.verify_blocks;
          const commitBlocks = data.commit_blocks - lastSample.commit_blocks;
          const validateBlocks = data.validate_blocks - lastSample.validate_blocks;
          const scriptBlocks = data.script_blocks - lastSample.script_blocks;
          const shieldedTxs = data.shielded_txs - lastSample.shielded_txs;
          const utxoBlocks = data.utxo_blocks - lastSample.utxo_blocks;
          const indexBlocks = data.index_blocks - lastSample.index_blocks;
          const anchorBlocks = data.anchor_blocks - lastSample.anchor_blocks;
          const flatfileBlocks = data.flatfile_blocks - lastSample.flatfile_blocks;
          if (downloadBlocks > 0) {
            downloadMs = ((data.download_us - lastSample.download_us) / 1000 / downloadBlocks).toFixed(2);
          }
          if (verifyBlocks > 0) {
            verifyMs = ((data.verify_us - lastSample.verify_us) / 1000 / verifyBlocks).toFixed(2);
          }
          if (commitBlocks > 0) {
            commitMs = ((data.commit_us - lastSample.commit_us) / 1000 / commitBlocks).toFixed(2);
          }
          if (validateBlocks > 0) {
            validateMs = ((data.validate_us - lastSample.validate_us) / 1000 / validateBlocks).toFixed(2);
          }
          if (scriptBlocks > 0) {
            scriptMs = ((data.script_us - lastSample.script_us) / 1000 / scriptBlocks).toFixed(2);
          }
          if (shieldedTxs > 0) {
            shieldedMs = ((data.shielded_us - lastSample.shielded_us) / 1000 / shieldedTxs).toFixed(2);
          }
          if (utxoBlocks > 0) {
            utxoMs = ((data.utxo_us - lastSample.utxo_us) / 1000 / utxoBlocks).toFixed(2);
          }
          if (indexBlocks > 0) {
            indexMs = ((data.index_us - lastSample.index_us) / 1000 / indexBlocks).toFixed(2);
          }
          if (anchorBlocks > 0) {
            anchorMs = ((data.anchor_us - lastSample.anchor_us) / 1000 / anchorBlocks).toFixed(2);
          }
          if (flatfileBlocks > 0) {
            flatfileMs = ((data.flatfile_us - lastSample.flatfile_us) / 1000 / flatfileBlocks).toFixed(2);
          }
        }

        el("network").textContent = data.network;
        el("backend").textContent = data.backend;
        el("bestHeaderHeight").textContent = data.best_header_height;
        el("bestBlockHeight").textContent = data.best_block_height;
        el("headerGap").textContent = data.header_gap;
        el("headerCount").textContent = data.header_count;
        el("blockCount").textContent = data.block_count;
        el("blocksPerSec").textContent = blocksPerSec;
        el("headersPerSec").textContent = headersPerSec;
        el("downloadMs").textContent = downloadMs;
        el("verifyMs").textContent = verifyMs;
        el("commitMs").textContent = commitMs;
        el("validateMs").textContent = validateMs;
        el("scriptMs").textContent = scriptMs;
        el("shieldedMs").textContent = shieldedMs;
        el("utxoMs").textContent = utxoMs;
        el("indexMs").textContent = indexMs;
        el("anchorMs").textContent = anchorMs;
        el("flatfileMs").textContent = flatfileMs;
        el("bestHeaderHash").textContent = data.best_header_hash || "-";
        el("bestBlockHash").textContent = data.best_block_hash || "-";
        el("uptime").textContent = formatUptime(data.uptime_secs);
        el("serverTime").textContent = new Date(data.unix_time_secs * 1000).toLocaleString();
        el("lastUpdate").textContent = new Date().toLocaleTimeString();

        el("syncState").textContent = data.sync_state;
        if (data.sync_state === "synced") {
          syncStatus.classList.remove("syncing");
        } else {
          syncStatus.classList.add("syncing");
        }

        lastSample = data;
      }

      async function refresh() {
        try {
          const res = await fetch("/stats", { cache: "no-store" });
          if (!res.ok) return;
          const data = await res.json();
          applyStats(data);
        } catch (err) {
          // ignore transient fetch errors
        }
      }

      refresh();
      setInterval(refresh, 3000);
    </script>
  </body>
</html>
"#;
