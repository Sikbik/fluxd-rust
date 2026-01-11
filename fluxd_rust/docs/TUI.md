# TUI (Terminal UI)

`fluxd-rust` includes an optional terminal UI for monitoring sync, storage health, peers, mempool, wallet state, and live logs.

## Launch

- In-process (recommended): `fluxd --tui [other flags...]`
- Remote attach (read-only monitor): `fluxd --tui-attach <host[:port]>`
  - Polls `http://<host[:port]>/stats` (default port: `8080`)

## Navigation

- `q` / `Esc`: quit (requests shutdown in in-process mode)
- `Tab`: cycle views (Monitor → Peers → DB → Mempool → Wallet → Logs)
- `1-6`: jump to view (Monitor/Peers/DB/Mempool/Wallet/Logs)
- `?` / `h`: toggle help
- `a`: toggle advanced metrics
- `s`: toggle setup wizard

## Views

- **Monitor**: sync state + historical blocks/sec and headers/sec chart.
- **Peers**: connection counts and peer list (remote attach mode shows an empty list).
- **DB**: Fjall telemetry (write buffer, journals, compactions, per-partition segments/flushes).
- **Mempool**: size and recent accept/reject/orphan counters.
- **Wallet**: key/encryption state, transparent + Sapling balance summaries, recent wallet txs, and pending async ops.
- **Logs**: in-TUI ring buffer with filter + scroll.
  - Keys: `f` filter, `Space` pause/follow, `c` clear, `Up/Down/PageUp/PageDown/Home/End` scroll.

## Screenshot (example)

This is a representative “shape” of the monitor screen (values will differ):

```text
fluxd-rust  Monitor   Tab views  1-6 jump  ? help  q quit
Network: mainnet  Backend: fjall  Uptime: 01:23:45
Tip: headers 2200057  blocks 2200049  gap 8
Rates: h/s 150.0  b/s 120.0
...
```

## Setup wizard

The setup wizard writes a starter `flux.conf` into the active `--data-dir` and is meant to be “safe defaults” for a new install.

- Open: press `s`
- Keys:
  - `n`: cycle network (`mainnet` → `testnet` → `regtest`)
  - `p`: cycle profile (`low` → `default` → `high`)
  - `g`: generate new RPC username/password
  - `v`: show/hide RPC password
  - `w`: write `flux.conf` (backs up any existing `flux.conf` as `flux.conf.bak.<unix_ts>`)

Changes apply on restart.

## Remote attach

Remote attach mode requires a running dashboard server on the remote daemon (it serves `/stats`).

Example (remote daemon):
- Start the daemon with a dashboard bind, e.g. `--dashboard-addr 0.0.0.0:8080`

Example (local machine):
- `fluxd --tui-attach <remote-host>:8080`

Remote attach mode only has access to what the dashboard exposes; peer lists and logs will be empty.
