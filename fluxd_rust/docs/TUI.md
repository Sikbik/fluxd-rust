# TUI (Terminal UI)

`fluxd-rust` includes an optional terminal UI for monitoring sync, storage health, peers, mempool, wallet state, and live logs.

## Launch

- In-process (recommended): `fluxd --tui [other flags...]`
- Remote attach (read-only monitor): `fluxd --tui-attach <host[:port]>`
  - Polls `http://<host[:port]>/{stats,peers,nettotals}` (default port: `8080`)
  - When `a` (advanced) is enabled and the Mempool view is open, it also polls `http://<host[:port]>/mempool`

## Navigation

- `q` / `Esc`: quit (requests shutdown in in-process mode)
- `Tab`: cycle views (Monitor → Stats → Peers → DB → Mempool → Wallet → Logs)
- `Shift+Tab`: cycle views backwards
- `←/→`: cycle views
- `1-6`: jump to view (Monitor/Peers/DB/Mempool/Wallet/Logs)
- `7`: jump to Stats view
- `?` / `h`: toggle help
- `a`: toggle advanced metrics
- `s`: toggle setup wizard
- Shielded params: in-process TUI auto-downloads missing params (same as `--fetch-params`).

## Views

- **Monitor**: sync state + historical blocks/sec and headers/sec chart.
- **Stats**: coin supply breakdown (transparent + shielded pools) and chain state.
- **Peers**: connection counts and peer list.
- **DB**: Fjall telemetry (write buffer, journals, compactions, per-partition segments/flushes).
- **Mempool**: size and recent accept/reject/orphan counters.
  - With `a` (advanced), shows fee/age/version breakdown.
- **Wallet**: key/encryption state, transparent + Sapling balance summaries, addresses + receive QR, recent wallet txs, and async ops.
  - Keys (in-process only): `Up/Down` select address, `Enter` toggle QR, `o` open explorer, `n` new t-addr, `N` new z-addr, `x` open send modal, `i` watch address.
  - Notes:
    - `Transparent (watch-only)` tracks imported addresses/scripts you don’t control (visible but not spendable).
      - Add via the TUI (`i`) or via RPC `importaddress`.
    - Change addresses are internal; they’re hidden in basic mode unless they currently hold funds (press `a` to show everything).
    - The `Async ops` panel is for shielded RPC operations that run asynchronously (e.g. `z_sendmany`, `z_shieldcoinbase`); normal transparent sends/receives won’t show up there.
- **Logs**: in-TUI ring buffer with filter + scroll.
  - Keys: `f` filter, `Space` pause/follow, `c` clear, `Up/Down/PageUp/PageDown/Home/End` scroll.
- **Setup**: writes `flux.conf` and configures RPC/auth defaults.
  - Keys: `↑/↓` highlight data dir, `Enter` select, `r` rescan, `d` delete (confirm), `b` backup wallet.dat, `n/p` cycle network/profile, `l` toggle lead, `[`/`]` adjust lead, `w` write config.

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
  - `l`: toggle header lead (`20000` ↔ `unlimited`)
  - `[` / `]`: decrease/increase header lead by 5000
  - `g`: generate new RPC username/password
  - `v`: show/hide RPC password
  - `w`: write `flux.conf` (backs up any existing `flux.conf` as `flux.conf.bak.<unix_ts>`)

Changes apply on restart.

## Remote attach

Remote attach mode requires a running dashboard server on the remote daemon (it serves `/stats`, `/peers`, `/nettotals`, and `/mempool`).

Example (remote daemon):
- Start the daemon with a dashboard bind, e.g. `--dashboard-addr 0.0.0.0:8080`

Example (local machine):
- `fluxd --tui-attach <remote-host>:8080`

Remote attach mode only has access to what the dashboard exposes; wallet controls and log capture are in-process only.
