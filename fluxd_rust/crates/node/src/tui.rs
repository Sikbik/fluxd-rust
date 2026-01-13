use std::collections::{BTreeMap, HashSet, VecDeque};
use std::fmt::Write as FmtWrite;
use std::fs;
use std::io::{self, Read, Write};
use std::net::TcpStream;
use std::path::PathBuf;
use std::process::{Command, Stdio};
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};

use crossbeam_channel::{Receiver, TryRecvError};

use bech32::{Bech32, Hrp};
use crossterm::cursor::{Hide, Show};
use crossterm::event::{
    self, DisableMouseCapture, EnableMouseCapture, Event, KeyCode, KeyEvent, KeyEventKind,
    KeyModifiers, MouseButton, MouseEvent, MouseEventKind,
};
use crossterm::execute;
use crossterm::terminal::{
    disable_raw_mode, enable_raw_mode, EnterAlternateScreen, LeaveAlternateScreen,
};
use qrcode::render::unicode;
use qrcode::QrCode;
use rand::distributions::Alphanumeric;
use rand::Rng;
use ratatui::backend::CrosstermBackend;
use ratatui::layout::{Constraint, Direction, Layout, Rect};
use ratatui::style::{Color, Modifier, Style};
use ratatui::symbols;
use ratatui::text::{Line, Span};
use ratatui::widgets::{
    Block, BorderType, Borders, Cell, Clear, Gauge, Paragraph, Row, Scrollbar,
    ScrollbarOrientation, ScrollbarState, Sparkline, SparklineBar, Table, Wrap,
};
use ratatui::Terminal;
use serde::de::DeserializeOwned;
use serde_json;
use tokio::sync::{broadcast, watch};

use fluxd_log as logging;

use fluxd_chainstate::metrics::ConnectMetrics;
use fluxd_chainstate::state::ChainState;
use fluxd_chainstate::validation::ValidationFlags;
use fluxd_chainstate::validation::ValidationMetrics;
use fluxd_consensus::constants::COINBASE_MATURITY;
use fluxd_consensus::params::ChainParams;
use fluxd_consensus::params::Network;
use fluxd_consensus::Hash256;
use fluxd_primitives::{address_to_script_pubkey, script_pubkey_to_address, OutPoint};

use crate::fee_estimator::FeeEstimator;
use crate::mempool::Mempool;
use crate::mempool::MempoolPolicy;
use crate::p2p::{NetTotals, PeerKind, PeerRegistry};
use crate::stats::{self, HeaderMetrics, MempoolMetrics, StatsSnapshot, SyncMetrics};
use crate::wallet::{SaplingAddressInfo, TransparentAddressInfo, Wallet};
use crate::RunProfile;
use crate::{Backend, Store};

const HISTORY_SAMPLES: usize = 900;
const SAMPLE_INTERVAL: Duration = Duration::from_secs(1);
const UI_TICK: Duration = Duration::from_millis(100);
const LOG_SNAPSHOT_LIMIT: usize = 4096;
const WALLET_REFRESH_INTERVAL: Duration = Duration::from_secs(5);
const WALLET_RECENT_TXS: usize = 12;
const WALLET_PENDING_OPS: usize = 12;
const PEER_SCROLL_STEP: u16 = 1;
const PEER_PAGE_STEP: u16 = 10;
const MOUSE_WHEEL_STEP: u16 = 3;
const QR_MAX_DIM: usize = 48;
const CTRL_C_GRACE: Duration = Duration::from_secs(2);
const HEADER_LEAD_STEP: i32 = 5000;

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
enum Screen {
    Monitor,
    Stats,
    Peers,
    Db,
    Mempool,
    Wallet,
    Logs,
    Setup,
    Help,
}

#[derive(Clone, Copy, Debug)]
struct Theme {
    bg: Color,
    panel: Color,
    border: Color,
    text: Color,
    muted: Color,
    accent: Color,
    accent_alt: Color,
    warning: Color,
    danger: Color,
    success: Color,
}

const THEME: Theme = Theme {
    bg: Color::Rgb(0, 0, 0),
    panel: Color::Rgb(10, 10, 10),
    border: Color::Rgb(40, 40, 40),
    text: Color::Rgb(230, 230, 230),
    muted: Color::Rgb(100, 100, 100),
    accent: Color::Rgb(0, 122, 204),
    accent_alt: Color::Rgb(50, 168, 82),
    warning: Color::Rgb(255, 180, 0),
    danger: Color::Rgb(255, 60, 60),
    success: Color::Rgb(80, 200, 80),
};

fn style_base() -> Style {
    Style::default().fg(THEME.text).bg(THEME.bg)
}

fn style_panel() -> Style {
    Style::default().fg(THEME.text).bg(THEME.panel)
}

fn style_muted() -> Style {
    Style::default().fg(THEME.muted).bg(THEME.panel)
}

fn style_key() -> Style {
    Style::default()
        .fg(THEME.accent)
        .bg(THEME.panel)
        .add_modifier(Modifier::BOLD)
}

fn style_command() -> Style {
    Style::default()
        .fg(THEME.accent)
        .add_modifier(Modifier::BOLD)
}

fn style_border() -> Style {
    Style::default().fg(THEME.border).bg(THEME.panel)
}

fn style_title() -> Style {
    Style::default()
        .fg(THEME.text)
        .bg(THEME.panel)
        .add_modifier(Modifier::BOLD)
}

fn style_error() -> Style {
    Style::default()
        .fg(THEME.danger)
        .bg(THEME.panel)
        .add_modifier(Modifier::BOLD)
}

fn style_warn() -> Style {
    Style::default()
        .fg(THEME.warning)
        .bg(THEME.panel)
        .add_modifier(Modifier::BOLD)
}

fn panel_block(title: impl Into<String>) -> Block<'static> {
    Block::default()
        .borders(Borders::ALL)
        .border_type(BorderType::Plain)
        .border_style(style_border())
        .style(Style::default().bg(THEME.panel))
        .title(Span::styled(title.into(), style_title()))
}

#[derive(Clone, Copy, Debug)]
enum CommandAction {
    Navigate(Screen),
    ToggleHelp,
    ToggleSetup,
    ToggleAdvanced,
    ToggleMouseCapture,
    Quit,
    LogLevel(Option<logging::Level>),
    WalletSend,
    WalletWatch,
    WalletNewTransparent,
    WalletNewSapling,
    WalletToggleQr,
    LogsClear,
    LogsPause,
    LogsFollow,
    Hint(&'static str),
}

#[derive(Clone, Copy, Debug)]
struct CommandSpec {
    command: &'static str,
    aliases: &'static [&'static str],
    description: &'static str,
    category: &'static str,
    quick: bool,
    action: CommandAction,
}

const COMMANDS: &[CommandSpec] = &[
    CommandSpec {
        command: "/monitor",
        aliases: &["/m"],
        description: "Monitor view",
        category: "Quick",
        quick: true,
        action: CommandAction::Navigate(Screen::Monitor),
    },
    CommandSpec {
        command: "/stats",
        aliases: &["/coin"],
        description: "Coin stats view",
        category: "Quick",
        quick: true,
        action: CommandAction::Navigate(Screen::Stats),
    },
    CommandSpec {
        command: "/peers",
        aliases: &["/p"],
        description: "Peers view",
        category: "Quick",
        quick: true,
        action: CommandAction::Navigate(Screen::Peers),
    },
    CommandSpec {
        command: "/db",
        aliases: &["/d"],
        description: "DB view",
        category: "Quick",
        quick: true,
        action: CommandAction::Navigate(Screen::Db),
    },
    CommandSpec {
        command: "/mempool",
        aliases: &["/t"],
        description: "Mempool view",
        category: "Quick",
        quick: true,
        action: CommandAction::Navigate(Screen::Mempool),
    },
    CommandSpec {
        command: "/wallet",
        aliases: &["/w"],
        description: "Wallet view",
        category: "Quick",
        quick: true,
        action: CommandAction::Navigate(Screen::Wallet),
    },
    CommandSpec {
        command: "/logs",
        aliases: &["/l"],
        description: "Logs view",
        category: "Quick",
        quick: true,
        action: CommandAction::Navigate(Screen::Logs),
    },
    CommandSpec {
        command: "/help",
        aliases: &["/h", "/?"],
        description: "Help panel",
        category: "Quick",
        quick: true,
        action: CommandAction::ToggleHelp,
    },
    CommandSpec {
        command: "/setup",
        aliases: &["/s"],
        description: "Setup wizard",
        category: "Quick",
        quick: true,
        action: CommandAction::ToggleSetup,
    },
    CommandSpec {
        command: "/quit",
        aliases: &["/q"],
        description: "Quit daemon",
        category: "Quick",
        quick: true,
        action: CommandAction::Quit,
    },
    CommandSpec {
        command: "/advanced",
        aliases: &["/a"],
        description: "Toggle advanced metrics",
        category: "System",
        quick: false,
        action: CommandAction::ToggleAdvanced,
    },
    CommandSpec {
        command: "/mouse",
        aliases: &[],
        description: "Toggle mouse capture for text selection",
        category: "System",
        quick: false,
        action: CommandAction::ToggleMouseCapture,
    },
    CommandSpec {
        command: "/send",
        aliases: &["/wallet-send"],
        description: "Send funds (in-process wallet)",
        category: "Wallet",
        quick: false,
        action: CommandAction::WalletSend,
    },
    CommandSpec {
        command: "/watch",
        aliases: &["/wallet-watch"],
        description: "Import watch-only address",
        category: "Wallet",
        quick: false,
        action: CommandAction::WalletWatch,
    },
    CommandSpec {
        command: "/new-t",
        aliases: &["/taddr", "/new-taddr"],
        description: "New transparent address",
        category: "Wallet",
        quick: false,
        action: CommandAction::WalletNewTransparent,
    },
    CommandSpec {
        command: "/new-z",
        aliases: &["/zaddr", "/new-zaddr"],
        description: "New sapling address",
        category: "Wallet",
        quick: false,
        action: CommandAction::WalletNewSapling,
    },
    CommandSpec {
        command: "/qr",
        aliases: &["/wallet-qr"],
        description: "Toggle wallet QR code",
        category: "Wallet",
        quick: false,
        action: CommandAction::WalletToggleQr,
    },
    CommandSpec {
        command: "/log",
        aliases: &[],
        description: "Set log level: trace|debug|info|warn|error",
        category: "Logs",
        quick: false,
        action: CommandAction::LogLevel(None),
    },
    CommandSpec {
        command: "/debug",
        aliases: &[],
        description: "Log level debug",
        category: "Logs",
        quick: false,
        action: CommandAction::LogLevel(Some(logging::Level::Debug)),
    },
    CommandSpec {
        command: "/info",
        aliases: &[],
        description: "Log level info",
        category: "Logs",
        quick: false,
        action: CommandAction::LogLevel(Some(logging::Level::Info)),
    },
    CommandSpec {
        command: "/warn",
        aliases: &[],
        description: "Log level warn",
        category: "Logs",
        quick: false,
        action: CommandAction::LogLevel(Some(logging::Level::Warn)),
    },
    CommandSpec {
        command: "/error",
        aliases: &[],
        description: "Log level error",
        category: "Logs",
        quick: false,
        action: CommandAction::LogLevel(Some(logging::Level::Error)),
    },
    CommandSpec {
        command: "/trace",
        aliases: &[],
        description: "Log level trace",
        category: "Logs",
        quick: false,
        action: CommandAction::LogLevel(Some(logging::Level::Trace)),
    },
    CommandSpec {
        command: "/log-clear",
        aliases: &["/clear"],
        description: "Clear captured logs",
        category: "Logs",
        quick: false,
        action: CommandAction::LogsClear,
    },
    CommandSpec {
        command: "/log-pause",
        aliases: &["/pause"],
        description: "Pause log capture",
        category: "Logs",
        quick: false,
        action: CommandAction::LogsPause,
    },
    CommandSpec {
        command: "/log-follow",
        aliases: &["/follow"],
        description: "Resume following logs",
        category: "Logs",
        quick: false,
        action: CommandAction::LogsFollow,
    },
    CommandSpec {
        command: "/cli-help",
        aliases: &["/help-cli"],
        description: "Show CLI help (run: fluxd --help)",
        category: "CLI",
        quick: false,
        action: CommandAction::Hint("Run: fluxd --help"),
    },
    CommandSpec {
        command: "/version",
        aliases: &["/cli-version"],
        description: "Show version (run: fluxd --version)",
        category: "CLI",
        quick: false,
        action: CommandAction::Hint("Run: fluxd --version"),
    },
    CommandSpec {
        command: "/backend",
        aliases: &[],
        description: "Storage backend (fjall|memory)",
        category: "Configuration",
        quick: false,
        action: CommandAction::Hint("Restart required: fluxd --backend <fjall|memory>"),
    },
    CommandSpec {
        command: "/data-dir",
        aliases: &[],
        description: "Data directory path",
        category: "Configuration",
        quick: false,
        action: CommandAction::Hint("Restart required: fluxd --data-dir <path>"),
    },
    CommandSpec {
        command: "/conf",
        aliases: &[],
        description: "Config file path",
        category: "Configuration",
        quick: false,
        action: CommandAction::Hint("Restart required: fluxd --conf <path>"),
    },
    CommandSpec {
        command: "/params-dir",
        aliases: &[],
        description: "Shielded params directory",
        category: "Configuration",
        quick: false,
        action: CommandAction::Hint("Restart required: fluxd --params-dir <path>"),
    },
    CommandSpec {
        command: "/profile",
        aliases: &[],
        description: "Tuning preset (low|default|high)",
        category: "Configuration",
        quick: false,
        action: CommandAction::Hint("Restart required: fluxd --profile <low|default|high>"),
    },
    CommandSpec {
        command: "/network",
        aliases: &[],
        description: "Network (mainnet|testnet|regtest)",
        category: "Configuration",
        quick: false,
        action: CommandAction::Hint("Restart required: fluxd --network <mainnet|testnet|regtest>"),
    },
    CommandSpec {
        command: "/miner-address",
        aliases: &[],
        description: "Default miner address",
        category: "Configuration",
        quick: false,
        action: CommandAction::Hint("Restart required: fluxd --miner-address <addr>"),
    },
    CommandSpec {
        command: "/log-level",
        aliases: &[],
        description: "Startup log verbosity",
        category: "Configuration",
        quick: false,
        action: CommandAction::Hint(
            "Restart required: fluxd --log-level <error|warn|info|debug|trace>",
        ),
    },
    CommandSpec {
        command: "/log-format",
        aliases: &[],
        description: "Log output format (text|json)",
        category: "Configuration",
        quick: false,
        action: CommandAction::Hint("Restart required: fluxd --log-format <text|json>"),
    },
    CommandSpec {
        command: "/log-timestamps",
        aliases: &[],
        description: "Enable timestamps in text logs",
        category: "Configuration",
        quick: false,
        action: CommandAction::Hint("Restart required: fluxd --log-timestamps"),
    },
    CommandSpec {
        command: "/no-log-timestamps",
        aliases: &[],
        description: "Disable timestamps in text logs",
        category: "Configuration",
        quick: false,
        action: CommandAction::Hint("Restart required: fluxd --no-log-timestamps"),
    },
    CommandSpec {
        command: "/fetch-params",
        aliases: &[],
        description: "Download shielded params",
        category: "Configuration",
        quick: false,
        action: CommandAction::Hint("Run once: fluxd --fetch-params"),
    },
    CommandSpec {
        command: "/tui",
        aliases: &[],
        description: "Launch TUI mode",
        category: "Configuration",
        quick: false,
        action: CommandAction::Hint("Start: fluxd --tui"),
    },
    CommandSpec {
        command: "/tui-attach",
        aliases: &[],
        description: "Launch TUI attach mode",
        category: "Configuration",
        quick: false,
        action: CommandAction::Hint("Start: fluxd --tui-attach http://HOST:PORT/stats"),
    },
    CommandSpec {
        command: "/dashboard-addr",
        aliases: &[],
        description: "Dashboard HTTP bind address",
        category: "RPC",
        quick: false,
        action: CommandAction::Hint("Restart required: fluxd --dashboard-addr <host:port>"),
    },
    CommandSpec {
        command: "/rpc-addr",
        aliases: &[],
        description: "RPC bind address",
        category: "RPC",
        quick: false,
        action: CommandAction::Hint("Restart required: fluxd --rpc-addr <host:port>"),
    },
    CommandSpec {
        command: "/rpc-user",
        aliases: &[],
        description: "RPC username",
        category: "RPC",
        quick: false,
        action: CommandAction::Hint("Restart required: fluxd --rpc-user <user>"),
    },
    CommandSpec {
        command: "/rpc-pass",
        aliases: &[],
        description: "RPC password",
        category: "RPC",
        quick: false,
        action: CommandAction::Hint("Restart required: fluxd --rpc-pass <pass>"),
    },
    CommandSpec {
        command: "/rpc-allow-ip",
        aliases: &[],
        description: "RPC allowlist (CIDR)",
        category: "RPC",
        quick: false,
        action: CommandAction::Hint("Restart required: fluxd --rpc-allow-ip <cidr>"),
    },
    CommandSpec {
        command: "/p2p-addr",
        aliases: &[],
        description: "P2P bind address",
        category: "P2P",
        quick: false,
        action: CommandAction::Hint("Restart required: fluxd --p2p-addr <host:port>"),
    },
    CommandSpec {
        command: "/no-p2p-listen",
        aliases: &[],
        description: "Disable inbound P2P listener",
        category: "P2P",
        quick: false,
        action: CommandAction::Hint("Restart required: fluxd --no-p2p-listen"),
    },
    CommandSpec {
        command: "/addnode",
        aliases: &[],
        description: "Add manual peer",
        category: "P2P",
        quick: false,
        action: CommandAction::Hint("Restart required: fluxd --addnode <host[:port]>"),
    },
    CommandSpec {
        command: "/maxconnections",
        aliases: &[],
        description: "Max peer connections",
        category: "P2P",
        quick: false,
        action: CommandAction::Hint("Restart required: fluxd --maxconnections <n>"),
    },
    CommandSpec {
        command: "/block-peers",
        aliases: &[],
        description: "Parallel block peers",
        category: "P2P",
        quick: false,
        action: CommandAction::Hint("Restart required: fluxd --block-peers <n>"),
    },
    CommandSpec {
        command: "/header-peers",
        aliases: &[],
        description: "Header sync peers",
        category: "P2P",
        quick: false,
        action: CommandAction::Hint("Restart required: fluxd --header-peers <n>"),
    },
    CommandSpec {
        command: "/header-peer",
        aliases: &[],
        description: "Pin header peer",
        category: "P2P",
        quick: false,
        action: CommandAction::Hint("Restart required: fluxd --header-peer <host[:port]>"),
    },
    CommandSpec {
        command: "/header-lead",
        aliases: &[],
        description: "Target header lead",
        category: "P2P",
        quick: false,
        action: CommandAction::Hint("Restart required: fluxd --header-lead <n>"),
    },
    CommandSpec {
        command: "/tx-peers",
        aliases: &[],
        description: "Relay peers for txs",
        category: "P2P",
        quick: false,
        action: CommandAction::Hint("Restart required: fluxd --tx-peers <n>"),
    },
    CommandSpec {
        command: "/inflight-per-peer",
        aliases: &[],
        description: "Concurrent getdata per peer",
        category: "P2P",
        quick: false,
        action: CommandAction::Hint("Restart required: fluxd --inflight-per-peer <n>"),
    },
    CommandSpec {
        command: "/getdata-batch",
        aliases: &[],
        description: "Blocks per getdata request",
        category: "P2P",
        quick: false,
        action: CommandAction::Hint("Restart required: fluxd --getdata-batch <n>"),
    },
    CommandSpec {
        command: "/minrelaytxfee",
        aliases: &[],
        description: "Minimum relay fee-rate",
        category: "Mempool",
        quick: false,
        action: CommandAction::Hint("Restart required: fluxd --minrelaytxfee <zatoshi/kB>"),
    },
    CommandSpec {
        command: "/limitfreerelay",
        aliases: &[],
        description: "Rate-limit free txs",
        category: "Mempool",
        quick: false,
        action: CommandAction::Hint("Restart required: fluxd --limitfreerelay <n>"),
    },
    CommandSpec {
        command: "/accept-non-standard",
        aliases: &[],
        description: "Disable standardness checks",
        category: "Mempool",
        quick: false,
        action: CommandAction::Hint("Restart required: fluxd --accept-non-standard"),
    },
    CommandSpec {
        command: "/require-standard",
        aliases: &[],
        description: "Force standardness on regtest",
        category: "Mempool",
        quick: false,
        action: CommandAction::Hint("Restart required: fluxd --require-standard"),
    },
    CommandSpec {
        command: "/mempool-max-mb",
        aliases: &[],
        description: "Mempool size cap (MiB)",
        category: "Mempool",
        quick: false,
        action: CommandAction::Hint("Restart required: fluxd --mempool-max-mb <n>"),
    },
    CommandSpec {
        command: "/mempool-persist-interval",
        aliases: &[],
        description: "Persist mempool every N seconds",
        category: "Mempool",
        quick: false,
        action: CommandAction::Hint("Restart required: fluxd --mempool-persist-interval <secs>"),
    },
    CommandSpec {
        command: "/fee-estimates-persist-interval",
        aliases: &[],
        description: "Persist fee estimates",
        category: "Mempool",
        quick: false,
        action: CommandAction::Hint(
            "Restart required: fluxd --fee-estimates-persist-interval <secs>",
        ),
    },
    CommandSpec {
        command: "/db-cache-mb",
        aliases: &[],
        description: "Fjall cache size (MiB)",
        category: "Storage",
        quick: false,
        action: CommandAction::Hint("Restart required: fluxd --db-cache-mb <MiB>"),
    },
    CommandSpec {
        command: "/db-write-buffer-mb",
        aliases: &[],
        description: "Fjall write buffer (MiB)",
        category: "Storage",
        quick: false,
        action: CommandAction::Hint("Restart required: fluxd --db-write-buffer-mb <MiB>"),
    },
    CommandSpec {
        command: "/db-journal-mb",
        aliases: &[],
        description: "Fjall journal size (MiB)",
        category: "Storage",
        quick: false,
        action: CommandAction::Hint("Restart required: fluxd --db-journal-mb <MiB>"),
    },
    CommandSpec {
        command: "/db-memtable-mb",
        aliases: &[],
        description: "Fjall memtable size (MiB)",
        category: "Storage",
        quick: false,
        action: CommandAction::Hint("Restart required: fluxd --db-memtable-mb <MiB>"),
    },
    CommandSpec {
        command: "/db-flush-workers",
        aliases: &[],
        description: "Fjall flush worker threads",
        category: "Storage",
        quick: false,
        action: CommandAction::Hint("Restart required: fluxd --db-flush-workers <n>"),
    },
    CommandSpec {
        command: "/db-compaction-workers",
        aliases: &[],
        description: "Fjall compaction workers",
        category: "Storage",
        quick: false,
        action: CommandAction::Hint("Restart required: fluxd --db-compaction-workers <n>"),
    },
    CommandSpec {
        command: "/db-fsync-ms",
        aliases: &[],
        description: "Fjall async fsync interval",
        category: "Storage",
        quick: false,
        action: CommandAction::Hint("Restart required: fluxd --db-fsync-ms <ms>"),
    },
    CommandSpec {
        command: "/utxo-cache-entries",
        aliases: &[],
        description: "In-memory UTXO cache entries",
        category: "Storage",
        quick: false,
        action: CommandAction::Hint("Restart required: fluxd --utxo-cache-entries <n>"),
    },
    CommandSpec {
        command: "/header-verify-workers",
        aliases: &[],
        description: "Header POW verification threads",
        category: "Performance",
        quick: false,
        action: CommandAction::Hint("Restart required: fluxd --header-verify-workers <n>"),
    },
    CommandSpec {
        command: "/verify-workers",
        aliases: &[],
        description: "Pre-validation worker threads",
        category: "Performance",
        quick: false,
        action: CommandAction::Hint("Restart required: fluxd --verify-workers <n>"),
    },
    CommandSpec {
        command: "/verify-queue",
        aliases: &[],
        description: "Pre-validation queue depth",
        category: "Performance",
        quick: false,
        action: CommandAction::Hint("Restart required: fluxd --verify-queue <n>"),
    },
    CommandSpec {
        command: "/shielded-workers",
        aliases: &[],
        description: "Shielded verification threads",
        category: "Performance",
        quick: false,
        action: CommandAction::Hint("Restart required: fluxd --shielded-workers <n>"),
    },
    CommandSpec {
        command: "/txconfirmtarget",
        aliases: &[],
        description: "Fee estimation target blocks",
        category: "Performance",
        quick: false,
        action: CommandAction::Hint("Restart required: fluxd --txconfirmtarget <blocks>"),
    },
    CommandSpec {
        command: "/status-interval",
        aliases: &[],
        description: "Status log interval seconds",
        category: "Performance",
        quick: false,
        action: CommandAction::Hint("Restart required: fluxd --status-interval <secs>"),
    },
    CommandSpec {
        command: "/reindex",
        aliases: &[],
        description: "Rebuild DB/indexes (offline)",
        category: "Maintenance",
        quick: false,
        action: CommandAction::Hint("Run once: fluxd --reindex"),
    },
    CommandSpec {
        command: "/resync",
        aliases: &[],
        description: "Wipe data-dir and resync (danger)",
        category: "Maintenance",
        quick: false,
        action: CommandAction::Hint("Danger: fluxd --resync (wipes data-dir)"),
    },
    CommandSpec {
        command: "/reindex-txindex",
        aliases: &[],
        description: "Rebuild txindex",
        category: "Maintenance",
        quick: false,
        action: CommandAction::Hint("Run once: fluxd --reindex-txindex"),
    },
    CommandSpec {
        command: "/reindex-spentindex",
        aliases: &[],
        description: "Rebuild spent index",
        category: "Maintenance",
        quick: false,
        action: CommandAction::Hint("Run once: fluxd --reindex-spentindex"),
    },
    CommandSpec {
        command: "/reindex-addressindex",
        aliases: &[],
        description: "Rebuild address index",
        category: "Maintenance",
        quick: false,
        action: CommandAction::Hint("Run once: fluxd --reindex-addressindex"),
    },
    CommandSpec {
        command: "/db-info",
        aliases: &[],
        description: "DB/flatfile size breakdown",
        category: "Diagnostics",
        quick: false,
        action: CommandAction::Hint("Run once: fluxd db-info"),
    },
    CommandSpec {
        command: "/db-info-keys",
        aliases: &[],
        description: "DB key/byte counts (slow)",
        category: "Diagnostics",
        quick: false,
        action: CommandAction::Hint("Run once: fluxd db-info-keys"),
    },
    CommandSpec {
        command: "/db-integrity",
        aliases: &[],
        description: "DB sanity + verify last 288 blocks",
        category: "Diagnostics",
        quick: false,
        action: CommandAction::Hint("Run once: fluxd db-integrity"),
    },
    CommandSpec {
        command: "/scan-flatfiles",
        aliases: &[],
        description: "Scan flatfiles for mismatches",
        category: "Diagnostics",
        quick: false,
        action: CommandAction::Hint("Run once: fluxd scan-flatfiles"),
    },
    CommandSpec {
        command: "/scan-supply",
        aliases: &[],
        description: "Scan supply from local DB",
        category: "Diagnostics",
        quick: false,
        action: CommandAction::Hint("Run once: fluxd scan-supply"),
    },
    CommandSpec {
        command: "/scan-fluxnodes",
        aliases: &[],
        description: "Scan fluxnode records",
        category: "Diagnostics",
        quick: false,
        action: CommandAction::Hint("Run once: fluxd scan-fluxnodes"),
    },
    CommandSpec {
        command: "/debug-fluxnode-payee-script",
        aliases: &[],
        description: "Find matching fluxnode payee script",
        category: "Debug",
        quick: false,
        action: CommandAction::Hint("Run once: fluxd --debug-fluxnode-payee-script <script-hex>"),
    },
    CommandSpec {
        command: "/debug-fluxnode-payouts",
        aliases: &[],
        description: "Print deterministic fluxnode payouts",
        category: "Debug",
        quick: false,
        action: CommandAction::Hint("Run once: fluxd --debug-fluxnode-payouts <height>"),
    },
    CommandSpec {
        command: "/debug-fluxnode-payee-candidates",
        aliases: &[],
        description: "Print ordered payee candidates",
        category: "Debug",
        quick: false,
        action: CommandAction::Hint(
            "Run once: fluxd --debug-fluxnode-payee-candidates <tier> <height> <limit>",
        ),
    },
    CommandSpec {
        command: "/skip-script",
        aliases: &[],
        description: "Disable script validation (testing)",
        category: "Debug",
        quick: false,
        action: CommandAction::Hint("Danger: fluxd --skip-script"),
    },
];

fn command_query(input: &str) -> &str {
    input.trim().split_whitespace().next().unwrap_or("")
}

fn normalize_command(value: &str) -> String {
    value.trim().trim_start_matches('/').to_lowercase()
}

fn command_match_score(spec: &CommandSpec, query: &str) -> Option<u8> {
    if query.is_empty() {
        return Some(0);
    }
    let command = normalize_command(spec.command);
    if command.starts_with(query) {
        return Some(0);
    }
    if spec
        .aliases
        .iter()
        .any(|alias| normalize_command(alias).starts_with(query))
    {
        return Some(1);
    }
    if command.contains(query) {
        return Some(2);
    }
    if spec
        .aliases
        .iter()
        .any(|alias| normalize_command(alias).contains(query))
    {
        return Some(3);
    }
    if spec.description.to_lowercase().contains(query) {
        return Some(4);
    }
    if spec.category.to_lowercase().contains(query) {
        return Some(5);
    }
    None
}

fn command_suggestions(input: &str) -> Vec<&'static CommandSpec> {
    let raw_query = command_query(input);
    let blank = raw_query.is_empty() || raw_query == "/";
    let query = normalize_command(raw_query);
    if blank {
        return COMMANDS.iter().filter(|spec| spec.quick).collect();
    }

    let mut results: Vec<(u8, &CommandSpec)> = COMMANDS
        .iter()
        .filter_map(|spec| command_match_score(spec, &query).map(|score| (score, spec)))
        .collect();
    results.sort_by(|(score_a, spec_a), (score_b, spec_b)| {
        score_a
            .cmp(score_b)
            .then_with(|| spec_a.command.cmp(spec_b.command))
    });
    results.into_iter().map(|(_, spec)| spec).collect()
}

fn find_command_spec(command: &str) -> Option<&'static CommandSpec> {
    let needle = normalize_command(command);
    if needle.is_empty() {
        return None;
    }
    COMMANDS.iter().find(|spec| {
        let command = normalize_command(spec.command);
        if command == needle {
            return true;
        }
        spec.aliases
            .iter()
            .any(|alias| normalize_command(alias) == needle)
    })
}

fn parse_log_level(value: &str) -> Option<logging::Level> {
    match value {
        "trace" => Some(logging::Level::Trace),
        "debug" => Some(logging::Level::Debug),
        "info" => Some(logging::Level::Info),
        "warn" | "warning" => Some(logging::Level::Warn),
        "error" => Some(logging::Level::Error),
        _ => None,
    }
}

fn log_level_label(level: logging::Level) -> &'static str {
    match level {
        logging::Level::Trace => "trace",
        logging::Level::Debug => "debug",
        logging::Level::Info => "info",
        logging::Level::Warn => "warn",
        logging::Level::Error => "error",
    }
}

fn base64_encode(input: &[u8]) -> String {
    const TABLE: &[u8; 64] = b"ABCDEFGHIJKLMNOPQRSTUVWXYZabcdefghijklmnopqrstuvwxyz0123456789+/";
    let mut output = String::with_capacity((input.len() + 2) / 3 * 4);
    let mut index = 0;
    while index < input.len() {
        let b0 = input[index];
        let b1 = input.get(index + 1).copied().unwrap_or(0);
        let b2 = input.get(index + 2).copied().unwrap_or(0);
        let n = ((b0 as u32) << 16) | ((b1 as u32) << 8) | (b2 as u32);
        output.push(TABLE[((n >> 18) & 0x3f) as usize] as char);
        output.push(TABLE[((n >> 12) & 0x3f) as usize] as char);
        if index + 1 < input.len() {
            output.push(TABLE[((n >> 6) & 0x3f) as usize] as char);
        } else {
            output.push('=');
        }
        if index + 2 < input.len() {
            output.push(TABLE[(n & 0x3f) as usize] as char);
        } else {
            output.push('=');
        }
        index += 3;
    }
    output
}

fn osc52_copy(text: &str) -> Result<(), String> {
    let payload = base64_encode(text.as_bytes());
    let seq = format!("\u{1b}]52;c;{}\u{7}", payload);
    let mut stdout = io::stdout();
    stdout
        .write_all(seq.as_bytes())
        .map_err(|err| err.to_string())?;
    stdout.flush().map_err(|err| err.to_string())?;
    Ok(())
}

fn is_wsl() -> bool {
    if std::env::var("WSL_DISTRO_NAME").is_ok() || std::env::var("WSL_INTEROP").is_ok() {
        return true;
    }
    fs::read_to_string("/proc/sys/kernel/osrelease")
        .map(|value| value.to_lowercase().contains("microsoft"))
        .unwrap_or(false)
}

fn wsl_clipboard_copy(text: &str) -> Result<(), String> {
    let mut child = Command::new("clip.exe")
        .stdin(Stdio::piped())
        .stdout(Stdio::null())
        .stderr(Stdio::null())
        .spawn()
        .map_err(|err| err.to_string())?;
    if let Some(stdin) = child.stdin.as_mut() {
        stdin
            .write_all(text.as_bytes())
            .map_err(|err| err.to_string())?;
    }
    let status = child.wait().map_err(|err| err.to_string())?;
    if status.success() {
        Ok(())
    } else {
        Err("clip.exe failed".to_string())
    }
}

fn clipboard_copy(text: &str) -> Result<(), String> {
    if is_wsl() {
        if wsl_clipboard_copy(text).is_ok() {
            return Ok(());
        }
    }
    osc52_copy(text)
}

fn ctrl_c_exit(state: &mut TuiState, shutdown_tx: &watch::Sender<bool>) -> Result<bool, String> {
    let now = Instant::now();
    if let Some(last) = state.last_ctrl_c {
        if now.duration_since(last) <= CTRL_C_GRACE {
            let _ = shutdown_tx.send(true);
            return Ok(true);
        }
    }
    state.last_ctrl_c = Some(now);
    state.command_status = Some("Press Ctrl+C again to quit".to_string());
    Ok(false)
}

fn explorer_tx_url(txid: &str) -> String {
    format!("https://explorer.runonflux.io/tx/{txid}")
}

fn explorer_address_url(address: &str) -> String {
    format!("https://explorer.runonflux.io/address/{address}")
}

fn open_url(url: &str) -> Result<(), String> {
    let candidates: [(&str, &[&str]); 3] = [
        ("xdg-open", &[url]),
        ("open", &[url]),
        ("explorer.exe", &[url]),
    ];
    for (cmd, args) in candidates {
        if Command::new(cmd)
            .args(args)
            .stdin(Stdio::null())
            .stdout(Stdio::null())
            .stderr(Stdio::null())
            .spawn()
            .is_ok()
        {
            return Ok(());
        }
    }
    Err("no URL opener available".to_string())
}

fn open_or_copy_url(url: &str) -> Option<&'static str> {
    if open_url(url).is_ok() {
        return Some("Explorer opened");
    }
    if clipboard_copy(url).is_ok() {
        return Some("Explorer link copied");
    }
    None
}

fn header_line(state: &TuiState, active: Screen) -> Line<'static> {
    let mut spans: Vec<Span<'static>> = Vec::new();
    spans.push(Span::styled(
        " fluxd ",
        Style::default()
            .fg(THEME.bg)
            .bg(THEME.accent)
            .add_modifier(Modifier::BOLD),
    ));
    spans.push(Span::raw(" "));

    if state.is_remote {
        spans.push(Span::styled(
            " REMOTE ",
            Style::default()
                .fg(THEME.bg)
                .bg(THEME.warning)
                .add_modifier(Modifier::BOLD),
        ));
        spans.push(Span::raw(" "));
    }

    for (screen, label) in [
        (Screen::Monitor, "Monitor"),
        (Screen::Stats, "Stats"),
        (Screen::Peers, "Peers"),
        (Screen::Db, "DB"),
        (Screen::Mempool, "Mempool"),
        (Screen::Wallet, "Wallet"),
        (Screen::Logs, "Logs"),
    ] {
        if screen == active {
            spans.push(Span::styled(
                format!(" {label} "),
                Style::default()
                    .fg(THEME.accent_alt)
                    .bg(THEME.panel)
                    .add_modifier(Modifier::BOLD),
            ));
        } else {
            spans.push(Span::styled(
                format!(" {label} "),
                Style::default().fg(THEME.muted).bg(THEME.panel),
            ));
        }
    }

    Line::from(spans)
}

fn rect_contains(area: Rect, column: u16, row: u16) -> bool {
    column >= area.x
        && column < area.x.saturating_add(area.width)
        && row >= area.y
        && row < area.y.saturating_add(area.height)
}

fn render_vertical_scrollbar(
    frame: &mut ratatui::Frame<'_>,
    area: Rect,
    content_length: usize,
    position: usize,
    viewport_length: usize,
) {
    if area.width == 0 || area.height == 0 {
        return;
    }

    let scrollbar = Scrollbar::new(ScrollbarOrientation::VerticalRight)
        .style(Style::default().fg(THEME.muted).bg(THEME.panel));

    let mut scrollbar_state = ScrollbarState::new(content_length)
        .position(position)
        .viewport_content_length(viewport_length);

    frame.render_stateful_widget(scrollbar, area, &mut scrollbar_state);
}

fn layout_areas(state: &TuiState, area: Rect) -> (Rect, Rect, Rect, Rect) {
    let command_height = if state.command_mode { 10 } else { 3 };
    let vertical = Layout::default()
        .direction(Direction::Vertical)
        .constraints([
            Constraint::Length(1),
            Constraint::Min(0),
            Constraint::Length(command_height),
        ])
        .split(area);

    let middle = Layout::default()
        .direction(Direction::Horizontal)
        .constraints([Constraint::Min(0), Constraint::Length(42)])
        .split(vertical[1]);

    (vertical[0], middle[0], middle[1], vertical[2])
}

fn header_tab_at(state: &TuiState, area: Rect, column: u16, row: u16) -> Option<Screen> {
    if row != area.y || column < area.x || column >= area.x.saturating_add(area.width) {
        return None;
    }

    let mut cursor = area.x;
    cursor = cursor.saturating_add(" fluxd ".len() as u16);
    cursor = cursor.saturating_add(1);
    if state.is_remote {
        cursor = cursor.saturating_add(" REMOTE ".len() as u16);
        cursor = cursor.saturating_add(1);
    }

    for (screen, label) in [
        (Screen::Monitor, "Monitor"),
        (Screen::Stats, "Stats"),
        (Screen::Peers, "Peers"),
        (Screen::Db, "DB"),
        (Screen::Mempool, "Mempool"),
        (Screen::Wallet, "Wallet"),
        (Screen::Logs, "Logs"),
    ] {
        let width = (label.len() + 2) as u16;
        let end = cursor.saturating_add(width);
        if column >= cursor && column < end {
            return Some(screen);
        }
        cursor = end;
    }

    None
}

fn command_palette_areas(area: Rect) -> Option<(Rect, Rect)> {
    if area.width < 3 || area.height < 3 {
        return None;
    }
    let inner = Rect::new(
        area.x.saturating_add(1),
        area.y.saturating_add(1),
        area.width.saturating_sub(2),
        area.height.saturating_sub(2),
    );
    if inner.height < 2 {
        return None;
    }
    let chunks = Layout::default()
        .direction(Direction::Vertical)
        .constraints([Constraint::Min(0), Constraint::Length(1)])
        .split(inner);
    Some((chunks[0], chunks[1]))
}

fn wallet_qr_modal_area(area: Rect) -> Rect {
    centered_rect(60, 75, area)
}

#[derive(Clone, Debug, Default, serde::Deserialize)]
struct RemoteNetTotals {
    bytes_recv: u64,
    bytes_sent: u64,
    connections: usize,
}

#[derive(Clone, Debug, serde::Deserialize)]
struct RemotePeerInfo {
    addr: String,
    kind: String,
    inbound: bool,
    version: i32,
    start_height: i32,
    user_agent: String,
}

#[derive(Clone, Debug, Default, serde::Deserialize)]
struct RemoteMempoolVersionCount {
    version: i32,
    count: u64,
}

#[derive(Clone, Debug, Default, serde::Deserialize)]
struct RemoteMempoolAgeSecs {
    newest_secs: u64,
    median_secs: u64,
    oldest_secs: u64,
}

#[derive(Clone, Debug, Default, serde::Deserialize)]
struct RemoteMempoolSummary {
    size: u64,
    bytes: u64,
    fee_zero: u64,
    fee_nonzero: u64,
    versions: Vec<RemoteMempoolVersionCount>,
    age_secs: RemoteMempoolAgeSecs,
}

#[derive(Clone, Debug)]
struct WalletTxRow {
    txid: Hash256,
    received_at: u64,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
enum WalletAddressKind {
    TransparentReceive,
    TransparentChange,
    TransparentWatch,
    Sapling,
    SaplingWatch,
}

impl WalletAddressKind {
    fn label(self) -> &'static str {
        match self {
            Self::TransparentReceive => "t",
            Self::TransparentChange => "t (change)",
            Self::TransparentWatch => "t (watch-only)",
            Self::Sapling => "z",
            Self::SaplingWatch => "z (watch)",
        }
    }

    fn hidden_in_basic(self) -> bool {
        matches!(
            self,
            Self::TransparentChange | Self::TransparentWatch | Self::SaplingWatch
        )
    }
}

#[derive(Clone, Debug)]
struct WalletAddressRow {
    kind: WalletAddressKind,
    address: String,
    label: Option<String>,
    transparent_balance: Option<WalletBalanceBucket>,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
enum WalletModal {
    Send,
    ImportWatch,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
enum WalletSendField {
    To,
    Amount,
}

impl Default for WalletSendField {
    fn default() -> Self {
        Self::To
    }
}

#[derive(Clone, Debug, Default)]
struct WalletSendForm {
    to: String,
    amount: String,
    subtract_fee: bool,
    focus: WalletSendField,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
enum WalletImportWatchField {
    Address,
    Label,
}

impl Default for WalletImportWatchField {
    fn default() -> Self {
        Self::Address
    }
}

#[derive(Clone, Debug, Default)]
struct WalletImportWatchForm {
    address: String,
    label: String,
    focus: WalletImportWatchField,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
enum SaplingNoteOwnership {
    Spendable,
    WatchOnly,
}

#[derive(Clone, Debug)]
struct SaplingNoteSummary {
    ownership: SaplingNoteOwnership,
    value: i64,
    height: i32,
    nullifier: Hash256,
}

#[derive(Clone, Debug, Default)]
struct WalletBalanceBucket {
    confirmed: i64,
    unconfirmed: i64,
    immature: i64,
}

#[derive(Clone, Debug)]
struct SetupWizard {
    data_dir: PathBuf,
    active_data_dir: PathBuf,
    conf_path: PathBuf,
    data_sets: Vec<PathBuf>,
    data_set_index: usize,
    delete_confirm: Option<(PathBuf, Instant)>,
    network: Network,
    profile: RunProfile,
    header_lead: i32,
    rpc_user: String,
    rpc_pass: String,
    show_pass: bool,
    status: Option<String>,
}

impl SetupWizard {
    fn new(data_dir: PathBuf, conf_path: PathBuf, network: Network, header_lead: i32) -> Self {
        let mut wizard = Self {
            data_dir: data_dir.clone(),
            active_data_dir: data_dir,
            conf_path,
            data_sets: Vec::new(),
            data_set_index: 0,
            delete_confirm: None,
            network,
            profile: RunProfile::Default,
            header_lead: header_lead.max(0),
            rpc_user: String::new(),
            rpc_pass: String::new(),
            show_pass: false,
            status: None,
        };
        wizard.refresh_data_sets();
        if wizard.rpc_user.is_empty() || wizard.rpc_pass.is_empty() {
            wizard.regenerate_auth();
        }
        wizard
    }

    fn cycle_network(&mut self) {
        self.network = match self.network {
            Network::Mainnet => Network::Testnet,
            Network::Testnet => Network::Regtest,
            Network::Regtest => Network::Mainnet,
        };
    }

    fn cycle_profile(&mut self) {
        self.profile = match self.profile {
            RunProfile::Low => RunProfile::Default,
            RunProfile::Default => RunProfile::High,
            RunProfile::High => RunProfile::Low,
        };
    }

    fn refresh_data_sets(&mut self) {
        let normalize = |path: PathBuf| fs::canonicalize(&path).unwrap_or(path);

        let canonical_active = normalize(self.active_data_dir.clone());
        let canonical_selected = normalize(self.data_dir.clone());

        let mut roots: Vec<PathBuf> = Vec::new();
        if let Some(parent) = canonical_active
            .parent()
            .filter(|path| !path.as_os_str().is_empty())
        {
            roots.push(parent.to_path_buf());
        }
        if let Ok(exe) = std::env::current_exe() {
            for ancestor in exe.ancestors() {
                if ancestor
                    .file_name()
                    .and_then(|value| value.to_str())
                    .is_some_and(|value| value == "target")
                {
                    if let Some(parent) = ancestor.parent() {
                        roots.push(parent.to_path_buf());
                    }
                    break;
                }
            }
        }
        if let Ok(cwd) = std::env::current_dir() {
            roots.push(cwd);
        }

        let mut root_set: HashSet<PathBuf> = HashSet::new();
        let mut unique_roots: Vec<PathBuf> = Vec::new();
        for root in roots {
            let root = normalize(root);
            if root_set.insert(root.clone()) {
                unique_roots.push(root);
            }
        }

        let mut seen: HashSet<PathBuf> = HashSet::new();
        let mut sets: Vec<PathBuf> = Vec::new();
        for root in unique_roots {
            if let Ok(entries) = fs::read_dir(&root) {
                for entry in entries.flatten() {
                    let path = entry.path();
                    if !path.is_dir() {
                        continue;
                    }
                    let name = path
                        .file_name()
                        .and_then(|value| value.to_str())
                        .unwrap_or("");
                    if !name.starts_with("data") {
                        continue;
                    }
                    let path = normalize(path);
                    if seen.insert(path.clone()) {
                        sets.push(path);
                    }
                }
            }
        }

        for extra in [canonical_active.clone(), canonical_selected.clone()] {
            if seen.insert(extra.clone()) {
                sets.push(extra);
            }
        }

        sets.sort_by(|left, right| {
            let left_name = left
                .file_name()
                .and_then(|value| value.to_str())
                .unwrap_or("")
                .to_string();
            let right_name = right
                .file_name()
                .and_then(|value| value.to_str())
                .unwrap_or("")
                .to_string();
            left_name
                .cmp(&right_name)
                .then_with(|| left.to_string_lossy().cmp(&right.to_string_lossy()))
        });
        self.data_sets = sets;
        self.active_data_dir = canonical_active;
        self.data_dir = canonical_selected;
        self.conf_path = self.data_dir.join("flux.conf");

        if let Some(index) = self
            .data_sets
            .iter()
            .position(|path| path == &self.data_dir)
        {
            self.data_set_index = index;
        } else if let Some(index) = self
            .data_sets
            .iter()
            .position(|path| path == &self.active_data_dir)
        {
            self.data_set_index = index;
        } else {
            self.data_set_index = 0;
        }

        if let Some((user, pass)) = self.read_rpc_from_conf() {
            self.rpc_user = user;
            self.rpc_pass = pass;
        }
        self.delete_confirm = None;
    }

    fn read_rpc_from_conf(&self) -> Option<(String, String)> {
        let Ok(Some(conf)) = crate::load_flux_conf(&self.conf_path) else {
            return None;
        };
        let user = conf
            .get("rpcuser")
            .and_then(|values| values.last())
            .cloned();
        let pass = conf
            .get("rpcpassword")
            .and_then(|values| values.last())
            .cloned();
        match (user, pass) {
            (Some(user), Some(pass)) => Some((user, pass)),
            _ => None,
        }
    }

    fn datadir_pointer_path(&self) -> PathBuf {
        self.active_data_dir
            .parent()
            .filter(|path| !path.as_os_str().is_empty())
            .unwrap_or_else(|| std::path::Path::new("."))
            .join(crate::DATADIR_POINTER_FILE_NAME)
    }

    fn write_datadir_pointer(&self) -> Result<(), String> {
        let pointer_path = self.datadir_pointer_path();
        let contents = format!("{}\n", self.data_dir.display());
        fs::write(&pointer_path, contents)
            .map_err(|err| format!("failed to write {}: {err}", pointer_path.display()))?;
        Ok(())
    }

    fn apply_selected_data_set(&mut self) -> Result<(), String> {
        let Some(path) = self.data_sets.get(self.data_set_index).cloned() else {
            return Ok(());
        };
        let path = fs::canonicalize(&path).unwrap_or(path);
        self.data_dir = path;
        self.conf_path = self.data_dir.join("flux.conf");
        if let Some((user, pass)) = self.read_rpc_from_conf() {
            self.rpc_user = user;
            self.rpc_pass = pass;
        }
        self.write_datadir_pointer()?;
        self.delete_confirm = None;
        if self.data_dir == self.active_data_dir {
            self.status = Some("Selected active data dir".to_string());
        } else {
            self.status = Some(format!(
                "Default data dir set to {} (restart fluxd)",
                self.data_dir.display()
            ));
        }
        Ok(())
    }

    fn move_data_set(&mut self, delta: i32) {
        if self.data_sets.is_empty() {
            return;
        }
        let len = self.data_sets.len();
        let current = self.data_set_index as i32;
        let next = (current + delta).clamp(0, (len - 1) as i32) as usize;
        if next != self.data_set_index {
            self.data_set_index = next;
            if let Some(path) = self.data_sets.get(next) {
                self.status = Some(format!("Highlight {} (Enter to select)", path.display()));
            }
        }
        self.delete_confirm = None;
    }

    fn write_config_for_data_dir(&mut self, data_dir: PathBuf) -> Result<(), String> {
        let saved_data_dir = self.data_dir.clone();
        let saved_conf_path = self.conf_path.clone();
        let saved_status = self.status.clone();

        self.data_dir = data_dir;
        self.conf_path = self.data_dir.join("flux.conf");

        let result = if self.conf_path.exists() {
            Ok(())
        } else {
            self.write_config()
        };

        self.data_dir = saved_data_dir;
        self.conf_path = saved_conf_path;
        self.status = saved_status;
        result
    }

    fn create_new_data_set(&mut self) -> Result<(), String> {
        let root = self
            .active_data_dir
            .parent()
            .filter(|path| !path.as_os_str().is_empty())
            .unwrap_or_else(|| std::path::Path::new("."));

        let base = "data-new";
        let mut suffix = 0u32;
        let target = loop {
            let name = if suffix == 0 {
                base.to_string()
            } else {
                format!("{base}{suffix}")
            };
            let candidate = root.join(name);
            if !candidate.exists() {
                break candidate;
            }
            suffix = suffix.saturating_add(1);
            if suffix > 10_000 {
                return Err("too many data-new* dirs exist".to_string());
            }
        };

        fs::create_dir_all(&target)
            .map_err(|err| format!("failed to create {}: {err}", target.display()))?;

        let canonical_target = fs::canonicalize(&target).unwrap_or(target);
        self.write_config_for_data_dir(canonical_target.clone())?;
        self.refresh_data_sets();
        if let Some(index) = self
            .data_sets
            .iter()
            .position(|path| path == &canonical_target)
        {
            self.data_set_index = index;
        }
        self.status = Some(format!(
            "Created {} with starter flux.conf (Enter to select)",
            canonical_target.display()
        ));
        Ok(())
    }

    fn request_delete_selected(&mut self) -> Result<(), String> {
        let Some(target) = self.data_sets.get(self.data_set_index).cloned() else {
            return Ok(());
        };
        if target == self.active_data_dir {
            self.status = Some("Cannot delete active data dir".to_string());
            return Ok(());
        }
        let now = Instant::now();
        if let Some((pending, since)) = self.delete_confirm.as_ref() {
            if pending == &target && now.duration_since(*since) <= Duration::from_secs(5) {
                fs::remove_dir_all(&target).map_err(|err| format!("delete failed: {err}"))?;
                if target == self.data_dir {
                    self.data_dir = self.active_data_dir.clone();
                    self.conf_path = self.data_dir.join("flux.conf");
                    if let Some((user, pass)) = self.read_rpc_from_conf() {
                        self.rpc_user = user;
                        self.rpc_pass = pass;
                    }
                    let _ = self.write_datadir_pointer();
                }
                self.status = Some(format!("Deleted {}", target.display()));
                self.delete_confirm = None;
                self.refresh_data_sets();
                return Ok(());
            }
        }
        self.delete_confirm = Some((target.clone(), now));
        self.status = Some(format!("Press d again to delete {}", target.display()));
        Ok(())
    }

    fn toggle_header_lead_unlimited(&mut self) {
        if self.header_lead == 0 {
            self.header_lead = crate::DEFAULT_HEADER_LEAD;
        } else {
            self.header_lead = 0;
        }
    }

    fn adjust_header_lead(&mut self, delta: i32) {
        if self.header_lead == 0 {
            if delta <= 0 {
                return;
            }
            self.header_lead = crate::DEFAULT_HEADER_LEAD;
        }
        let next = self.header_lead.saturating_add(delta);
        self.header_lead = next.max(0);
    }

    fn regenerate_auth(&mut self) {
        let mut rng = rand::thread_rng();
        let user_suffix: String = (&mut rng)
            .sample_iter(&Alphanumeric)
            .take(6)
            .map(char::from)
            .collect();
        let pass: String = (&mut rng)
            .sample_iter(&Alphanumeric)
            .take(32)
            .map(char::from)
            .collect();
        self.rpc_user = format!("rpc{user_suffix}");
        self.rpc_pass = pass;
        self.status = None;
    }

    fn toggle_pass_visible(&mut self) {
        self.show_pass = !self.show_pass;
    }

    fn masked_pass(&self) -> String {
        if self.show_pass {
            return self.rpc_pass.clone();
        }
        if self.rpc_pass.is_empty() {
            return "-".to_string();
        }
        let shown = self.rpc_pass.chars().take(4).collect::<String>();
        format!("{shown}…")
    }

    fn write_config(&mut self) -> Result<(), String> {
        if let Some(conf_dir) = self
            .conf_path
            .parent()
            .filter(|path| !path.as_os_str().is_empty())
        {
            fs::create_dir_all(conf_dir)
                .map_err(|err| format!("failed to create conf dir: {err}"))?;
        }
        fs::create_dir_all(&self.data_dir)
            .map_err(|err| format!("failed to create data dir: {err}"))?;

        let mut contents = String::new();
        writeln!(&mut contents, "# fluxd-rust configuration file").ok();
        writeln!(
            &mut contents,
            "# Generated by the in-process TUI setup wizard."
        )
        .ok();
        writeln!(&mut contents, "").ok();
        writeln!(&mut contents, "profile={}", self.profile.as_str()).ok();
        match self.network {
            Network::Mainnet => {}
            Network::Testnet => {
                writeln!(&mut contents, "testnet=1").ok();
            }
            Network::Regtest => {
                writeln!(&mut contents, "regtest=1").ok();
            }
        }
        writeln!(&mut contents, "headerlead={}", self.header_lead).ok();
        writeln!(&mut contents, "").ok();
        writeln!(&mut contents, "rpcuser={}", self.rpc_user).ok();
        writeln!(&mut contents, "rpcpassword={}", self.rpc_pass).ok();
        writeln!(&mut contents, "rpcbind=127.0.0.1").ok();
        writeln!(
            &mut contents,
            "rpcport={}",
            crate::default_rpc_addr(self.network).port()
        )
        .ok();
        writeln!(&mut contents, "rpcallowip=127.0.0.1").ok();

        let mut backup: Option<PathBuf> = None;
        if self.conf_path.exists() {
            let suffix = unix_seconds();
            let backup_name = format!("flux.conf.bak.{suffix}");
            let backup_dir = self
                .conf_path
                .parent()
                .filter(|path| !path.as_os_str().is_empty())
                .unwrap_or(self.data_dir.as_path());
            let backup_path = backup_dir.join(backup_name);
            if let Err(_err) = fs::rename(&self.conf_path, &backup_path) {
                fs::copy(&self.conf_path, &backup_path)
                    .map_err(|err| format!("failed to backup existing flux.conf: {err}"))?;
                fs::remove_file(&self.conf_path)
                    .map_err(|err| format!("failed to remove old flux.conf: {err}"))?;
            }
            backup = Some(backup_path);
        }

        fs::write(&self.conf_path, contents)
            .map_err(|err| format!("failed to write flux.conf: {err}"))?;
        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt;
            let _ = fs::set_permissions(&self.conf_path, fs::Permissions::from_mode(0o600));
        }

        self.status = Some(match backup {
            Some(path) => format!(
                "Wrote {} (backed up previous to {}). Restart the daemon to apply.",
                self.conf_path.display(),
                path.display()
            ),
            None => format!(
                "Wrote {}. Restart the daemon to apply.",
                self.conf_path.display()
            ),
        });
        Ok(())
    }
}

struct RatePoint {
    t: f64,
    value: f64,
}

struct RateHistory {
    points: VecDeque<RatePoint>,
    capacity: usize,
}

impl RateHistory {
    fn new(capacity: usize) -> Self {
        Self {
            points: VecDeque::with_capacity(capacity),
            capacity,
        }
    }

    fn push(&mut self, point: RatePoint) {
        if self.points.len() == self.capacity {
            let _ = self.points.pop_front();
        }
        self.points.push_back(point);
    }

    fn as_vec(&self) -> Vec<(f64, f64)> {
        self.points
            .iter()
            .map(|point| (point.t, point.value))
            .collect()
    }
}

struct TuiState {
    screen: Screen,
    help_return: Screen,
    setup_return: Screen,
    advanced: bool,
    is_remote: bool,
    last_snapshot: Option<StatsSnapshot>,
    last_rate_snapshot: Option<StatsSnapshot>,
    last_error: Option<String>,
    startup_status: Option<String>,
    blocks_per_sec: Option<f64>,
    headers_per_sec: Option<f64>,
    orphan_count: Option<usize>,
    orphan_bytes: Option<usize>,
    mempool_detail: Option<RemoteMempoolSummary>,
    logs_min_level: logging::Level,
    logs_follow: bool,
    logs_paused: bool,
    logs_scroll: u16,
    logs: Vec<logging::CapturedLog>,
    wallet_encrypted: Option<bool>,
    wallet_unlocked_until: Option<u64>,
    wallet_key_count: Option<usize>,
    wallet_keypool_size: Option<usize>,
    wallet_tx_count: Option<usize>,
    wallet_pay_tx_fee_per_kb: Option<i64>,
    wallet_has_sapling_keys: Option<bool>,
    wallet_transparent: Option<WalletBalanceBucket>,
    wallet_transparent_watchonly: Option<WalletBalanceBucket>,
    wallet_sapling_spendable: Option<i64>,
    wallet_sapling_watchonly: Option<i64>,
    wallet_sapling_scan_height: Option<i32>,
    wallet_sapling_note_count: Option<usize>,
    wallet_recent_txs: Vec<WalletTxRow>,
    wallet_pending_ops: Vec<crate::rpc::AsyncOpSnapshot>,
    wallet_detail_error: Option<String>,
    wallet_addresses: Vec<WalletAddressRow>,
    wallet_selected_address: usize,
    wallet_show_qr: bool,
    wallet_qr_expanded: bool,
    wallet_status: Option<String>,
    wallet_force_refresh: bool,
    wallet_select_after_refresh: Option<String>,
    wallet_modal: Option<WalletModal>,
    wallet_send_form: WalletSendForm,
    wallet_import_watch_form: WalletImportWatchForm,
    setup: Option<SetupWizard>,
    bps_history: RateHistory,
    hps_history: RateHistory,
    peers_scroll: u16,
    remote_peers: Vec<RemotePeerInfo>,
    remote_net_totals: Option<RemoteNetTotals>,
    command_mode: bool,
    command_input: String,
    command_selected: usize,
    command_status: Option<String>,
    mouse_capture: bool,
    last_ctrl_c: Option<Instant>,
}

impl TuiState {
    fn new() -> Self {
        Self {
            screen: Screen::Monitor,
            help_return: Screen::Monitor,
            setup_return: Screen::Monitor,
            advanced: false,
            is_remote: false,
            last_snapshot: None,
            last_rate_snapshot: None,
            last_error: None,
            startup_status: None,
            blocks_per_sec: None,
            headers_per_sec: None,
            orphan_count: None,
            orphan_bytes: None,
            mempool_detail: None,
            logs_min_level: logging::Level::Info,
            logs_follow: true,
            logs_paused: false,
            logs_scroll: 0,
            logs: Vec::new(),
            wallet_encrypted: None,
            wallet_unlocked_until: None,
            wallet_key_count: None,
            wallet_keypool_size: None,
            wallet_tx_count: None,
            wallet_pay_tx_fee_per_kb: None,
            wallet_has_sapling_keys: None,
            wallet_transparent: None,
            wallet_transparent_watchonly: None,
            wallet_sapling_spendable: None,
            wallet_sapling_watchonly: None,
            wallet_sapling_scan_height: None,
            wallet_sapling_note_count: None,
            wallet_recent_txs: Vec::new(),
            wallet_pending_ops: Vec::new(),
            wallet_detail_error: None,
            wallet_addresses: Vec::new(),
            wallet_selected_address: 0,
            wallet_show_qr: true,
            wallet_qr_expanded: false,
            wallet_status: None,
            wallet_force_refresh: false,
            wallet_select_after_refresh: None,
            wallet_modal: None,
            wallet_send_form: WalletSendForm::default(),
            wallet_import_watch_form: WalletImportWatchForm::default(),
            setup: None,
            bps_history: RateHistory::new(HISTORY_SAMPLES),
            hps_history: RateHistory::new(HISTORY_SAMPLES),
            peers_scroll: 0,
            remote_peers: Vec::new(),
            remote_net_totals: None,
            command_mode: false,
            command_input: String::new(),
            command_selected: 0,
            command_status: None,
            mouse_capture: false,
            last_ctrl_c: None,
        }
    }

    fn toggle_help(&mut self) {
        match self.screen {
            Screen::Help => {
                self.screen = self.help_return;
            }
            other => {
                self.help_return = other;
                self.screen = Screen::Help;
            }
        }
    }

    fn toggle_setup(&mut self) {
        match self.screen {
            Screen::Setup => {
                self.screen = self.setup_return;
            }
            other => {
                self.setup_return = other;
                self.screen = Screen::Setup;
                if let Some(setup) = self.setup.as_mut() {
                    setup.refresh_data_sets();
                }
            }
        };
    }

    fn cycle_screen(&mut self) {
        self.screen = match self.screen {
            Screen::Monitor => Screen::Stats,
            Screen::Stats => Screen::Peers,
            Screen::Peers => Screen::Db,
            Screen::Db => Screen::Mempool,
            Screen::Mempool => Screen::Wallet,
            Screen::Wallet => Screen::Logs,
            Screen::Logs => Screen::Monitor,
            Screen::Setup => self.setup_return,
            Screen::Help => self.help_return,
        };
    }

    fn cycle_screen_reverse(&mut self) {
        self.screen = match self.screen {
            Screen::Monitor => Screen::Logs,
            Screen::Stats => Screen::Monitor,
            Screen::Peers => Screen::Stats,
            Screen::Db => Screen::Peers,
            Screen::Mempool => Screen::Db,
            Screen::Wallet => Screen::Mempool,
            Screen::Logs => Screen::Wallet,
            Screen::Setup => self.setup_return,
            Screen::Help => self.help_return,
        };
    }

    fn toggle_advanced(&mut self) {
        self.advanced = !self.advanced;
        self.wallet_clamp_selection();
    }

    fn update_snapshot(&mut self, snapshot: StatsSnapshot) {
        let (headers_per_sec, blocks_per_sec) = match self.last_rate_snapshot.as_ref() {
            Some(prev) => {
                let dt = snapshot.unix_time_secs.saturating_sub(prev.unix_time_secs);
                if dt == 0 {
                    (None, None)
                } else {
                    let headers_delta = snapshot.header_count.saturating_sub(prev.header_count);
                    let blocks_delta = snapshot.block_count.saturating_sub(prev.block_count);
                    (
                        Some(headers_delta as f64 / dt as f64),
                        Some(blocks_delta as f64 / dt as f64),
                    )
                }
            }
            None => (None, None),
        };

        self.headers_per_sec = headers_per_sec;
        self.blocks_per_sec = blocks_per_sec;

        let t = snapshot.uptime_secs as f64;
        if let Some(value) = blocks_per_sec {
            self.bps_history.push(RatePoint { t, value });
        }
        if let Some(value) = headers_per_sec {
            self.hps_history.push(RatePoint { t, value });
        }

        self.last_rate_snapshot = Some(snapshot.clone());
        self.last_snapshot = Some(snapshot);
        self.last_error = None;
    }

    fn update_error(&mut self, err: String) {
        self.last_error = Some(err);
    }

    fn update_orphans(&mut self, orphan_count: Option<usize>, orphan_bytes: Option<usize>) {
        self.orphan_count = orphan_count;
        self.orphan_bytes = orphan_bytes;
    }

    fn update_wallet(
        &mut self,
        wallet_encrypted: Option<bool>,
        wallet_unlocked_until: Option<u64>,
        wallet_key_count: Option<usize>,
        wallet_keypool_size: Option<usize>,
        wallet_tx_count: Option<usize>,
        wallet_pay_tx_fee_per_kb: Option<i64>,
        wallet_has_sapling_keys: Option<bool>,
    ) {
        self.wallet_encrypted = wallet_encrypted;
        self.wallet_unlocked_until = wallet_unlocked_until;
        self.wallet_key_count = wallet_key_count;
        self.wallet_keypool_size = wallet_keypool_size;
        self.wallet_tx_count = wallet_tx_count;
        self.wallet_pay_tx_fee_per_kb = wallet_pay_tx_fee_per_kb;
        self.wallet_has_sapling_keys = wallet_has_sapling_keys;
    }

    fn wallet_visible_indices(&self) -> Vec<usize> {
        self.wallet_addresses
            .iter()
            .enumerate()
            .filter_map(|(idx, row)| {
                if self.advanced {
                    return Some(idx);
                }
                if !row.kind.hidden_in_basic() {
                    return Some(idx);
                }
                match row.kind {
                    WalletAddressKind::TransparentChange | WalletAddressKind::TransparentWatch => {
                        let total = row
                            .transparent_balance
                            .as_ref()
                            .map(|bucket| {
                                bucket
                                    .confirmed
                                    .saturating_add(bucket.unconfirmed)
                                    .saturating_add(bucket.immature)
                            })
                            .unwrap_or(0);
                        if total != 0 {
                            Some(idx)
                        } else {
                            None
                        }
                    }
                    _ => None,
                }
            })
            .collect()
    }

    fn wallet_clamp_selection(&mut self) {
        let visible = self.wallet_visible_indices();
        if visible.is_empty() {
            self.wallet_selected_address = 0;
            return;
        }
        if visible
            .iter()
            .any(|idx| *idx == self.wallet_selected_address)
        {
            return;
        }
        self.wallet_selected_address = visible[0];
    }

    fn wallet_move_selection(&mut self, delta: isize) {
        let visible = self.wallet_visible_indices();
        if visible.is_empty() {
            return;
        }
        let current_pos = visible
            .iter()
            .position(|idx| *idx == self.wallet_selected_address)
            .unwrap_or(0) as isize;
        let max_pos = visible.len().saturating_sub(1) as isize;
        let next_pos = (current_pos + delta).clamp(0, max_pos) as usize;
        self.wallet_selected_address = visible[next_pos];
    }

    fn update_wallet_addresses(&mut self, addresses: Vec<WalletAddressRow>) {
        self.wallet_addresses = addresses;

        if let Some(target) = self.wallet_select_after_refresh.take() {
            if let Some(found) = self
                .wallet_addresses
                .iter()
                .position(|row| row.address == target)
            {
                self.wallet_selected_address = found;
            }
        }

        self.wallet_clamp_selection();
    }

    fn update_logs(&mut self, logs: Vec<logging::CapturedLog>) {
        self.logs = logs;
    }

    fn cycle_logs_min_level(&mut self) {
        self.logs_min_level = match self.logs_min_level {
            logging::Level::Error => logging::Level::Warn,
            logging::Level::Warn => logging::Level::Info,
            logging::Level::Info => logging::Level::Debug,
            logging::Level::Debug => logging::Level::Trace,
            logging::Level::Trace => logging::Level::Error,
        };
    }

    fn toggle_logs_pause(&mut self) {
        self.logs_paused = !self.logs_paused;
        if self.logs_paused {
            self.logs_follow = false;
        } else {
            self.logs_follow = true;
            self.logs_scroll = 0;
        }
    }
}

fn set_mouse_capture(enabled: bool) -> Result<(), String> {
    let mut stdout = io::stdout();
    if enabled {
        execute!(stdout, EnableMouseCapture).map_err(|err| err.to_string())?;
    } else {
        execute!(stdout, DisableMouseCapture).map_err(|err| err.to_string())?;
    }
    Ok(())
}

struct TerminalGuard;

impl TerminalGuard {
    fn enter() -> Result<Self, String> {
        enable_raw_mode().map_err(|err| err.to_string())?;
        let mut stdout = io::stdout();
        execute!(stdout, EnterAlternateScreen, Hide, EnableMouseCapture)
            .map_err(|err| err.to_string())?;
        Ok(Self)
    }
}

impl Drop for TerminalGuard {
    fn drop(&mut self) {
        let _ = disable_raw_mode();
        let mut stdout = io::stdout();
        let _ = execute!(stdout, Show, LeaveAlternateScreen, DisableMouseCapture);
    }
}

pub struct TuiInit {
    pub chainstate: Arc<ChainState<Store>>,
    pub store: Arc<Store>,
    pub sync_metrics: Arc<SyncMetrics>,
    pub header_metrics: Arc<HeaderMetrics>,
    pub validation_metrics: Arc<ValidationMetrics>,
    pub connect_metrics: Arc<ConnectMetrics>,
    pub mempool: Arc<Mutex<Mempool>>,
    pub mempool_policy: Arc<MempoolPolicy>,
    pub mempool_metrics: Arc<MempoolMetrics>,
    pub fee_estimator: Arc<Mutex<FeeEstimator>>,
    pub tx_confirm_target: u32,
    pub mempool_flags_rx: Receiver<ValidationFlags>,
    pub wallet: Arc<Mutex<Wallet>>,
    pub tx_announce: broadcast::Sender<Hash256>,
}

pub fn run_tui(
    data_dir: PathBuf,
    conf_path: PathBuf,
    start_in_setup: bool,
    header_lead: i32,
    peer_registry: Arc<PeerRegistry>,
    net_totals: Arc<NetTotals>,
    chain_params: Arc<ChainParams>,
    network: Network,
    storage_backend: Backend,
    start_time: Instant,
    shutdown_rx: watch::Receiver<bool>,
    shutdown_tx: watch::Sender<bool>,
    init_rx: Receiver<TuiInit>,
) -> Result<(), String> {
    logging::set_stderr_enabled(false);
    let _guard = match TerminalGuard::enter() {
        Ok(guard) => guard,
        Err(err) => {
            logging::set_stderr_enabled(true);
            return Err(err);
        }
    };
    let stdout = io::stdout();

    let term_backend = CrosstermBackend::new(stdout);
    let mut terminal = Terminal::new(term_backend).map_err(|err| err.to_string())?;
    terminal.clear().map_err(|err| err.to_string())?;

    let mut state = TuiState::new();
    state.setup = Some(SetupWizard::new(data_dir, conf_path, network, header_lead));
    if start_in_setup {
        state.screen = Screen::Setup;
        state.setup_return = Screen::Monitor;
    }
    set_mouse_capture(state.mouse_capture)?;
    let mut next_sample = Instant::now();
    let mut next_wallet_refresh = Instant::now();
    let mut runtime: Option<TuiInit> = None;
    let mut mempool_flags: Option<ValidationFlags> = None;
    state.startup_status = Some("Startup: opening database...".to_string());

    loop {
        if *shutdown_rx.borrow() {
            break;
        }

        if runtime.is_none() {
            match init_rx.try_recv() {
                Ok(init) => {
                    runtime = Some(init);
                    state.startup_status = Some("Startup: loading shielded params...".to_string());
                }
                Err(TryRecvError::Disconnected) => {
                    state.update_error("Startup: init channel disconnected".to_string());
                }
                Err(TryRecvError::Empty) => {}
            }
        }

        if mempool_flags.is_none() {
            if let Some(runtime) = runtime.as_ref() {
                if let Ok(flags) = runtime.mempool_flags_rx.try_recv() {
                    mempool_flags = Some(flags);
                    state.startup_status = None;
                }
            }
        }

        let now = Instant::now();
        if now >= next_sample {
            if let Some(runtime) = runtime.as_ref() {
                match stats::snapshot_stats(
                    runtime.chainstate.as_ref(),
                    Some(runtime.store.as_ref()),
                    network,
                    storage_backend,
                    start_time,
                    Some(runtime.sync_metrics.as_ref()),
                    Some(runtime.header_metrics.as_ref()),
                    Some(runtime.validation_metrics.as_ref()),
                    Some(runtime.connect_metrics.as_ref()),
                    Some(runtime.mempool.as_ref()),
                    Some(runtime.mempool_metrics.as_ref()),
                ) {
                    Ok(snapshot) => {
                        state.update_snapshot(snapshot);
                        let (orphan_count, orphan_bytes) = match runtime.mempool.lock() {
                            Ok(guard) => (Some(guard.orphan_count()), Some(guard.orphan_bytes())),
                            Err(_) => (None, None),
                        };
                        state.update_orphans(orphan_count, orphan_bytes);
                        if matches!(state.screen, Screen::Mempool) && state.advanced {
                            state.mempool_detail = compute_mempool_detail(runtime.mempool.as_ref());
                        } else {
                            state.mempool_detail = None;
                        }

                        let wallet_snapshot = match runtime.wallet.lock() {
                            Ok(mut guard) => (
                                Some(guard.is_encrypted()),
                                Some(guard.unlocked_until()),
                                Some(guard.key_count()),
                                Some(guard.keypool_size()),
                                Some(guard.tx_count()),
                                Some(guard.pay_tx_fee_per_kb()),
                                Some(guard.has_sapling_keys()),
                            ),
                            Err(_) => (None, None, None, None, None, None, None),
                        };
                        state.update_wallet(
                            wallet_snapshot.0,
                            wallet_snapshot.1,
                            wallet_snapshot.2,
                            wallet_snapshot.3,
                            wallet_snapshot.4,
                            wallet_snapshot.5,
                            wallet_snapshot.6,
                        );
                    }
                    Err(err) => {
                        state.update_error(err);
                        state.update_orphans(None, None);
                        state.update_wallet(None, None, None, None, None, None, None);
                    }
                }
            }

            if matches!(state.screen, Screen::Logs) && !state.logs_paused {
                state.update_logs(logging::capture_snapshot(LOG_SNAPSHOT_LIMIT));
            }
            if matches!(state.screen, Screen::Wallet)
                && (now >= next_wallet_refresh || state.wallet_force_refresh)
            {
                let tip_height = state
                    .last_snapshot
                    .as_ref()
                    .map(|snap| snap.best_block_height);
                if let Some(runtime) = runtime.as_ref() {
                    match refresh_wallet_details(
                        runtime.chainstate.as_ref(),
                        runtime.mempool.as_ref(),
                        runtime.wallet.as_ref(),
                        tip_height,
                    ) {
                        Ok(details) => {
                            state.wallet_transparent = Some(details.transparent_owned);
                            state.wallet_transparent_watchonly =
                                Some(details.transparent_watchonly);
                            state.wallet_sapling_spendable = Some(details.sapling_spendable);
                            state.wallet_sapling_watchonly = Some(details.sapling_watchonly);
                            state.wallet_sapling_scan_height = Some(details.sapling_scan_height);
                            state.wallet_sapling_note_count = Some(details.sapling_note_count);
                            state.update_wallet_addresses(details.addresses);
                            state.wallet_recent_txs = details.recent_txs;
                            state.wallet_pending_ops = details.pending_ops;
                            state.wallet_detail_error = None;
                        }
                        Err(err) => {
                            state.wallet_transparent = None;
                            state.wallet_transparent_watchonly = None;
                            state.wallet_sapling_spendable = None;
                            state.wallet_sapling_watchonly = None;
                            state.wallet_sapling_scan_height = None;
                            state.wallet_sapling_note_count = None;
                            state.wallet_addresses.clear();
                            state.wallet_recent_txs.clear();
                            state.wallet_pending_ops.clear();
                            state.wallet_detail_error = Some(err);
                        }
                    }
                } else {
                    state.wallet_detail_error = Some(wallet_ops_unavailable(state.is_remote));
                }
                state.wallet_force_refresh = false;
                next_wallet_refresh = now + WALLET_REFRESH_INTERVAL;
            }
            next_sample = now + SAMPLE_INTERVAL;
        }

        terminal
            .draw(|frame| draw(frame, &state, peer_registry.as_ref(), net_totals.as_ref()))
            .map_err(|err| err.to_string())?;

        if event::poll(UI_TICK).map_err(|err| err.to_string())? {
            match event::read().map_err(|err| err.to_string())? {
                Event::Key(key) => {
                    if key.kind == KeyEventKind::Press {
                        let wallet_ops = if state.is_remote {
                            None
                        } else {
                            runtime.as_ref().and_then(|runtime| {
                                mempool_flags.as_ref().map(|flags| InProcessWalletOps {
                                    chainstate: runtime.chainstate.as_ref(),
                                    mempool: runtime.mempool.as_ref(),
                                    mempool_policy: runtime.mempool_policy.as_ref(),
                                    mempool_metrics: runtime.mempool_metrics.as_ref(),
                                    fee_estimator: runtime.fee_estimator.as_ref(),
                                    tx_confirm_target: runtime.tx_confirm_target,
                                    mempool_flags: flags,
                                    wallet: runtime.wallet.as_ref(),
                                    chain_params: chain_params.as_ref(),
                                    tx_announce: &runtime.tx_announce,
                                })
                            })
                        };
                        if handle_key(key, &mut state, &shutdown_tx, wallet_ops.as_ref())? {
                            break;
                        }
                    }
                }
                Event::Mouse(event) => {
                    let size = terminal.size().map_err(|err| err.to_string())?;
                    let area = Rect::new(0, 0, size.width, size.height);
                    if handle_mouse(event, &mut state, area)? {
                        break;
                    }
                }
                Event::Resize(_, _) => {
                    terminal.clear().map_err(|err| err.to_string())?;
                }
                _ => {}
            }
        }
    }

    terminal.show_cursor().map_err(|err| err.to_string())?;
    logging::set_stderr_enabled(true);
    Ok(())
}

pub fn run_remote_tui(endpoint: String) -> Result<(), String> {
    let endpoint = endpoint.trim().to_string();
    if endpoint.is_empty() {
        return Err("missing --tui-attach endpoint".to_string());
    }

    logging::set_stderr_enabled(false);
    let _guard = TerminalGuard::enter()?;
    let stdout = io::stdout();
    let term_backend = CrosstermBackend::new(stdout);
    let mut terminal = Terminal::new(term_backend).map_err(|err| err.to_string())?;
    terminal.clear().map_err(|err| err.to_string())?;

    let (shutdown_tx, shutdown_rx) = watch::channel(false);
    let peer_registry = PeerRegistry::default();
    let net_totals = NetTotals::default();

    let mut state = TuiState::new();
    state.is_remote = true;
    state.logs_min_level = logging::Level::Warn;
    let mut next_sample = Instant::now();
    let mut next_peers_refresh = Instant::now();

    loop {
        if *shutdown_rx.borrow() {
            break;
        }

        let now = Instant::now();
        if now >= next_sample {
            match fetch_remote_stats_snapshot(&endpoint) {
                Ok(snapshot) => {
                    state.update_snapshot(snapshot);
                }
                Err(err) => {
                    state.update_error(err);
                }
            }
            next_sample = now + SAMPLE_INTERVAL;
        }

        if now >= next_peers_refresh {
            if let Ok(peers) = fetch_remote_peers(&endpoint) {
                state.remote_peers = peers;
            }
            if let Ok(totals) = fetch_remote_net_totals(&endpoint) {
                state.remote_net_totals = Some(totals);
            }
            if matches!(state.screen, Screen::Mempool) && state.advanced {
                if let Ok(detail) = fetch_remote_mempool(&endpoint) {
                    state.mempool_detail = Some(detail);
                }
            } else {
                state.mempool_detail = None;
            }
            next_peers_refresh = now + Duration::from_secs(2);
        }

        terminal
            .draw(|frame| draw(frame, &state, &peer_registry, &net_totals))
            .map_err(|err| err.to_string())?;

        if event::poll(UI_TICK).map_err(|err| err.to_string())? {
            match event::read().map_err(|err| err.to_string())? {
                Event::Key(key) => {
                    if key.kind == KeyEventKind::Press {
                        if handle_key(key, &mut state, &shutdown_tx, None)? {
                            break;
                        }
                    }
                }
                Event::Mouse(event) => {
                    let size = terminal.size().map_err(|err| err.to_string())?;
                    let area = Rect::new(0, 0, size.width, size.height);
                    if handle_mouse(event, &mut state, area)? {
                        break;
                    }
                }
                Event::Resize(_, _) => {
                    terminal.clear().map_err(|err| err.to_string())?;
                }
                _ => {}
            }
        }
    }

    terminal.show_cursor().map_err(|err| err.to_string())?;
    logging::set_stderr_enabled(true);
    Ok(())
}

fn fetch_remote_stats_snapshot(endpoint: &str) -> Result<StatsSnapshot, String> {
    fetch_remote_json(endpoint, "/stats")
}

fn fetch_remote_peers(endpoint: &str) -> Result<Vec<RemotePeerInfo>, String> {
    fetch_remote_json(endpoint, "/peers")
}

fn fetch_remote_net_totals(endpoint: &str) -> Result<RemoteNetTotals, String> {
    fetch_remote_json(endpoint, "/nettotals")
}

fn fetch_remote_mempool(endpoint: &str) -> Result<RemoteMempoolSummary, String> {
    fetch_remote_json(endpoint, "/mempool")
}

fn fetch_remote_json<T: DeserializeOwned>(endpoint: &str, path: &str) -> Result<T, String> {
    let endpoint = endpoint.trim();
    let endpoint = endpoint
        .strip_prefix("http://")
        .or_else(|| endpoint.strip_prefix("https://"))
        .unwrap_or(endpoint);
    let endpoint = endpoint.trim_end_matches('/');

    if path.is_empty() || !path.starts_with('/') {
        return Err("invalid remote path".to_string());
    }

    let (host, port) = endpoint
        .rsplit_once(':')
        .and_then(|(host, port)| port.parse::<u16>().ok().map(|port| (host, port)))
        .unwrap_or((endpoint, 8080));
    if host.trim().is_empty() {
        return Err("invalid --tui-attach endpoint".to_string());
    }

    let addr = format!("{host}:{port}");
    let mut stream = TcpStream::connect(&addr).map_err(|err| format!("connect {addr}: {err}"))?;
    let _ = stream.set_read_timeout(Some(Duration::from_secs(2)));
    let _ = stream.set_write_timeout(Some(Duration::from_secs(2)));

    let request = format!("GET {path} HTTP/1.1\r\nHost: {host}\r\nConnection: close\r\n\r\n");
    stream
        .write_all(request.as_bytes())
        .map_err(|err| format!("write request: {err}"))?;
    let mut response_bytes = Vec::new();
    stream
        .read_to_end(&mut response_bytes)
        .map_err(|err| format!("read response: {err}"))?;

    let response = String::from_utf8(response_bytes)
        .map_err(|_| "remote response not valid utf-8".to_string())?;
    let (head, body) = response
        .split_once("\r\n\r\n")
        .ok_or_else(|| "invalid http response".to_string())?;
    let status_line = head.lines().next().unwrap_or_default();
    if !status_line.contains("200") {
        return Err(format!("remote returned '{status_line}'"));
    }

    serde_json::from_str::<T>(body).map_err(|err| format!("invalid json: {err}"))
}

struct WalletDetails {
    transparent_owned: WalletBalanceBucket,
    transparent_watchonly: WalletBalanceBucket,
    sapling_spendable: i64,
    sapling_watchonly: i64,
    sapling_scan_height: i32,
    sapling_note_count: usize,
    addresses: Vec<WalletAddressRow>,
    recent_txs: Vec<WalletTxRow>,
    pending_ops: Vec<crate::rpc::AsyncOpSnapshot>,
}

fn refresh_wallet_details(
    chainstate: &ChainState<Store>,
    mempool: &Mutex<Mempool>,
    wallet: &Mutex<Wallet>,
    tip_height: Option<i32>,
) -> Result<WalletDetails, String> {
    let (
        scripts,
        watch_script_set,
        wallet_network,
        sapling_scan_height,
        sapling_note_count,
        sapling_notes,
        mut addresses,
    ) = {
        let guard = wallet
            .lock()
            .map_err(|_| "wallet lock poisoned".to_string())?;
        let scripts = guard
            .all_script_pubkeys_including_watchonly()
            .map_err(|err| err.to_string())?;
        let mut watch_scripts = HashSet::new();
        for script in &scripts {
            if guard.script_pubkey_is_watchonly(script) {
                watch_scripts.insert(script.clone());
            }
        }

        let sapling_scan_height = guard.sapling_scan_height();
        let sapling_note_count = guard.sapling_note_count();
        let wallet_network = guard.network();
        let mut sapling_notes = Vec::new();
        if guard.has_sapling_keys() {
            for note in guard.sapling_note_map().values() {
                let is_mine = guard
                    .sapling_address_is_mine(&note.address)
                    .map_err(|err| err.to_string())?;
                let ownership = if is_mine {
                    Some(SaplingNoteOwnership::Spendable)
                } else if guard
                    .sapling_address_is_watchonly(&note.address)
                    .map_err(|err| err.to_string())?
                {
                    Some(SaplingNoteOwnership::WatchOnly)
                } else {
                    None
                };
                let Some(ownership) = ownership else {
                    continue;
                };
                sapling_notes.push(SaplingNoteSummary {
                    ownership,
                    value: note.value,
                    height: note.height,
                    nullifier: note.nullifier,
                });
            }
        }

        let mut addresses = Vec::new();
        let transparent = guard
            .transparent_address_infos()
            .map_err(|err| err.to_string())?;
        for TransparentAddressInfo {
            address,
            label,
            is_change,
        } in transparent
        {
            addresses.push(WalletAddressRow {
                kind: if is_change {
                    WalletAddressKind::TransparentChange
                } else {
                    WalletAddressKind::TransparentReceive
                },
                address,
                label,
                transparent_balance: None,
            });
        }
        for script_pubkey in &watch_scripts {
            let Some(address) = script_pubkey_to_address(script_pubkey, wallet_network) else {
                continue;
            };
            let label = guard
                .label_for_script_pubkey(script_pubkey)
                .map(str::to_owned)
                .filter(|value| !value.is_empty());
            addresses.push(WalletAddressRow {
                kind: WalletAddressKind::TransparentWatch,
                address,
                label,
                transparent_balance: None,
            });
        }
        let sapling = guard
            .sapling_address_infos()
            .map_err(|err| err.to_string())?;
        for SaplingAddressInfo {
            address,
            is_watchonly,
        } in sapling
        {
            addresses.push(WalletAddressRow {
                kind: if is_watchonly {
                    WalletAddressKind::SaplingWatch
                } else {
                    WalletAddressKind::Sapling
                },
                address,
                label: None,
                transparent_balance: None,
            });
        }
        addresses.sort_by(|a, b| {
            wallet_address_kind_sort_key(a.kind)
                .cmp(&wallet_address_kind_sort_key(b.kind))
                .then_with(|| a.address.cmp(&b.address))
        });

        (
            scripts,
            watch_scripts,
            wallet_network,
            sapling_scan_height,
            sapling_note_count,
            sapling_notes,
            addresses,
        )
    };

    let utxos = collect_wallet_utxos(chainstate, mempool, &scripts, true)?;
    let mut owned = WalletBalanceBucket::default();
    let mut watch = WalletBalanceBucket::default();
    let mut balances_by_script: BTreeMap<Vec<u8>, WalletBalanceBucket> = BTreeMap::new();
    for utxo in &utxos {
        let bucket = if watch_script_set.contains(&utxo.script_pubkey) {
            &mut watch
        } else {
            &mut owned
        };
        let per_script_bucket = balances_by_script
            .entry(utxo.script_pubkey.clone())
            .or_default();

        if utxo.confirmations == 0 {
            bucket.unconfirmed = bucket
                .unconfirmed
                .checked_add(utxo.value)
                .ok_or_else(|| "wallet balance overflow".to_string())?;
            per_script_bucket.unconfirmed = per_script_bucket
                .unconfirmed
                .checked_add(utxo.value)
                .ok_or_else(|| "wallet balance overflow".to_string())?;
            continue;
        }
        if utxo.is_coinbase && utxo.confirmations < COINBASE_MATURITY {
            bucket.immature = bucket
                .immature
                .checked_add(utxo.value)
                .ok_or_else(|| "wallet balance overflow".to_string())?;
            per_script_bucket.immature = per_script_bucket
                .immature
                .checked_add(utxo.value)
                .ok_or_else(|| "wallet balance overflow".to_string())?;
            continue;
        }
        bucket.confirmed = bucket
            .confirmed
            .checked_add(utxo.value)
            .ok_or_else(|| "wallet balance overflow".to_string())?;
        per_script_bucket.confirmed = per_script_bucket
            .confirmed
            .checked_add(utxo.value)
            .ok_or_else(|| "wallet balance overflow".to_string())?;
    }

    for row in &mut addresses {
        if !matches!(
            row.kind,
            WalletAddressKind::TransparentReceive
                | WalletAddressKind::TransparentChange
                | WalletAddressKind::TransparentWatch
        ) {
            continue;
        }
        if let Ok(script) = address_to_script_pubkey(&row.address, wallet_network) {
            row.transparent_balance = balances_by_script.get(&script).cloned();
        }
    }

    if !utxos.is_empty() {
        let txids = utxos
            .iter()
            .map(|utxo| utxo.outpoint.hash)
            .collect::<HashSet<_>>();
        if let Ok(mut guard) = wallet.lock() {
            let _ = guard.record_txids(txids);
        }
    }
    let recent_txs = wallet
        .lock()
        .map_err(|_| "wallet lock poisoned".to_string())?
        .recent_transactions(WALLET_RECENT_TXS)
        .into_iter()
        .map(|(txid, received_at)| WalletTxRow { txid, received_at })
        .collect::<Vec<_>>();

    let best_height = tip_height
        .or_else(|| chainstate.best_block().ok().flatten().map(|tip| tip.height))
        .unwrap_or(0);
    let mut sapling_spendable = 0i64;
    let mut sapling_watchonly = 0i64;
    let mut chain_spend_status: Vec<(SaplingNoteSummary, bool)> = Vec::new();
    chain_spend_status.reserve(sapling_notes.len());
    for note in sapling_notes {
        let confirmations = best_height.saturating_sub(note.height).saturating_add(1);
        if confirmations < 1 {
            continue;
        }
        let spent = chainstate
            .sapling_nullifier_spent(&note.nullifier)
            .map_err(|err| err.to_string())?;
        chain_spend_status.push((note, spent));
    }
    let mempool_guard = mempool
        .lock()
        .map_err(|_| "mempool lock poisoned".to_string())?;
    for (note, spent_in_chain) in chain_spend_status {
        if spent_in_chain {
            continue;
        }
        if mempool_guard
            .sapling_nullifier_spender(&note.nullifier)
            .is_some()
        {
            continue;
        }
        match note.ownership {
            SaplingNoteOwnership::Spendable => {
                sapling_spendable = sapling_spendable
                    .checked_add(note.value)
                    .ok_or_else(|| "wallet balance overflow".to_string())?;
            }
            SaplingNoteOwnership::WatchOnly => {
                sapling_watchonly = sapling_watchonly
                    .checked_add(note.value)
                    .ok_or_else(|| "wallet balance overflow".to_string())?;
            }
        }
    }

    let pending_ops = crate::rpc::tui_async_ops_snapshot(WALLET_PENDING_OPS);

    Ok(WalletDetails {
        transparent_owned: owned,
        transparent_watchonly: watch,
        sapling_spendable,
        sapling_watchonly,
        sapling_scan_height,
        sapling_note_count,
        addresses,
        recent_txs,
        pending_ops,
    })
}

#[derive(Clone)]
struct WalletUtxoRow {
    outpoint: OutPoint,
    value: i64,
    script_pubkey: Vec<u8>,
    is_coinbase: bool,
    confirmations: i32,
}

fn collect_wallet_utxos(
    chainstate: &ChainState<Store>,
    mempool: &Mutex<Mempool>,
    scripts: &[Vec<u8>],
    include_mempool_outputs: bool,
) -> Result<Vec<WalletUtxoRow>, String> {
    if scripts.is_empty() {
        return Ok(Vec::new());
    }

    let best_height = chainstate
        .best_block()
        .map_err(|err| err.to_string())?
        .map(|tip| tip.height)
        .unwrap_or(0);

    let mut seen: HashSet<OutPoint> = HashSet::new();
    let mut out = Vec::new();
    for script_pubkey in scripts {
        let outpoints = chainstate
            .address_outpoints(script_pubkey)
            .map_err(|err| err.to_string())?;
        for outpoint in outpoints {
            if !seen.insert(outpoint.clone()) {
                continue;
            }
            let entry = chainstate
                .utxo_entry(&outpoint)
                .map_err(|err| err.to_string())?
                .ok_or_else(|| "missing utxo entry".to_string())?;
            let height_i32 = i32::try_from(entry.height).unwrap_or(0);
            let confirmations = if best_height >= height_i32 {
                best_height.saturating_sub(height_i32).saturating_add(1)
            } else {
                0
            };
            out.push(WalletUtxoRow {
                outpoint,
                value: entry.value,
                script_pubkey: entry.script_pubkey,
                is_coinbase: entry.is_coinbase,
                confirmations,
            });
        }
    }

    let mempool_guard = mempool
        .lock()
        .map_err(|_| "mempool lock poisoned".to_string())?;
    out.retain(|row| !mempool_guard.is_spent(&row.outpoint));

    if include_mempool_outputs {
        for entry in mempool_guard.entries() {
            for (output_index, output) in entry.tx.vout.iter().enumerate() {
                if !scripts
                    .iter()
                    .any(|script| script.as_slice() == output.script_pubkey.as_slice())
                {
                    continue;
                }
                let outpoint = OutPoint {
                    hash: entry.txid,
                    index: output_index as u32,
                };
                if mempool_guard.is_spent(&outpoint) {
                    continue;
                }
                if !seen.insert(outpoint.clone()) {
                    continue;
                }
                out.push(WalletUtxoRow {
                    outpoint,
                    value: output.value,
                    script_pubkey: output.script_pubkey.clone(),
                    is_coinbase: false,
                    confirmations: 0,
                });
            }
        }
    }
    Ok(out)
}

struct InProcessWalletOps<'a> {
    chainstate: &'a ChainState<Store>,
    mempool: &'a Mutex<Mempool>,
    mempool_policy: &'a MempoolPolicy,
    mempool_metrics: &'a MempoolMetrics,
    fee_estimator: &'a Mutex<FeeEstimator>,
    tx_confirm_target: u32,
    mempool_flags: &'a ValidationFlags,
    wallet: &'a Mutex<Wallet>,
    chain_params: &'a ChainParams,
    tx_announce: &'a broadcast::Sender<Hash256>,
}

fn wallet_ops_unavailable(is_remote: bool) -> String {
    if is_remote {
        "Remote attach mode: wallet unavailable.".to_string()
    } else {
        "Wallet is initializing (loading shielded params)...".to_string()
    }
}

fn wallet_send_unavailable(is_remote: bool) -> String {
    if is_remote {
        "Remote attach mode: wallet send unavailable.".to_string()
    } else {
        "Wallet send unavailable until shielded params finish loading.".to_string()
    }
}

fn handle_key(
    key: KeyEvent,
    state: &mut TuiState,
    shutdown_tx: &watch::Sender<bool>,
    wallet_ops: Option<&InProcessWalletOps<'_>>,
) -> Result<bool, String> {
    if state.command_mode {
        let suggestions = command_suggestions(&state.command_input);
        match (key.code, key.modifiers) {
            (KeyCode::Char('c'), KeyModifiers::CONTROL) => {
                return ctrl_c_exit(state, shutdown_tx);
            }
            (KeyCode::Esc, _) => {
                state.command_mode = false;
                state.command_input.clear();
                state.command_selected = 0;
                return Ok(false);
            }
            (KeyCode::Enter, _) => {
                let selected_index = state
                    .command_selected
                    .min(suggestions.len().saturating_sub(1));
                let mut line = state.command_input.trim().to_string();
                if line.is_empty() || line == "/" {
                    if let Some(spec) = suggestions.get(selected_index) {
                        line = spec.command.to_string();
                    }
                }
                let mut parts = line.split_whitespace();
                let raw_cmd = parts.next().unwrap_or("");
                let arg = parts.next().map(|value| value.to_lowercase());
                state.command_mode = false;
                state.command_input.clear();
                state.command_selected = 0;
                state.command_status = None;

                let spec = if raw_cmd.is_empty() {
                    suggestions.get(selected_index).copied()
                } else {
                    find_command_spec(raw_cmd)
                };

                if let Some(spec) = spec {
                    match spec.action {
                        CommandAction::Navigate(screen) => state.screen = screen,
                        CommandAction::ToggleHelp => state.toggle_help(),
                        CommandAction::ToggleSetup => state.toggle_setup(),
                        CommandAction::ToggleAdvanced => state.toggle_advanced(),
                        CommandAction::ToggleMouseCapture => {
                            state.mouse_capture = !state.mouse_capture;
                            set_mouse_capture(state.mouse_capture)?;
                            state.command_status = Some(if state.mouse_capture {
                                "Mouse capture on".to_string()
                            } else {
                                "Mouse capture off (selection enabled)".to_string()
                            });
                        }
                        CommandAction::Quit => {
                            let _ = shutdown_tx.send(true);
                            return Ok(true);
                        }
                        CommandAction::LogLevel(level) => match level {
                            Some(level) => {
                                state.logs_min_level = level;
                                state.command_status =
                                    Some(format!("Log level: {}", log_level_label(level)));
                            }
                            None => match arg.as_deref() {
                                Some(value) => {
                                    if let Some(level) = parse_log_level(value) {
                                        state.logs_min_level = level;
                                        state.command_status =
                                            Some(format!("Log level: {}", log_level_label(level)));
                                    } else {
                                        state.command_status =
                                            Some(format!("Unknown log level: {value}"));
                                    }
                                }
                                None => {
                                    state.command_status = Some("Usage: /log <level>".to_string());
                                }
                            },
                        },
                        CommandAction::LogsClear => {
                            logging::clear_captured_logs();
                            state.logs.clear();
                            state.logs_scroll = 0;
                            state.logs_follow = true;
                            state.logs_paused = false;
                            state.command_status = Some("Logs cleared".to_string());
                        }
                        CommandAction::LogsPause => {
                            state.toggle_logs_pause();
                            state.command_status = Some(if state.logs_paused {
                                "Logs paused".to_string()
                            } else {
                                "Logs following".to_string()
                            });
                        }
                        CommandAction::LogsFollow => {
                            state.logs_follow = true;
                            state.logs_paused = false;
                            state.logs_scroll = 0;
                            state.command_status = Some("Logs following".to_string());
                        }
                        CommandAction::WalletSend => {
                            state.screen = Screen::Wallet;
                            if state.is_remote {
                                state.wallet_status = Some(
                                    "Remote attach mode: wallet send unavailable.".to_string(),
                                );
                            } else {
                                state.wallet_modal = Some(WalletModal::Send);
                                state.wallet_send_form = WalletSendForm::default();
                            }
                        }
                        CommandAction::WalletWatch => {
                            state.screen = Screen::Wallet;
                            if state.is_remote {
                                state.wallet_status =
                                    Some("Remote attach mode: wallet unavailable.".to_string());
                            } else {
                                state.wallet_modal = Some(WalletModal::ImportWatch);
                                state.wallet_import_watch_form = WalletImportWatchForm::default();
                            }
                        }
                        CommandAction::WalletNewTransparent => {
                            state.screen = Screen::Wallet;
                            if state.is_remote {
                                state.wallet_status =
                                    Some("Remote attach mode: wallet unavailable.".to_string());
                            } else if let Some(ops) = wallet_ops {
                                let address_res = {
                                    let guard = ops
                                        .wallet
                                        .lock()
                                        .map_err(|_| "wallet lock poisoned".to_string());
                                    match guard {
                                        Ok(mut g) => {
                                            g.generate_new_address(true).map_err(|e| e.to_string())
                                        }
                                        Err(e) => Err(e),
                                    }
                                };
                                match address_res {
                                    Ok(address) => {
                                        state.wallet_status = Some(format!("Generated {address}"));
                                        state.wallet_select_after_refresh = Some(address);
                                        state.wallet_force_refresh = true;
                                    }
                                    Err(e) => state.command_status = Some(format!("Error: {e}")),
                                }
                            } else {
                                state.wallet_status = Some(wallet_ops_unavailable(state.is_remote));
                            }
                        }
                        CommandAction::WalletNewSapling => {
                            state.screen = Screen::Wallet;
                            if state.is_remote {
                                state.wallet_status =
                                    Some("Remote attach mode: wallet unavailable.".to_string());
                            } else if let Some(ops) = wallet_ops {
                                let res = {
                                    let guard = ops
                                        .wallet
                                        .lock()
                                        .map_err(|_| "wallet lock poisoned".to_string());
                                    match guard {
                                        Ok(mut g) => {
                                            let had_keys = g.has_sapling_keys();
                                            match g.generate_new_sapling_address_bytes() {
                                                Ok(bytes) => {
                                                    if !had_keys {
                                                        if let Err(e) = g
                                                            .ensure_sapling_scan_initialized_to_tip(
                                                                ops.chainstate,
                                                            )
                                                        {
                                                            Err(e.to_string())
                                                        } else {
                                                            Ok(bytes)
                                                        }
                                                    } else {
                                                        Ok(bytes)
                                                    }
                                                }
                                                Err(e) => Err(e.to_string()),
                                            }
                                        }
                                        Err(e) => Err(e),
                                    }
                                };
                                match res {
                                    Ok(bytes) => {
                                        let hrp = match ops.chain_params.network {
                                            Network::Mainnet => "za",
                                            Network::Testnet => "ztestacadia",
                                            Network::Regtest => "zregtestsapling",
                                        };
                                        match Hrp::parse(hrp) {
                                            Ok(hrp) => match bech32::encode::<Bech32>(
                                                hrp,
                                                bytes.as_slice(),
                                            ) {
                                                Ok(address) => {
                                                    state.wallet_status =
                                                        Some(format!("Generated {address}"));
                                                    state.wallet_select_after_refresh =
                                                        Some(address);
                                                    state.wallet_force_refresh = true;
                                                }
                                                Err(_) => {
                                                    state.command_status = Some(
                                                        "failed to encode sapling address"
                                                            .to_string(),
                                                    )
                                                }
                                            },
                                            Err(_) => {
                                                state.command_status =
                                                    Some("invalid sapling address hrp".to_string())
                                            }
                                        }
                                    }
                                    Err(e) => state.command_status = Some(format!("Error: {e}")),
                                }
                            } else {
                                state.wallet_status = Some(wallet_ops_unavailable(state.is_remote));
                            }
                        }
                        CommandAction::WalletToggleQr => {
                            state.screen = Screen::Wallet;
                            if !state.wallet_show_qr {
                                state.wallet_show_qr = true;
                                state.wallet_qr_expanded = true;
                            } else {
                                state.wallet_qr_expanded = !state.wallet_qr_expanded;
                            }
                        }
                        CommandAction::Hint(message) => {
                            state.command_status = Some(message.to_string());
                        }
                    }
                } else if !raw_cmd.is_empty() {
                    state.command_status = Some(format!("Unknown command: {raw_cmd}"));
                }
                return Ok(false);
            }
            (KeyCode::Backspace, _) => {
                state.command_input.pop();
                state.command_selected = 0;
                return Ok(false);
            }
            (KeyCode::Up, _) => {
                if state.command_selected > 0 {
                    state.command_selected -= 1;
                }
                return Ok(false);
            }
            (KeyCode::Down, _) => {
                if state.command_selected + 1 < suggestions.len() {
                    state.command_selected += 1;
                }
                return Ok(false);
            }
            (KeyCode::Tab, _) => {
                if !suggestions.is_empty() {
                    state.command_selected = (state.command_selected + 1) % suggestions.len();
                }
                return Ok(false);
            }
            (KeyCode::BackTab, _) => {
                if !suggestions.is_empty() {
                    if state.command_selected == 0 {
                        state.command_selected = suggestions.len() - 1;
                    } else {
                        state.command_selected -= 1;
                    }
                }
                return Ok(false);
            }
            (KeyCode::Char(c), _) => {
                if !c.is_control() && state.command_input.len() < 128 {
                    state.command_input.push(c);
                    state.command_selected = 0;
                }
                return Ok(false);
            }
            _ => return Ok(false),
        }
    }

    if matches!(state.screen, Screen::Setup) {
        match (key.code, key.modifiers) {
            (KeyCode::Esc, _) => {
                state.toggle_setup();
                return Ok(false);
            }
            (KeyCode::Char('s'), _) => {
                state.toggle_setup();
                return Ok(false);
            }
            (KeyCode::Up, _) => {
                if let Some(setup) = state.setup.as_mut() {
                    setup.move_data_set(-1);
                }
                return Ok(false);
            }
            (KeyCode::Down, _) => {
                if let Some(setup) = state.setup.as_mut() {
                    setup.move_data_set(1);
                }
                return Ok(false);
            }
            (KeyCode::Enter, _) => {
                if let Some(setup) = state.setup.as_mut() {
                    if let Err(err) = setup.apply_selected_data_set() {
                        setup.status = Some(format!("Error: {err}"));
                    }
                }
                return Ok(false);
            }
            (KeyCode::Char('r'), _) => {
                if let Some(setup) = state.setup.as_mut() {
                    setup.refresh_data_sets();
                    setup.status = Some("Data sets refreshed".to_string());
                }
                return Ok(false);
            }
            (KeyCode::Char('d'), _) => {
                if let Some(setup) = state.setup.as_mut() {
                    if let Err(err) = setup.request_delete_selected() {
                        setup.status = Some(format!("Delete failed: {err}"));
                    }
                }
                return Ok(false);
            }
            (KeyCode::Char('b'), _) => {
                let is_remote = state.is_remote;
                if let Some(setup) = state.setup.as_mut() {
                    let target_dir = setup.data_dir.clone();
                    let backup_path = target_dir.join(format!(
                        "{}.bak.{}",
                        crate::wallet::WALLET_FILE_NAME,
                        unix_seconds()
                    ));
                    if target_dir == setup.active_data_dir {
                        let Some(ops) = wallet_ops else {
                            setup.status = Some(if is_remote {
                                "Remote attach mode: wallet unavailable.".to_string()
                            } else {
                                "Wallet is initializing (loading shielded params)...".to_string()
                            });
                            return Ok(false);
                        };

                        match ops.wallet.lock() {
                            Ok(mut guard) => match guard.backup_to(&backup_path) {
                                Ok(()) => {
                                    setup.status = Some(format!(
                                        "Wallet backup saved to {}",
                                        backup_path.display()
                                    ));
                                }
                                Err(err) => {
                                    setup.status = Some(format!("Backup failed: {err}"));
                                }
                            },
                            Err(_) => {
                                setup.status = Some("Wallet lock poisoned".to_string());
                            }
                        }
                    } else {
                        let source = target_dir.join(crate::wallet::WALLET_FILE_NAME);
                        if !source.exists() {
                            setup.status =
                                Some("wallet.dat not found in selected data dir".to_string());
                            return Ok(false);
                        }
                        match fs::copy(&source, &backup_path) {
                            Ok(_) => {
                                setup.status = Some(format!(
                                    "Wallet backup saved to {}",
                                    backup_path.display()
                                ));
                            }
                            Err(err) => {
                                setup.status = Some(format!("Backup failed: {err}"));
                            }
                        }
                    }
                }
                return Ok(false);
            }
            (KeyCode::Char('N'), _) => {
                if state.is_remote {
                    if let Some(setup) = state.setup.as_mut() {
                        setup.status =
                            Some("Remote attach mode: cannot create data dirs".to_string());
                    }
                    return Ok(false);
                }
                if let Some(setup) = state.setup.as_mut() {
                    if let Err(err) = setup.create_new_data_set() {
                        setup.status = Some(format!("Create failed: {err}"));
                    }
                }
                return Ok(false);
            }
            (KeyCode::Char('n'), _) => {
                if let Some(setup) = state.setup.as_mut() {
                    setup.cycle_network();
                }
                return Ok(false);
            }
            (KeyCode::Char('p'), _) => {
                if let Some(setup) = state.setup.as_mut() {
                    setup.cycle_profile();
                }
                return Ok(false);
            }
            (KeyCode::Char('l'), _) => {
                if let Some(setup) = state.setup.as_mut() {
                    setup.toggle_header_lead_unlimited();
                }
                return Ok(false);
            }
            (KeyCode::Char('['), _) => {
                if let Some(setup) = state.setup.as_mut() {
                    setup.adjust_header_lead(-HEADER_LEAD_STEP);
                }
                return Ok(false);
            }
            (KeyCode::Char(']'), _) => {
                if let Some(setup) = state.setup.as_mut() {
                    setup.adjust_header_lead(HEADER_LEAD_STEP);
                }
                return Ok(false);
            }
            (KeyCode::Char('g'), _) => {
                if let Some(setup) = state.setup.as_mut() {
                    setup.regenerate_auth();
                }
                return Ok(false);
            }
            (KeyCode::Char('v'), _) => {
                if let Some(setup) = state.setup.as_mut() {
                    setup.toggle_pass_visible();
                }
                return Ok(false);
            }
            (KeyCode::Char('w'), _) => {
                if let Some(setup) = state.setup.as_mut() {
                    if let Err(err) = setup.write_config() {
                        setup.status = Some(format!("Error: {err}"));
                    }
                }
                return Ok(false);
            }
            _ => {}
        }
    }

    if let Some(modal) = state.wallet_modal {
        match modal {
            WalletModal::Send => match (key.code, key.modifiers) {
                (KeyCode::Esc, _) => {
                    state.wallet_modal = None;
                    return Ok(false);
                }
                (KeyCode::Char('q'), _) => {
                    let _ = shutdown_tx.send(true);
                    return Ok(true);
                }
                (KeyCode::Char('c'), KeyModifiers::CONTROL) => {
                    return ctrl_c_exit(state, shutdown_tx);
                }
                (KeyCode::Tab, _) | (KeyCode::BackTab, _) => {
                    state.wallet_send_form.focus = match state.wallet_send_form.focus {
                        WalletSendField::To => WalletSendField::Amount,
                        WalletSendField::Amount => WalletSendField::To,
                    };
                    return Ok(false);
                }
                (KeyCode::Char('f'), _) => {
                    state.wallet_send_form.subtract_fee = !state.wallet_send_form.subtract_fee;
                    return Ok(false);
                }
                (KeyCode::Backspace, _) => {
                    let field = match state.wallet_send_form.focus {
                        WalletSendField::To => &mut state.wallet_send_form.to,
                        WalletSendField::Amount => &mut state.wallet_send_form.amount,
                    };
                    field.pop();
                    return Ok(false);
                }
                (KeyCode::Enter, _) => {
                    let Some(ops) = wallet_ops else {
                        state.wallet_status = Some(wallet_send_unavailable(state.is_remote));
                        return Ok(false);
                    };
                    let to = state.wallet_send_form.to.trim().to_string();
                    let amount = state.wallet_send_form.amount.trim().to_string();
                    if to.is_empty() || amount.is_empty() {
                        state.wallet_status = Some("Missing address or amount.".to_string());
                        return Ok(false);
                    }

                    let mut params = vec![
                        serde_json::Value::String(to),
                        serde_json::Value::String(amount),
                    ];
                    if state.wallet_send_form.subtract_fee {
                        params.push(serde_json::Value::Null);
                        params.push(serde_json::Value::Null);
                        params.push(serde_json::Value::Bool(true));
                    }

                    match crate::rpc::rpc_sendtoaddress(
                        ops.chainstate,
                        ops.mempool,
                        ops.mempool_policy,
                        ops.mempool_metrics,
                        ops.fee_estimator,
                        ops.tx_confirm_target,
                        ops.mempool_flags,
                        ops.wallet,
                        params,
                        ops.chain_params,
                        ops.tx_announce,
                    ) {
                        Ok(value) => {
                            let txid = value
                                .as_str()
                                .map(|value| value.to_string())
                                .unwrap_or_else(|| value.to_string());
                            state.wallet_status = Some(format!("Sent {txid}"));
                            state.wallet_modal = None;
                            state.wallet_send_form = WalletSendForm::default();
                            state.wallet_force_refresh = true;
                        }
                        Err(err) => {
                            state.wallet_status = Some(format!("Send failed: {err}"));
                        }
                    }
                    return Ok(false);
                }
                (KeyCode::Char(ch), _) => {
                    let field = match state.wallet_send_form.focus {
                        WalletSendField::To => &mut state.wallet_send_form.to,
                        WalletSendField::Amount => &mut state.wallet_send_form.amount,
                    };
                    if field.len() >= 128 {
                        return Ok(false);
                    }
                    match state.wallet_send_form.focus {
                        WalletSendField::To => {
                            if ch.is_ascii_alphanumeric() {
                                field.push(ch);
                            }
                        }
                        WalletSendField::Amount => {
                            if ch.is_ascii_digit() {
                                field.push(ch);
                            } else if ch == '.' && !field.contains('.') {
                                field.push(ch);
                            }
                        }
                    }
                    return Ok(false);
                }
                _ => return Ok(false),
            },
            WalletModal::ImportWatch => match (key.code, key.modifiers) {
                (KeyCode::Esc, _) => {
                    state.wallet_modal = None;
                    return Ok(false);
                }
                (KeyCode::Char('q'), _) => {
                    let _ = shutdown_tx.send(true);
                    return Ok(true);
                }
                (KeyCode::Char('c'), KeyModifiers::CONTROL) => {
                    return ctrl_c_exit(state, shutdown_tx);
                }
                (KeyCode::Tab, _) | (KeyCode::BackTab, _) => {
                    state.wallet_import_watch_form.focus =
                        match state.wallet_import_watch_form.focus {
                            WalletImportWatchField::Address => WalletImportWatchField::Label,
                            WalletImportWatchField::Label => WalletImportWatchField::Address,
                        };
                    return Ok(false);
                }
                (KeyCode::Backspace, _) => {
                    let field = match state.wallet_import_watch_form.focus {
                        WalletImportWatchField::Address => {
                            &mut state.wallet_import_watch_form.address
                        }
                        WalletImportWatchField::Label => &mut state.wallet_import_watch_form.label,
                    };
                    field.pop();
                    return Ok(false);
                }
                (KeyCode::Enter, _) => {
                    let Some(ops) = wallet_ops else {
                        state.wallet_status = Some(wallet_ops_unavailable(state.is_remote));
                        return Ok(false);
                    };
                    let address = state.wallet_import_watch_form.address.trim().to_string();
                    let label = state.wallet_import_watch_form.label.trim().to_string();
                    if address.is_empty() {
                        state.wallet_status = Some("Missing address.".to_string());
                        return Ok(false);
                    }

                    let script_pubkey =
                        match address_to_script_pubkey(&address, ops.chain_params.network) {
                            Ok(script) => script,
                            Err(_) => {
                                state.wallet_status = Some("Invalid address.".to_string());
                                return Ok(false);
                            }
                        };

                    match ops.wallet.lock() {
                        Err(_) => {
                            state.wallet_status = Some("wallet lock poisoned".to_string());
                            return Ok(false);
                        }
                        Ok(mut guard) => {
                            if let Err(err) =
                                guard.import_watch_script_pubkey(script_pubkey.clone())
                            {
                                state.wallet_status = Some(format!("Import failed: {err}"));
                                return Ok(false);
                            }
                            if !label.is_empty() {
                                if let Err(err) =
                                    guard.set_label_for_script_pubkey(script_pubkey, label)
                                {
                                    state.wallet_status = Some(format!("Label failed: {err}"));
                                    return Ok(false);
                                }
                            }
                        }
                    }

                    state.wallet_status = Some(format!("Watching {address}"));
                    state.wallet_modal = None;
                    state.wallet_import_watch_form = WalletImportWatchForm::default();
                    state.wallet_select_after_refresh = Some(address.to_string());
                    state.wallet_force_refresh = true;
                    return Ok(false);
                }
                (KeyCode::Char(ch), _) => {
                    let field = match state.wallet_import_watch_form.focus {
                        WalletImportWatchField::Address => {
                            &mut state.wallet_import_watch_form.address
                        }
                        WalletImportWatchField::Label => &mut state.wallet_import_watch_form.label,
                    };
                    if field.len() >= 128 {
                        return Ok(false);
                    }

                    match state.wallet_import_watch_form.focus {
                        WalletImportWatchField::Address => {
                            if ch.is_ascii_alphanumeric() {
                                field.push(ch);
                            }
                        }
                        WalletImportWatchField::Label => {
                            if !ch.is_control() {
                                field.push(ch);
                            }
                        }
                    }
                    return Ok(false);
                }
                _ => return Ok(false),
            },
        }
    }

    if state.wallet_qr_expanded && matches!(key.code, KeyCode::Esc) {
        state.wallet_qr_expanded = false;
        return Ok(false);
    }

    match (key.code, key.modifiers) {
        (KeyCode::Char('q'), _) | (KeyCode::Esc, _) => {
            let _ = shutdown_tx.send(true);
            Ok(true)
        }
        (KeyCode::Char('/'), _) => {
            state.command_mode = true;
            state.command_input.clear();
            state.command_input.push('/');
            state.command_selected = 0;
            state.command_status = None;
            Ok(false)
        }
        (KeyCode::Char('c'), KeyModifiers::CONTROL) => ctrl_c_exit(state, shutdown_tx),
        (KeyCode::Char('?'), _) => {
            state.toggle_help();
            Ok(false)
        }
        (KeyCode::Char('s'), _) => {
            state.toggle_setup();
            Ok(false)
        }
        (KeyCode::Tab, _) => {
            state.cycle_screen();
            Ok(false)
        }
        (KeyCode::BackTab, _) => {
            state.cycle_screen_reverse();
            Ok(false)
        }
        (KeyCode::Left, _) => {
            state.cycle_screen_reverse();
            Ok(false)
        }
        (KeyCode::Right, _) => {
            state.cycle_screen();
            Ok(false)
        }
        (KeyCode::Char('a'), _) => {
            state.toggle_advanced();
            Ok(false)
        }
        (KeyCode::Char('f'), _) => {
            if matches!(state.screen, Screen::Logs) {
                state.cycle_logs_min_level();
            }
            Ok(false)
        }
        (KeyCode::Char(' '), _) => {
            if matches!(state.screen, Screen::Logs) {
                state.toggle_logs_pause();
            }
            Ok(false)
        }
        (KeyCode::Char('c'), _) => {
            if matches!(state.screen, Screen::Logs) {
                logging::clear_captured_logs();
                state.logs.clear();
                state.logs_scroll = 0;
                state.logs_follow = true;
                state.logs_paused = false;
            }
            Ok(false)
        }
        (KeyCode::Enter, _) => {
            if matches!(state.screen, Screen::Wallet) {
                if !state.wallet_show_qr {
                    state.wallet_show_qr = true;
                    state.wallet_qr_expanded = true;
                } else {
                    state.wallet_qr_expanded = !state.wallet_qr_expanded;
                }
            }
            Ok(false)
        }
        (KeyCode::Char('o'), _) => {
            if matches!(state.screen, Screen::Wallet) {
                if let Some(selected) = state.wallet_addresses.get(state.wallet_selected_address) {
                    let target = explorer_address_url(&selected.address);
                    let status = open_or_copy_url(&target)
                        .map(str::to_string)
                        .unwrap_or_else(|| "Explorer open failed".to_string());
                    state.command_status = Some(status.clone());
                    state.wallet_status = Some(status);
                }
            }
            Ok(false)
        }
        (KeyCode::Char('x'), _) => {
            if matches!(state.screen, Screen::Wallet) {
                if state.is_remote {
                    state.wallet_status =
                        Some("Remote attach mode: wallet send unavailable.".to_string());
                } else {
                    state.wallet_modal = Some(WalletModal::Send);
                    state.wallet_send_form = WalletSendForm::default();
                }
            }
            Ok(false)
        }
        (KeyCode::Char('i'), _) => {
            if matches!(state.screen, Screen::Wallet) {
                if state.is_remote {
                    state.wallet_status =
                        Some("Remote attach mode: wallet unavailable.".to_string());
                } else {
                    state.wallet_modal = Some(WalletModal::ImportWatch);
                    state.wallet_import_watch_form = WalletImportWatchForm::default();
                }
            }
            Ok(false)
        }
        (KeyCode::Char('n'), _) => {
            if matches!(state.screen, Screen::Wallet) {
                let Some(ops) = wallet_ops else {
                    state.wallet_status = Some(wallet_ops_unavailable(state.is_remote));
                    return Ok(false);
                };
                let address = {
                    let mut guard = ops
                        .wallet
                        .lock()
                        .map_err(|_| "wallet lock poisoned".to_string())?;
                    guard
                        .generate_new_address(true)
                        .map_err(|err| err.to_string())?
                };
                state.wallet_status = Some(format!("Generated {address}"));
                state.wallet_select_after_refresh = Some(address);
                state.wallet_force_refresh = true;
            }
            Ok(false)
        }
        (KeyCode::Char('N'), _) => {
            if matches!(state.screen, Screen::Wallet) {
                let Some(ops) = wallet_ops else {
                    state.wallet_status = Some(wallet_ops_unavailable(state.is_remote));
                    return Ok(false);
                };
                let address = {
                    let mut guard = ops
                        .wallet
                        .lock()
                        .map_err(|_| "wallet lock poisoned".to_string())?;
                    let had_keys = guard.has_sapling_keys();
                    let bytes = guard
                        .generate_new_sapling_address_bytes()
                        .map_err(|err| err.to_string())?;
                    if !had_keys {
                        guard
                            .ensure_sapling_scan_initialized_to_tip(ops.chainstate)
                            .map_err(|err| err.to_string())?;
                    }
                    let hrp = match ops.chain_params.network {
                        Network::Mainnet => "za",
                        Network::Testnet => "ztestacadia",
                        Network::Regtest => "zregtestsapling",
                    };
                    let hrp =
                        Hrp::parse(hrp).map_err(|_| "invalid sapling address hrp".to_string())?;
                    bech32::encode::<Bech32>(hrp, bytes.as_slice())
                        .map_err(|_| "failed to encode sapling address".to_string())?
                };
                state.wallet_status = Some(format!("Generated {address}"));
                state.wallet_select_after_refresh = Some(address);
                state.wallet_force_refresh = true;
            }
            Ok(false)
        }
        (KeyCode::Up, _) => {
            if matches!(state.screen, Screen::Logs) {
                state.logs_follow = false;
                state.logs_scroll = state.logs_scroll.saturating_sub(1);
            } else if matches!(state.screen, Screen::Peers) {
                state.peers_scroll = state.peers_scroll.saturating_sub(PEER_SCROLL_STEP);
            } else if matches!(state.screen, Screen::Wallet) {
                state.wallet_move_selection(-1);
            }
            Ok(false)
        }
        (KeyCode::Down, _) => {
            if matches!(state.screen, Screen::Logs) {
                state.logs_follow = false;
                state.logs_scroll = state.logs_scroll.saturating_add(1);
            } else if matches!(state.screen, Screen::Peers) {
                state.peers_scroll = state.peers_scroll.saturating_add(PEER_SCROLL_STEP);
            } else if matches!(state.screen, Screen::Wallet) {
                state.wallet_move_selection(1);
            }
            Ok(false)
        }
        (KeyCode::PageUp, _) => {
            if matches!(state.screen, Screen::Logs) {
                state.logs_follow = false;
                state.logs_scroll = state.logs_scroll.saturating_sub(10);
            } else if matches!(state.screen, Screen::Peers) {
                state.peers_scroll = state.peers_scroll.saturating_sub(PEER_PAGE_STEP);
            } else if matches!(state.screen, Screen::Wallet) {
                state.wallet_move_selection(-5);
            }
            Ok(false)
        }
        (KeyCode::PageDown, _) => {
            if matches!(state.screen, Screen::Logs) {
                state.logs_follow = false;
                state.logs_scroll = state.logs_scroll.saturating_add(10);
            } else if matches!(state.screen, Screen::Peers) {
                state.peers_scroll = state.peers_scroll.saturating_add(PEER_PAGE_STEP);
            } else if matches!(state.screen, Screen::Wallet) {
                state.wallet_move_selection(5);
            }
            Ok(false)
        }
        (KeyCode::Home, _) => {
            if matches!(state.screen, Screen::Logs) {
                state.logs_follow = false;
                state.logs_paused = true;
                state.logs_scroll = 0;
            } else if matches!(state.screen, Screen::Peers) {
                state.peers_scroll = 0;
            } else if matches!(state.screen, Screen::Wallet) {
                state.wallet_selected_address =
                    state.wallet_visible_indices().first().copied().unwrap_or(0);
            }
            Ok(false)
        }
        (KeyCode::End, _) => {
            if matches!(state.screen, Screen::Logs) {
                state.logs_paused = false;
                state.logs_follow = true;
                state.logs_scroll = 0;
            } else if matches!(state.screen, Screen::Peers) {
                state.peers_scroll = u16::MAX;
            } else if matches!(state.screen, Screen::Wallet) {
                state.wallet_selected_address =
                    state.wallet_visible_indices().last().copied().unwrap_or(0);
            }
            Ok(false)
        }
        (KeyCode::Char('m'), _) => {
            state.screen = Screen::Monitor;
            Ok(false)
        }
        (KeyCode::Char('p'), _) => {
            state.screen = Screen::Peers;
            Ok(false)
        }
        (KeyCode::Char('d'), _) => {
            state.screen = Screen::Db;
            Ok(false)
        }
        (KeyCode::Char('t'), _) => {
            state.screen = Screen::Mempool;
            Ok(false)
        }
        (KeyCode::Char('w'), _) => {
            state.screen = Screen::Wallet;
            Ok(false)
        }
        (KeyCode::Char('l'), _) => {
            state.screen = Screen::Logs;
            Ok(false)
        }
        (KeyCode::Char('1'), _) => {
            state.screen = Screen::Monitor;
            Ok(false)
        }
        (KeyCode::Char('2'), _) => {
            state.screen = Screen::Peers;
            Ok(false)
        }
        (KeyCode::Char('3'), _) => {
            state.screen = Screen::Db;
            Ok(false)
        }
        (KeyCode::Char('4'), _) => {
            state.screen = Screen::Mempool;
            Ok(false)
        }
        (KeyCode::Char('5'), _) => {
            state.screen = Screen::Wallet;
            Ok(false)
        }
        (KeyCode::Char('6'), _) => {
            state.screen = Screen::Logs;
            Ok(false)
        }
        (KeyCode::Char('7'), _) => {
            state.screen = Screen::Stats;
            Ok(false)
        }
        (KeyCode::Char('h'), _) => {
            state.toggle_help();
            Ok(false)
        }
        _ => Ok(false),
    }
}

fn handle_mouse(event: MouseEvent, state: &mut TuiState, area: Rect) -> Result<bool, String> {
    if !state.mouse_capture {
        return Ok(false);
    }

    let (header_area, main_area, sidebar_area, cmd_area) = layout_areas(state, area);

    match event.kind {
        MouseEventKind::ScrollUp | MouseEventKind::ScrollDown => {
            let scroll_up = matches!(event.kind, MouseEventKind::ScrollUp);

            if state.command_mode && rect_contains(cmd_area, event.column, event.row) {
                if let Some((list_area, _)) = command_palette_areas(cmd_area) {
                    if rect_contains(list_area, event.column, event.row) {
                        let suggestions = command_suggestions(&state.command_input);
                        if suggestions.is_empty() {
                            return Ok(false);
                        }
                        let selected_index = state
                            .command_selected
                            .min(suggestions.len().saturating_sub(1));
                        let next = if scroll_up {
                            selected_index.saturating_sub(1)
                        } else {
                            (selected_index + 1).min(suggestions.len().saturating_sub(1))
                        };
                        state.command_selected = next;
                        return Ok(false);
                    }
                }
            }

            match state.screen {
                Screen::Logs => {
                    if rect_contains(main_area, event.column, event.row) {
                        state.logs_follow = false;
                        if scroll_up {
                            state.logs_scroll = state.logs_scroll.saturating_sub(MOUSE_WHEEL_STEP);
                        } else {
                            state.logs_scroll = state.logs_scroll.saturating_add(MOUSE_WHEEL_STEP);
                        }
                    }
                }
                Screen::Peers => {
                    if rect_contains(main_area, event.column, event.row) {
                        if scroll_up {
                            state.peers_scroll =
                                state.peers_scroll.saturating_sub(PEER_SCROLL_STEP);
                        } else {
                            state.peers_scroll =
                                state.peers_scroll.saturating_add(PEER_SCROLL_STEP);
                        }
                    }
                }
                Screen::Wallet => {
                    if rect_contains(main_area, event.column, event.row) {
                        let chunks = Layout::default()
                            .direction(Direction::Vertical)
                            .constraints([Constraint::Length(13), Constraint::Min(10)])
                            .split(main_area);

                        let lower_chunks = Layout::default()
                            .direction(Direction::Horizontal)
                            .constraints([Constraint::Percentage(60), Constraint::Percentage(40)])
                            .split(chunks[1]);

                        let right_chunks = Layout::default()
                            .direction(Direction::Vertical)
                            .constraints([Constraint::Percentage(55), Constraint::Percentage(45)])
                            .split(lower_chunks[1]);

                        let addr_area = right_chunks[0];
                        if rect_contains(addr_area, event.column, event.row) {
                            if scroll_up {
                                state.wallet_move_selection(-1);
                            } else {
                                state.wallet_move_selection(1);
                            }
                        }
                    }
                }
                Screen::Setup => {
                    if rect_contains(main_area, event.column, event.row) {
                        let sections = Layout::default()
                            .direction(Direction::Vertical)
                            .constraints([Constraint::Min(0), Constraint::Length(6)])
                            .split(main_area);
                        let columns = Layout::default()
                            .direction(Direction::Horizontal)
                            .constraints([Constraint::Percentage(60), Constraint::Percentage(40)])
                            .split(sections[0]);

                        if rect_contains(columns[0], event.column, event.row) {
                            if let Some(setup) = state.setup.as_mut() {
                                setup.move_data_set(if scroll_up { -1 } else { 1 });
                            }
                        }
                    }
                }
                _ => {}
            }

            return Ok(false);
        }
        MouseEventKind::Down(MouseButton::Left) | MouseEventKind::Down(MouseButton::Right) => {}
        _ => return Ok(false),
    }

    let ctrl = event.modifiers.contains(KeyModifiers::CONTROL);
    let (allow_navigation, link_click) = match event.kind {
        MouseEventKind::Down(MouseButton::Left) => (true, ctrl),
        MouseEventKind::Down(MouseButton::Right) => (false, true),
        _ => return Ok(false),
    };

    if state.wallet_qr_expanded {
        let modal_area = wallet_qr_modal_area(area);
        if let Some(selected) = state.wallet_addresses.get(state.wallet_selected_address) {
            let target = if link_click {
                explorer_address_url(&selected.address)
            } else {
                selected.address.clone()
            };
            if rect_contains(modal_area, event.column, event.row) {
                let status = if link_click {
                    open_or_copy_url(&target).map(str::to_string)
                } else if clipboard_copy(&target).is_ok() {
                    Some("Address copied".to_string())
                } else {
                    None
                };
                if let Some(status) = status {
                    state.command_status = Some(status);
                }
                return Ok(false);
            }
        }
        state.wallet_qr_expanded = false;
        return Ok(false);
    }

    if allow_navigation {
        if let Some(screen) = header_tab_at(state, header_area, event.column, event.row) {
            state.screen = screen;
            return Ok(false);
        }

        if rect_contains(sidebar_area, event.column, event.row) {
            let chunks = Layout::default()
                .direction(Direction::Vertical)
                .constraints([
                    Constraint::Length(11),
                    Constraint::Length(3),
                    Constraint::Length(3),
                    Constraint::Min(0),
                ])
                .split(sidebar_area);

            if rect_contains(chunks[0], event.column, event.row) {
                state.screen = Screen::Monitor;
                return Ok(false);
            }
            if rect_contains(chunks[2], event.column, event.row) {
                state.screen = Screen::Mempool;
                return Ok(false);
            }
            if rect_contains(chunks[3], event.column, event.row) {
                state.toggle_help();
                return Ok(false);
            }
        }
    }

    if matches!(state.screen, Screen::Wallet) && rect_contains(main_area, event.column, event.row) {
        let chunks = Layout::default()
            .direction(Direction::Vertical)
            .constraints([Constraint::Length(13), Constraint::Min(10)])
            .split(main_area);

        let lower_chunks = Layout::default()
            .direction(Direction::Horizontal)
            .constraints([Constraint::Percentage(60), Constraint::Percentage(40)])
            .split(chunks[1]);

        let left_chunks = Layout::default()
            .direction(Direction::Vertical)
            .constraints([Constraint::Percentage(55), Constraint::Percentage(45)])
            .split(lower_chunks[0]);

        let right_chunks = Layout::default()
            .direction(Direction::Vertical)
            .constraints([Constraint::Percentage(55), Constraint::Percentage(45)])
            .split(lower_chunks[1]);

        let tx_area = left_chunks[0];
        if rect_contains(tx_area, event.column, event.row) {
            let tx_inner = panel_block("").inner(tx_area);
            let header_rows = 2u16;
            if tx_inner.height > header_rows && event.row >= tx_inner.y + header_rows {
                let row_index = (event.row - tx_inner.y - header_rows) as usize;
                let max_rows = tx_inner.height.saturating_sub(header_rows) as usize;
                if row_index < max_rows && row_index < state.wallet_recent_txs.len() {
                    let entry = &state.wallet_recent_txs[row_index];
                    let txid = stats::hash256_to_hex(&entry.txid);
                    let target = if link_click {
                        explorer_tx_url(&txid)
                    } else {
                        txid.clone()
                    };
                    let status = if link_click {
                        open_or_copy_url(&target).map(str::to_string)
                    } else if clipboard_copy(&target).is_ok() {
                        Some("Txid copied".to_string())
                    } else {
                        None
                    };
                    if let Some(status) = status {
                        state.command_status = Some(status.clone());
                        state.wallet_status = Some(status);
                    }
                    return Ok(false);
                }
            }
        }

        let addr_area = right_chunks[0];
        if rect_contains(addr_area, event.column, event.row) {
            let addr_inner = panel_block("").inner(addr_area);
            let visible = state.wallet_visible_indices();
            if !visible.is_empty() && addr_inner.width > 0 && addr_inner.height > 0 {
                let layout = Layout::default()
                    .direction(Direction::Horizontal)
                    .constraints([Constraint::Min(0), Constraint::Length(1)])
                    .split(addr_inner);
                let list_area = layout[0];

                if rect_contains(list_area, event.column, event.row) {
                    let selected_pos = visible
                        .iter()
                        .position(|idx| *idx == state.wallet_selected_address)
                        .unwrap_or(0);
                    let view_height = list_area.height.max(1) as usize;
                    let max_start = visible.len().saturating_sub(view_height);
                    let mut start = selected_pos.saturating_sub(view_height / 2);
                    start = start.min(max_start);
                    let end = (start + view_height).min(visible.len());

                    let line_index = event.row.saturating_sub(list_area.y) as usize;
                    let visible_count = end.saturating_sub(start);
                    if line_index < visible_count {
                        let idx = visible[start + line_index];
                        state.wallet_selected_address = idx;
                        let address = state.wallet_addresses[idx].address.clone();
                        let target = if link_click {
                            explorer_address_url(&address)
                        } else {
                            address.clone()
                        };
                        let status = if link_click {
                            open_or_copy_url(&target).map(str::to_string)
                        } else if clipboard_copy(&target).is_ok() {
                            Some("Address copied".to_string())
                        } else {
                            None
                        };
                        if let Some(status) = status {
                            state.command_status = Some(status.clone());
                            state.wallet_status = Some(status);
                        }
                        return Ok(false);
                    }
                }
            }
        }

        let qr_area = right_chunks[1];
        if rect_contains(qr_area, event.column, event.row) {
            if !state.wallet_show_qr {
                state.wallet_show_qr = true;
                state.wallet_qr_expanded = true;
            } else {
                state.wallet_qr_expanded = !state.wallet_qr_expanded;
            }
            return Ok(false);
        }
    }

    if !allow_navigation {
        return Ok(false);
    }

    if rect_contains(cmd_area, event.column, event.row) {
        if !state.command_mode {
            state.command_mode = true;
            state.command_input.clear();
            state.command_input.push('/');
            state.command_selected = 0;
            state.command_status = None;
            return Ok(false);
        }

        if let Some((list_area, input_area)) = command_palette_areas(cmd_area) {
            if rect_contains(list_area, event.column, event.row) {
                let list_layout = Layout::default()
                    .direction(Direction::Horizontal)
                    .constraints([Constraint::Min(0), Constraint::Length(1)])
                    .split(list_area);
                let list_content_area = list_layout[0];

                if !rect_contains(list_content_area, event.column, event.row) {
                    return Ok(false);
                }

                let suggestions = command_suggestions(&state.command_input);
                if suggestions.is_empty() {
                    return Ok(false);
                }

                let max_rows = list_content_area.height.max(1) as usize;
                let selected_index = state
                    .command_selected
                    .min(suggestions.len().saturating_sub(1));

                let max_start = suggestions.len().saturating_sub(max_rows);
                let start = selected_index.saturating_sub(max_rows / 2).min(max_start);

                let row_offset = event.row.saturating_sub(list_content_area.y) as usize;
                let idx = start.saturating_add(row_offset);
                if idx < suggestions.len() && row_offset < max_rows {
                    state.command_selected = idx;
                }
                return Ok(false);
            }

            if rect_contains(input_area, event.column, event.row) {
                if state.command_input.is_empty() {
                    state.command_input.push('/');
                }
                return Ok(false);
            }
        }
    }

    Ok(false)
}

fn draw(
    frame: &mut ratatui::Frame<'_>,
    state: &TuiState,
    peer_registry: &PeerRegistry,
    net_totals: &NetTotals,
) {
    frame.render_widget(Clear, frame.area());
    frame.render_widget(Block::default().style(style_base()), frame.area());

    let (header_area, main_area, sidebar_area, cmd_area) = layout_areas(state, frame.area());

    draw_header(frame, state, header_area);
    draw_sidebar(frame, state, sidebar_area);
    draw_command_bar(frame, state, cmd_area);

    match state.screen {
        Screen::Monitor => draw_monitor(frame, state, main_area),
        Screen::Stats => draw_stats(frame, state, main_area),
        Screen::Peers => draw_peers(frame, state, peer_registry, net_totals, main_area),
        Screen::Db => draw_db(frame, state, main_area),
        Screen::Mempool => draw_mempool(frame, state, main_area),
        Screen::Wallet => draw_wallet(frame, state, main_area),
        Screen::Logs => draw_logs(frame, state, main_area),
        Screen::Setup => draw_setup(frame, state, main_area),
        Screen::Help => draw_help(frame, state, main_area),
    }
}

fn draw_header(frame: &mut ratatui::Frame<'_>, state: &TuiState, area: Rect) {
    let header = header_line(state, state.screen);
    let widget = Paragraph::new(header).style(style_panel());
    frame.render_widget(widget, area);
}

fn draw_command_bar(frame: &mut ratatui::Frame<'_>, state: &TuiState, area: Rect) {
    let border_style = if state.command_mode {
        Style::default().fg(THEME.accent)
    } else {
        style_border()
    };

    let block = Block::default()
        .borders(Borders::ALL)
        .border_type(BorderType::Plain)
        .border_style(border_style)
        .style(Style::default().bg(THEME.panel));
    let inner = block.inner(area);
    frame.render_widget(block, area);
    if inner.height == 0 {
        return;
    }

    if state.command_mode {
        let chunks = Layout::default()
            .direction(Direction::Vertical)
            .constraints([Constraint::Min(0), Constraint::Length(1)])
            .split(inner);
        let list_area = chunks[0];
        let input_area = chunks[1];

        let list_layout = Layout::default()
            .direction(Direction::Horizontal)
            .constraints([Constraint::Min(0), Constraint::Length(1)])
            .split(list_area);
        let list_content_area = list_layout[0];
        let list_scrollbar_area = list_layout[1];

        let suggestions = command_suggestions(&state.command_input);
        let max_rows = list_content_area.height.max(1) as usize;
        let mut lines: Vec<Line> = Vec::new();
        if suggestions.is_empty() {
            lines.push(Line::styled("No matches", style_muted()));
        } else {
            let selected_index = state
                .command_selected
                .min(suggestions.len().saturating_sub(1));

            let max_start = suggestions.len().saturating_sub(max_rows);
            let start = selected_index.saturating_sub(max_rows / 2).min(max_start);
            let end = (start + max_rows).min(suggestions.len());

            let cmd_width = 18usize;
            let desc_width = list_content_area
                .width
                .saturating_sub(cmd_width as u16)
                .saturating_sub(14)
                .max(16) as usize;

            for (row, spec) in suggestions[start..end].iter().enumerate() {
                let idx = start + row;
                let selected = idx == selected_index;
                let base_style = if selected {
                    Style::default().bg(THEME.accent).fg(THEME.bg)
                } else {
                    Style::default()
                };

                let cmd_style = if selected {
                    base_style.add_modifier(Modifier::BOLD)
                } else {
                    style_command()
                };

                let meta_style = if selected { base_style } else { style_muted() };

                let cmd_label = shorten(spec.command, cmd_width);
                let cmd_padded = format!("{:<cmd_width$}", cmd_label, cmd_width = cmd_width);
                let desc = shorten(spec.description, desc_width);

                let line = Line::from(vec![
                    Span::styled(cmd_padded, cmd_style),
                    Span::styled(spec.category, meta_style),
                    Span::styled(" · ", meta_style),
                    Span::styled(desc, meta_style),
                ])
                .style(base_style);

                lines.push(line);
            }

            render_vertical_scrollbar(
                frame,
                list_scrollbar_area,
                suggestions.len(),
                start,
                max_rows,
            );
        }
        let list = Paragraph::new(lines)
            .style(style_panel())
            .wrap(Wrap { trim: false });
        frame.render_widget(list, list_content_area);

        let input = if state.command_input.is_empty() {
            "/".to_string()
        } else {
            state.command_input.clone()
        };
        let mut input_spans = vec![Span::styled(input, Style::default().fg(THEME.text))];
        if state.command_input.len() <= 1 {
            input_spans.push(Span::styled(
                " Type to search · Tab/Shift+Tab select · Enter run",
                style_muted(),
            ));
        }

        let input_widget = Paragraph::new(Line::from(input_spans)).style(style_panel());
        frame.render_widget(input_widget, input_area);

        let cursor_x = input_area
            .x
            .saturating_add(state.command_input.len().max(1) as u16);
        frame.set_cursor_position((
            cursor_x.min(input_area.x + input_area.width.saturating_sub(1)),
            input_area.y,
        ));
    } else {
        let mut spans = vec![
            Span::styled("/", style_key()),
            Span::raw(" command  "),
            Span::styled("Tab", style_key()),
            Span::raw(" views  "),
            Span::styled("?", style_key()),
            Span::raw(" help  "),
            Span::styled("q", style_key()),
            Span::raw(" quit"),
        ];
        if let Some(status) = state.command_status.as_ref() {
            spans.push(Span::raw("  "));
            spans.push(Span::styled(status, style_warn()));
        } else if let Some(err) = state.last_error.as_ref() {
            spans.push(Span::raw("  "));
            spans.push(Span::styled(format!("Error: {err}"), style_error()));
        } else if let Some(status) = state.startup_status.as_ref() {
            spans.push(Span::raw("  "));
            spans.push(Span::styled(status, style_warn()));
        }
        let widget = Paragraph::new(Line::from(spans)).style(style_panel());
        frame.render_widget(widget, inner);
    }
}

fn draw_sidebar(frame: &mut ratatui::Frame<'_>, state: &TuiState, area: Rect) {
    let chunks = Layout::default()
        .direction(Direction::Vertical)
        .constraints([
            Constraint::Length(11),
            Constraint::Length(3),
            Constraint::Length(3),
            Constraint::Min(0),
        ])
        .split(area);

    let mut summary = Vec::new();
    if let Some(snapshot) = state.last_snapshot.as_ref() {
        let hps = state
            .headers_per_sec
            .map(|value| format!("{value:.2}"))
            .unwrap_or_else(|| "-".to_string());
        let bps = state
            .blocks_per_sec
            .map(|value| format!("{value:.2}"))
            .unwrap_or_else(|| "-".to_string());

        summary.push(Line::from(vec![
            Span::styled("Network:", style_muted()),
            Span::raw(format!(" {}", snapshot.network)),
        ]));
        summary.push(Line::from(vec![
            Span::styled("Backend:", style_muted()),
            Span::raw(format!(" {}", snapshot.backend)),
        ]));
        summary.push(Line::from(vec![
            Span::styled("Uptime:", style_muted()),
            Span::raw(format!(" {}", format_hms(snapshot.uptime_secs))),
        ]));
        summary.push(Line::from(vec![
            Span::styled("Sync:", style_muted()),
            Span::raw(format!(" {}", snapshot.sync_state)),
        ]));
        summary.push(Line::raw(""));
        summary.push(Line::from(vec![
            Span::styled("Tip:", style_muted()),
            Span::raw(format!(
                " h{} b{}",
                snapshot.best_header_height, snapshot.best_block_height
            )),
        ]));
        summary.push(Line::from(vec![
            Span::styled("Gap:", style_muted()),
            Span::raw(format!(" {}", snapshot.header_gap)),
        ]));
        summary.push(Line::from(vec![
            Span::styled("H/s:", style_muted()),
            Span::styled(format!(" {hps}"), Style::default().fg(THEME.accent_alt)),
            Span::raw("  "),
            Span::styled("B/s:", style_muted()),
            Span::styled(format!(" {bps}"), Style::default().fg(THEME.accent)),
        ]));
    } else {
        summary.push(Line::raw("Waiting for stats..."));
    }

    let summary_widget = Paragraph::new(summary)
        .block(panel_block("Status"))
        .style(style_panel());
    frame.render_widget(summary_widget, chunks[0]);

    if let Some(snapshot) = state.last_snapshot.as_ref() {
        let header_height = snapshot.best_header_height.max(0) as f64;
        let block_height = snapshot.best_block_height.max(0) as f64;
        let ratio = if header_height > 0.0 {
            (block_height / header_height).clamp(0.0, 1.0)
        } else {
            0.0
        };
        let indexing = Gauge::default()
            .block(panel_block("Indexing"))
            .ratio(ratio)
            .label(Span::styled(
                format!("{}%", (ratio * 100.0).round() as u64),
                Style::default().fg(THEME.text),
            ))
            .style(style_panel())
            .gauge_style(Style::default().fg(THEME.bg).bg(THEME.accent));
        frame.render_widget(indexing, chunks[1]);

        let mempool_ratio = if snapshot.mempool_max_bytes > 0 {
            (snapshot.mempool_bytes as f64 / snapshot.mempool_max_bytes as f64).clamp(0.0, 1.0)
        } else {
            0.0
        };
        let mempool_mb = snapshot.mempool_bytes as f64 / (1024.0 * 1024.0);
        let mempool_cap_mb = snapshot.mempool_max_bytes as f64 / (1024.0 * 1024.0);
        let mempool = Gauge::default()
            .block(panel_block("Mempool"))
            .ratio(mempool_ratio)
            .label(Span::styled(
                format!("{mempool_mb:.1}/{mempool_cap_mb:.0} MiB"),
                Style::default().fg(THEME.text),
            ))
            .style(style_panel())
            .gauge_style(Style::default().fg(THEME.bg).bg(THEME.accent_alt));
        frame.render_widget(mempool, chunks[2]);
    }

    let helper = Paragraph::new(vec![
        Line::from(vec![
            Span::styled("Commands:", style_muted()),
            Span::raw(" /help /monitor /stats /peers /logs"),
        ]),
        Line::from(vec![
            Span::styled("Keys:", style_muted()),
            Span::raw(" Tab cycle · / command"),
        ]),
    ])
    .block(panel_block("Hints"))
    .style(style_panel())
    .wrap(Wrap { trim: false });
    frame.render_widget(helper, chunks[3]);
}

fn draw_help(frame: &mut ratatui::Frame<'_>, state: &TuiState, area: Rect) {
    let lines = vec![
        Line::raw(""),
        Line::raw("Keys:"),
        Line::raw("  /           Command mode"),
        Line::raw("  q / Esc     Quit (requests daemon shutdown)"),
        Line::raw("  Tab         Cycle views"),
        Line::raw("  Shift+Tab   Cycle views backwards"),
        Line::raw("  \u{2190}/\u{2192}       Cycle views"),
        Line::raw("  1 / m       Monitor view"),
        Line::raw("  2 / p       Peers view"),
        Line::raw("  3 / d       DB view"),
        Line::raw("  4 / t       Mempool view"),
        Line::raw("  5 / w       Wallet view"),
        Line::raw("  6 / l       Logs view"),
        Line::raw("  7           Stats view"),
        Line::raw("  ? / h       Toggle help"),
        Line::raw("  s           Toggle setup wizard"),
        Line::raw("  a           Toggle advanced metrics"),
        Line::raw(""),
        Line::raw("Setup wizard:"),
        Line::raw("  Up/Down     Highlight data dir"),
        Line::raw("  Enter       Select data dir"),
        Line::raw("  r           Rescan data dirs"),
        Line::raw("  d           Delete data dir (confirm)"),
        Line::raw("  b           Backup wallet.dat"),
        Line::raw("  n/p/l/[ / ] Network/profile/lead"),
        Line::raw(""),
        Line::raw("Peers view:"),
        Line::raw("  Up/Down     Scroll"),
        Line::raw("  PageUp/Down Page scroll"),
        Line::raw("  Home/End    Top/Bottom"),
        Line::raw(""),
        Line::raw("Logs view:"),
        Line::raw("  f           Cycle level filter"),
        Line::raw("  Space       Pause/follow toggle"),
        Line::raw("  c           Clear captured logs"),
        Line::raw("  Up/Down     Scroll"),
        Line::raw("  Home/End    Top/Follow"),
        Line::raw(""),
        Line::raw("Wallet view (in-process only):"),
        Line::raw("  Up/Down     Select address"),
        Line::raw("  Enter       Toggle QR view"),
        Line::raw("  o           Open explorer for address"),
        Line::raw("  n           New receive address (t-addr)"),
        Line::raw("  N           New Sapling address (z-addr)"),
        Line::raw("  x           Send to address"),
        Line::raw("  i           Watch address (watch-only)"),
        Line::raw(""),
        Line::raw("Notes:"),
        Line::raw("  - Normal mode is in-process (internal stats, no HTTP)."),
        Line::raw("  - Remote attach mode polls http://HOST:PORT/{stats,peers,nettotals,mempool}."),
        Line::raw("  - For a clean display, run with --log-level warn (default under --tui)."),
    ];
    let paragraph = Paragraph::new(lines)
        .block(panel_block("Help"))
        .style(style_panel());
    frame.render_widget(paragraph, area);

    if state.advanced {}
}

fn draw_setup(frame: &mut ratatui::Frame<'_>, state: &TuiState, area: Rect) {
    let Some(setup) = state.setup.as_ref() else {
        let paragraph = Paragraph::new(vec![Line::raw(
            "Setup wizard unavailable (remote attach mode).",
        )])
        .block(panel_block("Setup Wizard"))
        .style(style_panel());
        frame.render_widget(paragraph, area);
        return;
    };

    let network = match setup.network {
        Network::Mainnet => "mainnet",
        Network::Testnet => "testnet",
        Network::Regtest => "regtest",
    };

    let sections = Layout::default()
        .direction(Direction::Vertical)
        .constraints([Constraint::Min(0), Constraint::Length(6)])
        .split(area);
    let content_area = sections[0];
    let footer_area = sections[1];

    let columns = Layout::default()
        .direction(Direction::Horizontal)
        .constraints([Constraint::Percentage(60), Constraint::Percentage(40)])
        .split(content_area);

    {
        let block = panel_block("Data Sets");
        let inner = block.inner(columns[0]);
        frame.render_widget(block, columns[0]);

        if inner.width == 0 || inner.height == 0 {
        } else {
            let layout = Layout::default()
                .direction(Direction::Horizontal)
                .constraints([Constraint::Min(0), Constraint::Length(1)])
                .split(inner);
            let list_area = layout[0];
            let scrollbar_area = layout[1];

            let total = setup.data_sets.len();
            let cursor = setup.data_set_index.min(total.saturating_sub(1));
            let visible_rows = list_area.height.max(1) as usize;

            let mut lines: Vec<Line> = Vec::new();
            if total == 0 {
                lines.push(Line::styled("No data dirs found", style_muted()));
            } else {
                let max_start = total.saturating_sub(visible_rows);
                let mut start = cursor.saturating_sub(visible_rows / 2);
                start = start.min(max_start);
                let end = (start + visible_rows).min(total);

                let name_width = list_area.width.saturating_sub(6).max(8) as usize;
                for idx in start..end {
                    let path = &setup.data_sets[idx];
                    let label = path
                        .file_name()
                        .and_then(|value| value.to_str())
                        .map(|value| value.to_string())
                        .unwrap_or_else(|| path.display().to_string());
                    let label = shorten(&label, name_width);

                    let is_cursor = idx == cursor;
                    let is_active = path == &setup.active_data_dir;
                    let is_selected = path == &setup.data_dir;

                    let prefix = if is_cursor { "▶" } else { " " };
                    let selected_marker = if is_selected { "✓" } else { " " };
                    let active_marker = if is_active { "*" } else { " " };

                    let style = if is_cursor {
                        Style::default()
                            .fg(THEME.accent)
                            .add_modifier(Modifier::BOLD)
                    } else {
                        style_panel()
                    };

                    lines.push(Line::from(vec![
                        Span::styled(prefix, style),
                        Span::raw(" "),
                        Span::styled(selected_marker, style_muted()),
                        Span::styled(active_marker, style_muted()),
                        Span::raw(" "),
                        Span::styled(label, style),
                    ]));
                }

                render_vertical_scrollbar(frame, scrollbar_area, total, start, visible_rows);
            }

            let list = Paragraph::new(lines)
                .style(style_panel())
                .wrap(Wrap { trim: false });
            frame.render_widget(list, list_area);
        }
    }

    {
        let header_lead_label = if setup.header_lead == 0 {
            "unlimited".to_string()
        } else {
            setup.header_lead.to_string()
        };

        let block = panel_block("Config");
        let inner = block.inner(columns[1]);
        frame.render_widget(block, columns[1]);

        let mut lines: Vec<Line> = Vec::new();
        lines.push(Line::raw(
            "Writes a starter `flux.conf` with RPC auth + basic settings.",
        ));
        lines.push(Line::raw(""));
        lines.push(Line::from(vec![
            Span::styled("Selected:", style_muted()),
            Span::raw(format!(
                " {}",
                shorten(&setup.data_dir.display().to_string(), 64)
            )),
        ]));
        lines.push(Line::from(vec![
            Span::styled("flux.conf:", style_muted()),
            Span::raw(format!(
                " {}{}",
                shorten(&setup.conf_path.display().to_string(), 64),
                if setup.conf_path.exists() {
                    ""
                } else {
                    " (missing)"
                }
            )),
        ]));
        if let Some(cursor) = setup.data_sets.get(setup.data_set_index) {
            if cursor != &setup.data_dir {
                lines.push(Line::from(vec![
                    Span::styled("Highlighted:", style_muted()),
                    Span::raw(format!(" {}", shorten(&cursor.display().to_string(), 64))),
                ]));
            }
        }
        if setup.data_dir != setup.active_data_dir {
            lines.push(Line::from(vec![
                Span::styled("Active:", style_muted()),
                Span::raw(format!(
                    " {}",
                    shorten(&setup.active_data_dir.display().to_string(), 64)
                )),
            ]));
        }
        lines.push(Line::raw(""));
        lines.push(Line::from(vec![
            Span::styled("Network:", style_muted()),
            Span::raw(format!(" {network}")),
        ]));
        lines.push(Line::from(vec![
            Span::styled("Profile:", style_muted()),
            Span::raw(format!(" {}", setup.profile.as_str())),
        ]));
        lines.push(Line::from(vec![
            Span::styled("Header lead:", style_muted()),
            Span::raw(format!(" {header_lead_label}")),
        ]));
        lines.push(Line::raw(""));
        lines.push(Line::from(vec![
            Span::styled("rpcuser:", style_muted()),
            Span::raw(format!(" {}", setup.rpc_user)),
        ]));
        lines.push(Line::from(vec![
            Span::styled("rpcpassword:", style_muted()),
            Span::raw(format!(" {}", setup.masked_pass())),
        ]));

        let widget = Paragraph::new(lines)
            .style(style_panel())
            .wrap(Wrap { trim: false });
        frame.render_widget(widget, inner);
    }

    {
        let block = panel_block("Controls");
        let inner = block.inner(footer_area);
        frame.render_widget(block, footer_area);

        let mut lines = vec![
            Line::from(vec![
                Span::styled("Legend:", style_muted()),
                Span::raw(" ✓ selected  * active  ▶ cursor"),
            ]),
            Line::from(vec![
                Span::styled("Keys:", style_muted()),
                Span::raw(" ↑/↓ highlight  Enter select  N new dataset  r rescan  d delete"),
            ]),
            Line::raw(" b backup wallet.dat  n network  p profile  l toggle lead  [/] adjust lead"),
            Line::raw(" g regen auth  v show/hide pass  w write flux.conf  Esc back"),
        ];
        if let Some(status) = setup.status.as_ref() {
            lines.push(Line::from(vec![
                Span::styled("Status:", style_warn()),
                Span::raw(" "),
                Span::raw(shorten(status, inner.width.max(1) as usize)),
            ]));
        }

        let widget = Paragraph::new(lines)
            .style(style_panel())
            .wrap(Wrap { trim: false });
        frame.render_widget(widget, inner);
    }
}

fn draw_stats(frame: &mut ratatui::Frame<'_>, state: &TuiState, area: Rect) {
    let layout = Layout::default()
        .direction(Direction::Vertical)
        .constraints([
            Constraint::Length(7),
            Constraint::Length(7),
            Constraint::Min(0),
        ])
        .split(area);

    let snapshot = state.last_snapshot.as_ref();
    let format_supply = |value: Option<i64>| -> String {
        value
            .map(|amount| crate::format_amount(amount as i128))
            .unwrap_or_else(|| "-".to_string())
    };

    let (total, transparent, shielded, sprout, sapling) = if let Some(snapshot) = snapshot {
        (
            format_supply(snapshot.supply_total_zat),
            format_supply(snapshot.supply_transparent_zat),
            format_supply(snapshot.supply_shielded_zat),
            format_supply(snapshot.supply_sprout_zat),
            format_supply(snapshot.supply_sapling_zat),
        )
    } else {
        (
            "-".to_string(),
            "-".to_string(),
            "-".to_string(),
            "-".to_string(),
            "-".to_string(),
        )
    };

    let supply_columns = Layout::default()
        .direction(Direction::Horizontal)
        .constraints([
            Constraint::Percentage(34),
            Constraint::Percentage(33),
            Constraint::Percentage(33),
        ])
        .split(layout[0]);

    let total_lines = vec![
        Line::styled("Total supply", style_muted()),
        Line::styled(total, style_title()),
    ];
    let transparent_lines = vec![
        Line::styled("Transparent", style_muted()),
        Line::styled(transparent, Style::default().fg(THEME.accent)),
    ];
    let shielded_lines = vec![
        Line::styled("Shielded", style_muted()),
        Line::styled(shielded, Style::default().fg(THEME.accent_alt)),
    ];

    frame.render_widget(
        Paragraph::new(total_lines)
            .block(panel_block("Network Supply"))
            .style(style_panel()),
        supply_columns[0],
    );
    frame.render_widget(
        Paragraph::new(transparent_lines)
            .block(panel_block("Transparent Pool"))
            .style(style_panel()),
        supply_columns[1],
    );
    frame.render_widget(
        Paragraph::new(shielded_lines)
            .block(panel_block("Shielded Pool"))
            .style(style_panel()),
        supply_columns[2],
    );

    let shielded_lines = vec![
        Line::from(vec![
            Span::styled("Sprout:", style_muted()),
            Span::raw(format!(" {sprout}")),
        ]),
        Line::from(vec![
            Span::styled("Sapling:", style_muted()),
            Span::raw(format!(" {sapling}")),
        ]),
    ];
    let shielded_panel = Paragraph::new(shielded_lines)
        .block(panel_block("Shielded Pools"))
        .style(style_panel());
    frame.render_widget(shielded_panel, layout[1]);

    let mut chain_lines = Vec::new();
    if let Some(snapshot) = snapshot {
        chain_lines.push(Line::from(vec![
            Span::styled("Tip:", style_muted()),
            Span::raw(format!(
                " h{} b{}",
                snapshot.best_header_height, snapshot.best_block_height
            )),
        ]));
        chain_lines.push(Line::from(vec![
            Span::styled("Gap:", style_muted()),
            Span::raw(format!(" {}", snapshot.header_gap)),
        ]));
        chain_lines.push(Line::from(vec![
            Span::styled("Sync:", style_muted()),
            Span::raw(format!(" {}", snapshot.sync_state)),
        ]));
        chain_lines.push(Line::from(vec![
            Span::styled("Uptime:", style_muted()),
            Span::raw(format!(" {}s", snapshot.uptime_secs)),
        ]));
    } else {
        chain_lines.push(Line::raw("Waiting for stats..."));
    }

    let chain_panel = Paragraph::new(chain_lines)
        .block(panel_block("Chain State"))
        .style(style_panel());
    frame.render_widget(chain_panel, layout[2]);
}

fn resample_window_max(
    points: &[(f64, f64)],
    window_start_t: f64,
    window_secs: f64,
    width: usize,
) -> Vec<f64> {
    let mut out = vec![0.0; width];
    if width == 0 || points.is_empty() || !window_secs.is_finite() || window_secs <= 0.0 {
        return out;
    }

    let window_start_idx = points
        .iter()
        .position(|(t, _)| *t >= window_start_t)
        .unwrap_or(points.len());
    if window_start_idx >= points.len() {
        return out;
    }

    for (t, value) in points[window_start_idx..].iter() {
        let rel = (*t - window_start_t) / window_secs;
        if !rel.is_finite() {
            continue;
        }

        let mut idx = (rel * width as f64).floor() as isize;
        if idx < 0 {
            idx = 0;
        } else if idx as usize >= width {
            idx = width as isize - 1;
        }

        let idx = idx as usize;
        out[idx] = out[idx].max(value.max(0.0));
    }

    out
}

fn draw_monitor(frame: &mut ratatui::Frame<'_>, state: &TuiState, area: Rect) {
    let sections = Layout::default()
        .direction(Direction::Vertical)
        .constraints([
            Constraint::Percentage(50),
            Constraint::Length(1),
            Constraint::Percentage(50),
        ])
        .split(area);

    let format_rate = |value: f64| {
        if value >= 1000.0 {
            format!("{value:.0}")
        } else {
            format!("{value:.1}")
        }
    };

    let format_rel = |seconds: f64| {
        if !seconds.is_finite() {
            return "∞".to_string();
        }
        let seconds = seconds.max(0.0);
        if seconds >= 3600.0 {
            format!("{:.1}h", seconds / 3600.0)
        } else if seconds >= 60.0 {
            let secs = seconds.round().max(0.0) as u64;
            let mins = secs / 60;
            let rem = secs % 60;
            if rem == 0 {
                format!("{mins}m")
            } else {
                format!("{mins}m{rem}s")
            }
        } else if seconds >= 1.0 {
            format!("{:.0}s", seconds)
        } else {
            format!("{:.0}ms", seconds * 1000.0)
        }
    };

    let format_interval = |rate: f64| {
        if rate <= 0.0 {
            "∞".to_string()
        } else {
            format_rel(1.0 / rate)
        }
    };

    let draw_sparkline_panel = |f: &mut ratatui::Frame<'_>,
                                area: Rect,
                                title: &str,
                                hist: &RateHistory,
                                current: Option<f64>,
                                color: Color,
                                show_interval: bool| {
        let points = hist.as_vec();
        if points.is_empty() {
            let widget = Paragraph::new(vec![Line::styled(
                "Waiting for throughput...",
                style_muted(),
            )])
            .block(panel_block(title))
            .style(style_panel());
            f.render_widget(widget, area);
            return;
        }

        let values = points
            .iter()
            .map(|(_, value)| value.max(0.0))
            .collect::<Vec<_>>();

        let peak = values.iter().copied().fold(0.0_f64, f64::max);
        let avg = if values.is_empty() {
            0.0
        } else {
            values.iter().sum::<f64>() / values.len() as f64
        };
        let now = current.unwrap_or_else(|| values.last().copied().unwrap_or(0.0));

        let now_interval = show_interval.then(|| format_interval(now));

        let title_line = if area.width >= 90 {
            if let Some(interval) = now_interval.as_ref() {
                format!(
                    "{title} │ now {now:.2}/s ({interval}/b) │ avg {avg:.2}/s │ peak {peak:.2}/s"
                )
            } else {
                format!("{title} │ now {now:.1}/s │ avg {avg:.1}/s │ peak {peak:.1}/s")
            }
        } else if area.width >= 60 {
            if let Some(interval) = now_interval.as_ref() {
                format!("{title} │ {now:.2}/s ({interval}/b)")
            } else {
                format!("{title} │ {now:.1}/s")
            }
        } else {
            format!("{title} │ {now:.1}/s")
        };

        let block = panel_block(title_line);
        let inner = block.inner(area);
        f.render_widget(block, area);

        if inner.width == 0 || inner.height == 0 {
            return;
        }

        let show_y_axis = inner.width >= 34 && inner.height >= 3;
        let show_x_axis = inner.width >= 34 && inner.height >= 4;
        let axis_width = if show_y_axis { 10u16 } else { 0u16 };

        let (chart_outer, x_axis_area) = if show_x_axis {
            let chunks = Layout::default()
                .direction(Direction::Vertical)
                .constraints([Constraint::Min(0), Constraint::Length(1)])
                .split(inner);
            (chunks[0], Some(chunks[1]))
        } else {
            (inner, None)
        };

        let (y_axis_area, chart_area) = if axis_width > 0 && chart_outer.width > axis_width + 2 {
            let chunks = Layout::default()
                .direction(Direction::Horizontal)
                .constraints([Constraint::Length(axis_width), Constraint::Min(0)])
                .split(chart_outer);
            (Some(chunks[0]), chunks[1])
        } else {
            (None, chart_outer)
        };

        if chart_area.width == 0 || chart_area.height == 0 {
            return;
        }

        let width = chart_area.width as usize;

        let now_t = points
            .last()
            .map(|(t, _)| *t)
            .unwrap_or_else(|| values.last().copied().unwrap_or(0.0));
        let window_secs = 6.0 * 60.0;
        let window_start_t = now_t - window_secs;

        let visible_values = resample_window_max(&points, window_start_t, window_secs, width);

        let mut sorted = visible_values.clone();
        sorted.sort_by(|a, b| a.partial_cmp(b).unwrap_or(std::cmp::Ordering::Equal));
        let p90_idx = sorted.len().saturating_sub(1) * 90 / 100;
        let p90 = sorted.get(p90_idx).copied().unwrap_or(0.0);
        let display_max = p90.max(avg).max(now).max(1.0);
        let display_max_u64 = display_max.ceil() as u64;

        let mut bars = Vec::with_capacity(width);
        for (i, value) in visible_values.iter().enumerate() {
            let mut bar_value = value.round().max(0.0) as u64;
            if *value > 0.0 && bar_value == 0 {
                bar_value = 1;
            }

            let style = if i + 1 == width {
                Style::default().fg(color).add_modifier(Modifier::BOLD)
            } else {
                Style::default().fg(color)
            };

            bars.push(SparklineBar::from(bar_value).style(style));
        }

        let sparkline = Sparkline::default()
            .style(Style::default().fg(color).bg(THEME.panel))
            .max(display_max_u64)
            .data(bars)
            .bar_set(symbols::bar::NINE_LEVELS);

        f.render_widget(sparkline, chart_area);

        let has_y_axis = y_axis_area.is_some();
        if let Some(y_axis_area) = y_axis_area {
            let height = chart_area.height.max(1) as usize;
            let label_width = axis_width.saturating_sub(2).max(1) as usize;

            let max_label = format_rate(display_max);
            let mid_label = format_rate(display_max / 2.0);
            let mut lines: Vec<Line> = Vec::with_capacity(height);

            for row in 0..height {
                let tick_top = row == 0;
                let tick_mid = row == height / 2;
                let tick_bottom = row + 1 == height;

                let (label, marker) = if tick_top {
                    (max_label.as_str(), symbols::line::VERTICAL_RIGHT)
                } else if tick_mid {
                    (mid_label.as_str(), symbols::line::VERTICAL_RIGHT)
                } else if tick_bottom {
                    ("0", symbols::line::VERTICAL_RIGHT)
                } else {
                    ("", symbols::line::VERTICAL)
                };

                lines.push(Line::from(vec![
                    Span::styled(
                        format!("{:>label_width$}", label, label_width = label_width),
                        style_muted(),
                    ),
                    Span::raw(" "),
                    Span::styled(marker, style_muted()),
                ]));
            }

            let widget = Paragraph::new(lines).style(style_panel());
            f.render_widget(widget, y_axis_area);
        }

        if let Some(x_axis_area) = x_axis_area {
            let total_width = x_axis_area.width.max(1) as usize;
            let axis_offset = if has_y_axis { axis_width as usize } else { 0 };
            let chart_width = total_width.saturating_sub(axis_offset);

            if chart_width >= 10 {
                let left = "6m ago";
                let mid = "3m ago";
                let right = "now";

                let mut buf = vec![' '; total_width];
                let horiz = symbols::line::HORIZONTAL.chars().next().unwrap_or('─');
                let corner = symbols::line::BOTTOM_LEFT.chars().next().unwrap_or('└');

                if axis_offset > 0 {
                    if axis_offset <= total_width {
                        buf[axis_offset - 1] = corner;
                    }
                    for idx in axis_offset..total_width {
                        buf[idx] = horiz;
                    }
                } else {
                    for idx in 0..total_width {
                        buf[idx] = horiz;
                    }
                }

                let write = |buf: &mut [char], start: usize, text: &str| {
                    for (i, ch) in text.chars().enumerate() {
                        if start + i < buf.len() {
                            buf[start + i] = ch;
                        }
                    }
                };

                let left_pos = axis_offset.saturating_add(1);
                if left_pos < total_width {
                    write(&mut buf, left_pos, left);
                }

                let right_pos = total_width.saturating_sub(right.chars().count());
                if right_pos >= axis_offset {
                    write(&mut buf, right_pos, right);
                }

                let mid_len = mid.chars().count();
                if mid_len < chart_width {
                    let mid_center = axis_offset.saturating_add(chart_width / 2);
                    let mid_pos = mid_center.saturating_sub(mid_len / 2);

                    let left_limit = left_pos
                        .saturating_add(left.chars().count())
                        .saturating_add(1);
                    let right_limit = right_pos.saturating_sub(1);
                    let mid_end = mid_pos.saturating_add(mid_len);

                    if mid_pos >= left_limit
                        && mid_end <= right_limit
                        && mid_pos >= axis_offset
                        && mid_pos < total_width
                    {
                        write(&mut buf, mid_pos, mid);
                    }
                }

                let line = buf.into_iter().collect::<String>();
                let widget =
                    Paragraph::new(vec![Line::styled(line, style_muted())]).style(style_panel());
                f.render_widget(widget, x_axis_area);
            }
        }
    };

    draw_sparkline_panel(
        frame,
        sections[0],
        "Block Throughput",
        &state.bps_history,
        state.blocks_per_sec,
        THEME.accent,
        true,
    );
    draw_sparkline_panel(
        frame,
        sections[2],
        "Header Throughput",
        &state.hps_history,
        state.headers_per_sec,
        THEME.accent_alt,
        false,
    );
}

fn draw_peers(
    frame: &mut ratatui::Frame<'_>,
    state: &TuiState,
    peer_registry: &PeerRegistry,
    net_totals: &NetTotals,
    area: Rect,
) {
    let chunks = Layout::default()
        .direction(Direction::Vertical)
        .constraints([Constraint::Min(10)])
        .split(area);

    #[derive(Clone, Debug)]
    struct PeerRow {
        kind_sort: u8,
        kind: String,
        inbound: bool,
        addr: String,
        start_height: i32,
        version: i32,
        user_agent: String,
    }

    let kind_sort = |label: &str| match label {
        "block" => 0,
        "header" => 1,
        "relay" => 2,
        _ => 3,
    };

    let (_bytes_recv, _bytes_sent, _connections, mut peers) = if state.is_remote {
        let totals = state.remote_net_totals.as_ref();
        let peers = state
            .remote_peers
            .iter()
            .map(|peer| PeerRow {
                kind_sort: kind_sort(&peer.kind),
                kind: peer.kind.clone(),
                inbound: peer.inbound,
                addr: peer.addr.clone(),
                start_height: peer.start_height,
                version: peer.version,
                user_agent: peer.user_agent.clone(),
            })
            .collect::<Vec<_>>();
        (
            totals.map(|totals| totals.bytes_recv),
            totals.map(|totals| totals.bytes_sent),
            totals.map(|totals| totals.connections),
            peers,
        )
    } else {
        let totals = net_totals.snapshot();
        let peers = peer_registry
            .snapshot()
            .into_iter()
            .map(|peer| PeerRow {
                kind_sort: peer_kind_sort_key(peer.kind),
                kind: peer_kind_label(peer.kind).to_string(),
                inbound: peer.inbound,
                addr: peer.addr.to_string(),
                start_height: peer.start_height,
                version: peer.version,
                user_agent: peer.user_agent,
            })
            .collect::<Vec<_>>();
        (
            Some(totals.bytes_recv),
            Some(totals.bytes_sent),
            Some(totals.connections),
            peers,
        )
    };

    peers.sort_by(|a, b| {
        a.kind_sort
            .cmp(&b.kind_sort)
            .then_with(|| a.inbound.cmp(&b.inbound))
            .then_with(|| a.addr.cmp(&b.addr))
    });

    let block = panel_block("Peer list");
    let inner = block.inner(chunks[0]);
    frame.render_widget(block, chunks[0]);

    if inner.width == 0 || inner.height == 0 {
        return;
    }

    let layout = Layout::default()
        .direction(Direction::Horizontal)
        .constraints([Constraint::Min(0), Constraint::Length(1)])
        .split(inner);
    let table_area = layout[0];
    let scrollbar_area = layout[1];

    let header_overhead = 2u16;
    let max_rows = table_area.height.saturating_sub(header_overhead).max(1) as usize;
    let peer_count = peers.len();
    let max_scroll = peer_count.saturating_sub(max_rows);
    let scroll = (state.peers_scroll as usize).min(max_scroll);

    if peers.is_empty() {
        let mut lines = vec![
            Line::raw(""),
            Line::from(vec![Span::styled("No peers to display.", style_muted())]),
        ];
        if state.is_remote {
            lines.push(Line::raw("Remote attach mode may be waiting on /peers."));
        }
        let widget = Paragraph::new(lines).style(style_panel());
        frame.render_widget(widget, inner);
        return;
    }

    let header_row = Row::new(vec![
        Cell::from("kind"),
        Cell::from("dir"),
        Cell::from("addr"),
        Cell::from("height"),
        Cell::from("ver"),
        Cell::from("ua"),
    ])
    .style(style_title())
    .bottom_margin(1);

    let table_rows = peers.into_iter().skip(scroll).take(max_rows).map(|peer| {
        let dir = if peer.inbound { "in" } else { "out" };
        let ua = shorten(&peer.user_agent, 32);
        Row::new(vec![
            Cell::from(peer.kind),
            Cell::from(dir),
            Cell::from(peer.addr),
            Cell::from(peer.start_height.to_string()),
            Cell::from(peer.version.to_string()),
            Cell::from(ua),
        ])
    });

    let widths = [
        Constraint::Length(6),
        Constraint::Length(4),
        Constraint::Length(22),
        Constraint::Length(8),
        Constraint::Length(7),
        Constraint::Min(10),
    ];
    let table = Table::new(table_rows, widths)
        .header(header_row)
        .style(style_panel())
        .column_spacing(1);
    frame.render_widget(table, table_area);
    render_vertical_scrollbar(frame, scrollbar_area, peer_count, scroll, max_rows);
}

fn draw_db(frame: &mut ratatui::Frame<'_>, state: &TuiState, area: Rect) {
    let chunks = Layout::default()
        .direction(Direction::Vertical)
        .constraints([Constraint::Length(7), Constraint::Min(10)])
        .split(area);

    let mut summary = Vec::new();
    match state.last_snapshot.as_ref() {
        Some(snapshot) => {
            let writebuf = fmt_opt_mib(snapshot.db_write_buffer_bytes);
            let writebuf_max = fmt_opt_mib(snapshot.db_max_write_buffer_bytes);
            let journal_bytes = fmt_opt_mib(snapshot.db_journal_disk_space_bytes);
            let journal_max = fmt_opt_mib(snapshot.db_max_journal_bytes);
            let journals = fmt_opt_u64(snapshot.db_journal_count);
            let flushes = fmt_opt_u64(snapshot.db_flushes_completed);
            let compactions_active = fmt_opt_u64(snapshot.db_active_compactions);
            let compactions_done = fmt_opt_u64(snapshot.db_compactions_completed);
            let compact_s = snapshot
                .db_time_compacting_us
                .map(|us| format!("{:.1}s", us as f64 / 1_000_000.0))
                .unwrap_or_else(|| "-".to_string());

            summary.push(Line::from(vec![
                Span::styled("Write buffer:", style_muted()),
                Span::raw(format!(" {writebuf}/{writebuf_max}")),
            ]));
            summary.push(Line::from(vec![
                Span::styled("Journal:", style_muted()),
                Span::raw(format!(" {journals}  {journal_bytes}/{journal_max}")),
            ]));
            summary.push(Line::from(vec![
                Span::styled("Flushes:", style_muted()),
                Span::raw(format!(" {flushes}")),
                Span::raw("  "),
                Span::styled("Compactions:", style_muted()),
                Span::raw(format!(
                    " active {compactions_active}  done {compactions_done}  time {compact_s}"
                )),
            ]));
        }
        None => summary.push(Line::raw("Waiting for stats...")),
    }

    if let Some(err) = state.last_error.as_ref() {
        summary.push(Line::from(vec![
            Span::styled("Error:", style_error()),
            Span::raw(" "),
            Span::raw(err),
        ]));
    }

    let summary_widget = Paragraph::new(summary)
        .block(panel_block("Fjall status"))
        .style(style_panel());
    frame.render_widget(summary_widget, chunks[0]);

    let Some(snapshot) = state.last_snapshot.as_ref() else {
        return;
    };

    let rows = vec![
        (
            "utxo",
            fmt_opt_u64(snapshot.db_utxo_segments),
            fmt_opt_u64(snapshot.db_utxo_flushes_completed),
        ),
        (
            "txindex",
            fmt_opt_u64(snapshot.db_tx_index_segments),
            fmt_opt_u64(snapshot.db_tx_index_flushes_completed),
        ),
        (
            "spentindex",
            fmt_opt_u64(snapshot.db_spent_index_segments),
            fmt_opt_u64(snapshot.db_spent_index_flushes_completed),
        ),
        (
            "address_outpoint",
            fmt_opt_u64(snapshot.db_address_outpoint_segments),
            fmt_opt_u64(snapshot.db_address_outpoint_flushes_completed),
        ),
        (
            "address_delta",
            fmt_opt_u64(snapshot.db_address_delta_segments),
            fmt_opt_u64(snapshot.db_address_delta_flushes_completed),
        ),
        (
            "header_index",
            fmt_opt_u64(snapshot.db_header_index_segments),
            fmt_opt_u64(snapshot.db_header_index_flushes_completed),
        ),
    ];

    let header_row = Row::new(vec![
        Cell::from("partition"),
        Cell::from("segments"),
        Cell::from("flushes"),
    ])
    .style(style_title())
    .bottom_margin(1);

    let table_rows = rows.into_iter().map(|(name, segments, flushes)| {
        Row::new(vec![
            Cell::from(name),
            Cell::from(segments),
            Cell::from(flushes),
        ])
    });

    let widths = [
        Constraint::Length(20),
        Constraint::Length(12),
        Constraint::Length(12),
    ];
    let table = Table::new(table_rows, widths)
        .header(header_row)
        .block(panel_block("Partitions"))
        .style(style_panel())
        .column_spacing(1);
    frame.render_widget(table, chunks[1]);
}

fn draw_mempool(frame: &mut ratatui::Frame<'_>, state: &TuiState, area: Rect) {
    let mut lines = Vec::new();

    match state.last_snapshot.as_ref() {
        Some(snapshot) => {
            let mempool_mb = snapshot.mempool_bytes as f64 / (1024.0 * 1024.0);
            let mempool_cap_mb = snapshot.mempool_max_bytes as f64 / (1024.0 * 1024.0);
            let orphan_count = state
                .orphan_count
                .map(|value| value.to_string())
                .unwrap_or_else(|| "-".to_string());
            let orphan_mb = state
                .orphan_bytes
                .map(|bytes| bytes as f64 / (1024.0 * 1024.0))
                .map(|value| format!("{value:.2}"))
                .unwrap_or_else(|| "-".to_string());

            lines.push(Line::from(vec![
                Span::styled("Mempool:", style_muted()),
                Span::raw(format!(
                    " {} tx  {:.1}/{:.0} MiB",
                    snapshot.mempool_size, mempool_mb, mempool_cap_mb
                )),
                Span::raw("  "),
                Span::styled("Orphans:", style_muted()),
                Span::raw(format!(" {orphan_count} tx  {orphan_mb} MiB")),
            ]));

            lines.push(Line::from(vec![
                Span::styled("RPC:", style_muted()),
                Span::raw(format!(
                    " accept {}  reject {}",
                    snapshot.mempool_rpc_accept, snapshot.mempool_rpc_reject
                )),
                Span::raw("  "),
                Span::styled("Relay:", style_muted()),
                Span::raw(format!(
                    " accept {}  reject {}",
                    snapshot.mempool_relay_accept, snapshot.mempool_relay_reject
                )),
            ]));

            if state.advanced {
                let evicted_mb = snapshot.mempool_evicted_bytes as f64 / (1024.0 * 1024.0);
                let persisted_mb = snapshot.mempool_persisted_bytes as f64 / (1024.0 * 1024.0);
                lines.push(Line::from(vec![
                    Span::styled("Evicted:", style_muted()),
                    Span::raw(format!(
                        " {} ({:.2} MiB)",
                        snapshot.mempool_evicted, evicted_mb
                    )),
                    Span::raw("  "),
                    Span::styled("Loaded:", style_muted()),
                    Span::raw(format!(
                        " {}  (reject {})",
                        snapshot.mempool_loaded, snapshot.mempool_load_reject
                    )),
                ]));
                lines.push(Line::from(vec![
                    Span::styled("Persist:", style_muted()),
                    Span::raw(format!(
                        " writes {}  {:.2} MiB",
                        snapshot.mempool_persisted_writes, persisted_mb
                    )),
                ]));

                if let Some(detail) = state.mempool_detail.as_ref() {
                    lines.push(Line::from(vec![
                        Span::styled("Detail:", style_muted()),
                        Span::raw(format!(
                            " {} tx  {:.1} KiB",
                            detail.size,
                            detail.bytes as f64 / 1024.0
                        )),
                    ]));
                    let mut version_pairs = detail.versions.clone();
                    version_pairs.sort_by_key(|entry| entry.version);
                    let versions = version_pairs
                        .iter()
                        .map(|entry| format!("v{} {}", entry.version, entry.count))
                        .collect::<Vec<_>>()
                        .join("  ");
                    lines.push(Line::from(vec![
                        Span::styled("Versions:", style_muted()),
                        Span::raw(format!(" {versions}")),
                    ]));
                    lines.push(Line::from(vec![
                        Span::styled("Fees:", style_muted()),
                        Span::raw(format!(
                            " zero {}  nonzero {}",
                            detail.fee_zero, detail.fee_nonzero
                        )),
                    ]));
                    lines.push(Line::from(vec![
                        Span::styled("Ages:", style_muted()),
                        Span::raw(format!(
                            " newest {}  median {}  oldest {}",
                            format_age(detail.age_secs.newest_secs),
                            format_age(detail.age_secs.median_secs),
                            format_age(detail.age_secs.oldest_secs),
                        )),
                    ]));
                } else if state.is_remote {
                    lines.push(Line::from(vec![
                        Span::styled("Detail:", style_muted()),
                        Span::raw(" waiting on /mempool..."),
                    ]));
                }
            }
        }
        None => lines.push(Line::raw("Waiting for stats...")),
    }

    if let Some(err) = state.last_error.as_ref() {
        lines.push(Line::from(vec![
            Span::styled("Error:", style_error()),
            Span::raw(" "),
            Span::raw(err),
        ]));
    }

    let widget = Paragraph::new(lines)
        .block(panel_block("Pool"))
        .style(style_panel());
    frame.render_widget(widget, area);
}

fn draw_wallet(frame: &mut ratatui::Frame<'_>, state: &TuiState, area: Rect) {
    let chunks = Layout::default()
        .direction(Direction::Vertical)
        .constraints([Constraint::Length(13), Constraint::Min(10)])
        .split(area);

    let mut lines = Vec::new();

    let now = unix_seconds();
    let encrypted = state.wallet_encrypted;
    let unlocked_until = state.wallet_unlocked_until.unwrap_or(0);
    let locked = encrypted.unwrap_or(false) && (unlocked_until == 0 || unlocked_until <= now);
    let unlocked_left = unlocked_until.saturating_sub(now);

    let encrypted_label = match encrypted {
        Some(true) => "yes",
        Some(false) => "no",
        None => "-",
    };
    let status = if encrypted == Some(false) {
        "unlocked (unencrypted)"
    } else if locked {
        "locked"
    } else if encrypted == Some(true) && unlocked_until > now {
        "unlocked"
    } else {
        "-"
    };
    let unlocked_for = if encrypted == Some(true) && unlocked_until > now {
        format!("{unlocked_left}s")
    } else {
        "-".to_string()
    };

    lines.push(Line::from(vec![
        Span::styled("Encrypted:", style_muted()),
        Span::raw(format!(" {encrypted_label}  ")),
        Span::styled("Status:", style_muted()),
        Span::raw(format!(" {status}  ")),
        Span::styled("Unlocked for:", style_muted()),
        Span::raw(format!(" {unlocked_for}")),
    ]));

    let key_count = state
        .wallet_key_count
        .map(|value| value.to_string())
        .unwrap_or_else(|| "-".to_string());
    let keypool = state
        .wallet_keypool_size
        .map(|value| value.to_string())
        .unwrap_or_else(|| "-".to_string());
    let tx_count = state
        .wallet_tx_count
        .map(|value| value.to_string())
        .unwrap_or_else(|| "-".to_string());
    let sapling = match state.wallet_has_sapling_keys {
        Some(true) => "yes",
        Some(false) => "no",
        None => "-",
    };
    let paytxfee = state
        .wallet_pay_tx_fee_per_kb
        .map(|value| format!("{value} zats/kB"))
        .unwrap_or_else(|| "-".to_string());

    lines.push(Line::from(vec![
        Span::styled("Keys:", style_muted()),
        Span::raw(format!(" {key_count}  ")),
        Span::styled("Keypool:", style_muted()),
        Span::raw(format!(" {keypool}  ")),
        Span::styled("Sapling:", style_muted()),
        Span::raw(format!(" {sapling}  ")),
        Span::styled("Wallet txs:", style_muted()),
        Span::raw(format!(" {tx_count}")),
    ]));
    lines.push(Line::from(vec![
        Span::styled("paytxfee:", style_muted()),
        Span::raw(format!(" {paytxfee}")),
    ]));

    if let Some(snapshot) = state.last_snapshot.as_ref() {
        lines.push(Line::from(vec![
            Span::styled("Tip:", style_muted()),
            Span::raw(format!(
                " headers {}  blocks {}",
                snapshot.best_header_height, snapshot.best_block_height
            )),
        ]));
    }

    let transparent = state.wallet_transparent.as_ref();
    let watch = state.wallet_transparent_watchonly.as_ref();
    let sapling_spendable = fmt_opt_amount(state.wallet_sapling_spendable);
    let sapling_watchonly = fmt_opt_amount(state.wallet_sapling_watchonly);
    let sapling_notes = state
        .wallet_sapling_note_count
        .map(|value| value.to_string())
        .unwrap_or_else(|| "-".to_string());
    let scan_height = state
        .wallet_sapling_scan_height
        .and_then(|value| (value >= 0).then_some(value))
        .map(|value| value.to_string())
        .unwrap_or_else(|| "-".to_string());
    let behind = state
        .last_snapshot
        .as_ref()
        .and_then(|snap| {
            state
                .wallet_sapling_scan_height
                .and_then(|h| (h >= 0).then_some(snap.best_block_height - h))
        })
        .filter(|delta| *delta >= 0)
        .map(|delta| delta.to_string())
        .unwrap_or_else(|| "-".to_string());

    lines.push(Line::raw(""));
    lines.push(Line::from(vec![
        Span::styled("Transparent (owned):", style_muted()),
        Span::raw(format!(
            " confirmed {}  unconf {}  immature {}",
            fmt_opt_amount(transparent.map(|b| b.confirmed)),
            fmt_opt_amount(transparent.map(|b| b.unconfirmed)),
            fmt_opt_amount(transparent.map(|b| b.immature)),
        )),
    ]));
    lines.push(Line::from(vec![
        Span::styled("Transparent (watch-only):", style_muted()),
        Span::raw(format!(
            " confirmed {}  unconf {}  immature {}",
            fmt_opt_amount(watch.map(|b| b.confirmed)),
            fmt_opt_amount(watch.map(|b| b.unconfirmed)),
            fmt_opt_amount(watch.map(|b| b.immature)),
        )),
    ]));
    lines.push(Line::from(vec![
        Span::styled("Sapling:", style_muted()),
        Span::raw(format!(
            " spendable {sapling_spendable}  watch {sapling_watchonly}"
        )),
    ]));
    lines.push(Line::from(vec![
        Span::styled("Sapling scan:", style_muted()),
        Span::raw(format!(
            " height {scan_height}  behind {behind}  notes {sapling_notes}"
        )),
    ]));

    if let Some(status) = state.wallet_status.as_ref() {
        lines.push(Line::from(vec![
            Span::styled("Status:", style_warn()),
            Span::raw(" "),
            Span::raw(status),
        ]));
    }

    if state.is_remote {
        lines.push(Line::from(vec![
            Span::styled("Note:", style_warn()),
            Span::raw(" wallet controls require in-process "),
            Span::styled("--tui", style_key()),
        ]));
    } else {
        lines.push(Line::from(vec![
            Span::styled("Keys:", style_muted()),
            Span::raw(" Up/Down select  "),
            Span::styled("Enter", style_key()),
            Span::raw(" QR view  "),
            Span::styled("n", style_key()),
            Span::raw(" new t-addr  "),
            Span::styled("N", style_key()),
            Span::raw(" new z-addr  "),
            Span::styled("x", style_key()),
            Span::raw(" send  "),
            Span::styled("i", style_key()),
            Span::raw(" watch"),
        ]));
    }

    if let Some(err) = state.wallet_detail_error.as_ref() {
        lines.push(Line::from(vec![
            Span::styled("Wallet:", style_error()),
            Span::raw(" "),
            Span::raw(err),
        ]));
    }

    if let Some(err) = state.last_error.as_ref() {
        lines.push(Line::from(vec![
            Span::styled("Error:", style_error()),
            Span::raw(" "),
            Span::raw(err),
        ]));
    }

    let summary_widget = Paragraph::new(lines)
        .block(panel_block("Wallet"))
        .style(style_panel());
    frame.render_widget(summary_widget, chunks[0]);

    let lower_chunks = Layout::default()
        .direction(Direction::Horizontal)
        .constraints([Constraint::Percentage(60), Constraint::Percentage(40)])
        .split(chunks[1]);

    let left_chunks = Layout::default()
        .direction(Direction::Vertical)
        .constraints([Constraint::Percentage(55), Constraint::Percentage(45)])
        .split(lower_chunks[0]);

    let right_chunks = Layout::default()
        .direction(Direction::Vertical)
        .constraints([Constraint::Percentage(55), Constraint::Percentage(45)])
        .split(lower_chunks[1]);

    let tx_header = Row::new(vec![Cell::from("age"), Cell::from("txid")])
        .style(style_title())
        .bottom_margin(1);
    let tx_inner = panel_block("Recent wallet transactions").inner(left_chunks[0]);
    let txid_width = tx_inner.width.saturating_sub(12).max(12) as usize;
    let tx_rows = state.wallet_recent_txs.iter().map(|entry| {
        let age = unix_seconds().saturating_sub(entry.received_at);
        let txid = stats::hash256_to_hex(&entry.txid);
        Row::new(vec![
            Cell::from(format_age(age)),
            Cell::from(shorten(&txid, txid_width)),
        ])
    });
    let tx_table = Table::new(tx_rows, [Constraint::Length(10), Constraint::Min(32)])
        .header(tx_header)
        .block(panel_block("Recent wallet transactions"))
        .style(style_panel())
        .column_spacing(1);
    frame.render_widget(tx_table, left_chunks[0]);

    if state.wallet_pending_ops.is_empty() {
        let op_widget = Paragraph::new(vec![
            Line::raw("No async ops yet."),
            Line::raw("This panel tracks shielded RPC jobs"),
            Line::raw("like z_sendmany / z_shieldcoinbase."),
        ])
        .block(panel_block("Async ops"))
        .style(style_panel())
        .wrap(Wrap { trim: false });
        frame.render_widget(op_widget, left_chunks[1]);
    } else {
        let op_header = Row::new(vec![
            Cell::from("status"),
            Cell::from("method"),
            Cell::from("age"),
            Cell::from("opid"),
        ])
        .style(style_title())
        .bottom_margin(1);
        let op_rows = state.wallet_pending_ops.iter().map(|entry| {
            let age_base = entry
                .finished_time
                .or(entry.started_time)
                .unwrap_or(entry.creation_time);
            let age = unix_seconds().saturating_sub(age_base);
            let status_style = match entry.status.as_str() {
                "queued" => Style::default()
                    .fg(THEME.warning)
                    .bg(THEME.panel)
                    .add_modifier(Modifier::BOLD),
                "executing" => Style::default()
                    .fg(THEME.accent_alt)
                    .bg(THEME.panel)
                    .add_modifier(Modifier::BOLD),
                "failed" => Style::default()
                    .fg(THEME.danger)
                    .bg(THEME.panel)
                    .add_modifier(Modifier::BOLD),
                "success" => Style::default()
                    .fg(THEME.success)
                    .bg(THEME.panel)
                    .add_modifier(Modifier::BOLD),
                _ => style_panel(),
            };
            Row::new(vec![
                Cell::from(Span::styled(entry.status.clone(), status_style)),
                Cell::from(shorten(&entry.method, 12)),
                Cell::from(format_age(age)),
                Cell::from(shorten_suffix(&entry.operationid, 18)),
            ])
        });
        let op_table = Table::new(
            op_rows,
            [
                Constraint::Length(9),
                Constraint::Length(14),
                Constraint::Length(8),
                Constraint::Min(10),
            ],
        )
        .header(op_header)
        .block(panel_block("Async ops"))
        .style(style_panel())
        .column_spacing(1);
        frame.render_widget(op_table, left_chunks[1]);
    }

    let visible = state.wallet_visible_indices();
    let addr_block = panel_block("Addresses");
    let addr_inner = addr_block.inner(right_chunks[0]);
    frame.render_widget(addr_block, right_chunks[0]);

    if addr_inner.width > 0 && addr_inner.height > 0 {
        let layout = Layout::default()
            .direction(Direction::Horizontal)
            .constraints([Constraint::Min(0), Constraint::Length(1)])
            .split(addr_inner);
        let list_area = layout[0];
        let scrollbar_area = layout[1];

        let mut addr_lines: Vec<Line> = Vec::new();
        if state.is_remote {
            addr_lines.push(Line::raw("Remote attach mode: wallet unavailable."));
        } else if visible.is_empty() {
            addr_lines.push(Line::raw("No wallet addresses yet."));
            addr_lines.push(Line::raw("Press n to generate a new receive address."));
        } else {
            let selected_pos = visible
                .iter()
                .position(|idx| *idx == state.wallet_selected_address)
                .unwrap_or(0);

            let view_height = list_area.height.max(1) as usize;
            let max_start = visible.len().saturating_sub(view_height);
            let mut start = selected_pos.saturating_sub(view_height / 2);
            start = start.min(max_start);
            let end = (start + view_height).min(visible.len());

            let available_width = list_area.width.max(1) as usize;
            for pos in start..end {
                let idx = visible[pos];
                let row = &state.wallet_addresses[idx];
                let selected = idx == state.wallet_selected_address;
                let prefix = if selected { "▶" } else { " " };
                let kind = row.kind.label();
                let balance_total = row
                    .transparent_balance
                    .as_ref()
                    .map(|b| {
                        b.confirmed
                            .saturating_add(b.unconfirmed)
                            .saturating_add(b.immature)
                    })
                    .unwrap_or(0);
                let show_balance = matches!(
                    row.kind,
                    WalletAddressKind::TransparentReceive
                        | WalletAddressKind::TransparentChange
                        | WalletAddressKind::TransparentWatch
                ) && (balance_total != 0 || selected);
                let balance = if show_balance {
                    format!(" {}", crate::format_amount(balance_total as i128))
                } else {
                    String::new()
                };
                let balance_width = balance.chars().count();
                let prefix_width = 2usize;
                let kind_width = kind.len() + 1;
                let mut label = String::new();
                if let Some(value) = row.label.as_ref() {
                    let max_label = available_width
                        .saturating_sub(prefix_width + kind_width + balance_width + 20)
                        .min(12);
                    if max_label > 0 {
                        let shortened = shorten(value, max_label);
                        if !shortened.is_empty() {
                            label = format!(" ({shortened})");
                        }
                    }
                }
                let mut label_width = label.chars().count();
                let min_addr = 12usize;
                let fixed = prefix_width + kind_width + balance_width + label_width + min_addr;
                if fixed > available_width && !label.is_empty() {
                    label.clear();
                    label_width = 0;
                }
                let max_addr = available_width
                    .saturating_sub(prefix_width + kind_width + balance_width + label_width)
                    .max(min_addr);
                let addr = shorten(&row.address, max_addr);
                let style = if selected {
                    Style::default()
                        .fg(THEME.accent)
                        .bg(THEME.panel)
                        .add_modifier(Modifier::BOLD)
                } else {
                    style_panel()
                };
                addr_lines.push(Line::from(vec![
                    Span::styled(prefix, style),
                    Span::raw(" "),
                    Span::styled(kind, style_muted()),
                    Span::raw(" "),
                    Span::styled(addr, style),
                    Span::styled(label, style_muted()),
                    Span::styled(balance, style_muted()),
                ]));
            }

            render_vertical_scrollbar(frame, scrollbar_area, visible.len(), start, view_height);
        }

        let addr_widget = Paragraph::new(addr_lines).style(style_panel());
        frame.render_widget(addr_widget, list_area);
    }

    let qr_inner = panel_block("Receive QR").inner(right_chunks[1]);
    let qr_inner_width = qr_inner.width as usize;
    let qr_inner_height = qr_inner.height as usize;
    let qr_can_render = qr_inner_width >= 22 && qr_inner_height >= 12;

    let mut qr_lines: Vec<Line> = Vec::new();
    if state.is_remote {
        qr_lines.push(Line::raw("Remote attach mode: wallet unavailable."));
    } else if !state.wallet_show_qr {
        qr_lines.push(Line::raw("QR hidden. Press Enter to open."));
    } else if let Some(selected) = state.wallet_addresses.get(state.wallet_selected_address) {
        let addr_width = qr_inner_width.saturating_sub(10).max(24);
        qr_lines.push(Line::from(vec![
            Span::styled("Selected:", style_muted()),
            Span::raw(" "),
            Span::raw(shorten(&selected.address, addr_width)),
        ]));
        qr_lines.push(Line::from(vec![
            Span::styled("Actions:", style_muted()),
            Span::raw(" Enter expand  Click copy  Right-click / o open"),
        ]));
        if let Some(bal) = selected.transparent_balance.as_ref() {
            qr_lines.push(Line::from(vec![
                Span::styled("Balance:", style_muted()),
                Span::raw(format!(
                    " confirmed {}  unconf {}  immature {}",
                    crate::format_amount(bal.confirmed as i128),
                    crate::format_amount(bal.unconfirmed as i128),
                    crate::format_amount(bal.immature as i128),
                )),
            ]));
        }
        qr_lines.push(Line::raw(""));
        if qr_can_render {
            let reserved = qr_lines.len();
            let qr_budget_height = qr_inner_height.saturating_sub(reserved).max(1);
            let qr_lines_rendered =
                build_qr_lines(&selected.address, qr_inner_width, qr_budget_height);
            if qr_lines_rendered.is_empty() {
                qr_lines.push(Line::styled("QR unavailable for this size.", style_warn()));
            } else {
                qr_lines.extend(qr_lines_rendered);
            }
        } else {
            qr_lines.push(Line::styled(
                "QR preview collapsed. Press Enter to expand.",
                style_muted(),
            ));
        }
    } else {
        qr_lines.push(Line::raw("No wallet addresses yet."));
    }
    let qr_widget = Paragraph::new(qr_lines)
        .wrap(Wrap { trim: false })
        .block(panel_block("Receive QR"))
        .style(style_panel());
    frame.render_widget(qr_widget, right_chunks[1]);

    if state.wallet_modal == Some(WalletModal::Send) {
        draw_wallet_send_modal(frame, state);
    } else if state.wallet_modal == Some(WalletModal::ImportWatch) {
        draw_wallet_import_watch_modal(frame, state);
    }

    if state.wallet_qr_expanded {
        draw_wallet_qr_modal(frame, state);
    }
}

fn build_qr_lines(address: &str, max_width: usize, max_height: usize) -> Vec<Line<'static>> {
    if max_width == 0 || max_height == 0 {
        return Vec::new();
    }

    let code = match QrCode::new(address.as_bytes()) {
        Ok(code) => code,
        Err(_) => return vec![Line::raw("Failed to render QR.")],
    };

    let max_dim = max_width.min(max_height).max(1).min(QR_MAX_DIM) as u32;
    let qr = code
        .render::<unicode::Dense1x2>()
        .quiet_zone(false)
        .max_dimensions(max_dim, max_dim)
        .build();

    let mut raw_lines = qr.lines().map(str::to_string).collect::<Vec<_>>();
    if raw_lines.is_empty() {
        return Vec::new();
    }

    let content_width = raw_lines
        .iter()
        .map(|line| line.chars().count())
        .max()
        .unwrap_or(0)
        .min(max_width);

    let mut centered: Vec<Line<'static>> = Vec::new();
    let top_padding = max_height.saturating_sub(raw_lines.len()) / 2;
    for _ in 0..top_padding {
        centered.push(Line::raw(""));
    }

    for line in raw_lines.drain(..) {
        if line.chars().count() > max_width {
            return Vec::new();
        }
        let pad = max_width.saturating_sub(content_width) / 2;
        let padded = if pad == 0 {
            line
        } else {
            format!("{:<pad$}{}", "", line, pad = pad)
        };
        centered.push(Line::styled(
            padded,
            Style::default().fg(THEME.accent).bg(THEME.panel),
        ));
    }

    centered
}

fn draw_wallet_qr_modal(frame: &mut ratatui::Frame<'_>, state: &TuiState) {
    let area = wallet_qr_modal_area(frame.area());
    frame.render_widget(Clear, area);
    let block = panel_block("Receive QR");
    let inner = block.inner(area);
    frame.render_widget(block, area);

    let mut lines = Vec::new();
    if state.is_remote {
        lines.push(Line::raw("Remote attach mode: wallet unavailable."));
    } else if let Some(selected) = state.wallet_addresses.get(state.wallet_selected_address) {
        let address_width = inner.width.saturating_sub(10).max(24) as usize;
        lines.push(Line::from(vec![
            Span::styled("Address:", style_muted()),
            Span::raw(" "),
            Span::raw(shorten(selected.address.as_str(), address_width)),
        ]));
        lines.push(Line::from(vec![
            Span::styled("Actions:", style_muted()),
            Span::raw(" Click copy  Right-click / o open  Enter close"),
        ]));
        lines.push(Line::raw(""));
        let reserved = lines.len();
        let qr_budget_height = inner.height.saturating_sub(reserved as u16) as usize;
        let qr_lines = build_qr_lines(
            selected.address.as_str(),
            inner.width as usize,
            qr_budget_height,
        );
        if qr_lines.is_empty() {
            lines.push(Line::styled("QR unavailable for this size.", style_warn()));
        } else {
            lines.extend(qr_lines);
        }
    } else {
        lines.push(Line::raw("No wallet addresses yet."));
    }

    let widget = Paragraph::new(lines)
        .wrap(Wrap { trim: false })
        .style(style_panel());
    frame.render_widget(widget, inner);
}

fn draw_wallet_send_modal(frame: &mut ratatui::Frame<'_>, state: &TuiState) {
    let area = centered_rect(80, 50, frame.area());
    frame.render_widget(Clear, area);

    let mut lines: Vec<Line> = Vec::new();
    lines.push(Line::raw("Enter transaction details:"));
    lines.push(Line::raw(""));

    if state.is_remote {
        lines.push(Line::raw("Remote attach mode: wallet send is unavailable."));
    } else {
        lines.push(Line::from(vec![
            Span::styled("Keys:", style_muted()),
            Span::raw(" Tab switch  "),
            Span::styled("f", style_key()),
            Span::raw(" subtract-fee  "),
            Span::styled("Enter", style_key()),
            Span::raw(" send  "),
            Span::styled("Esc", style_key()),
            Span::raw(" close"),
        ]));
        lines.push(Line::raw(""));

        let to_focused = state.wallet_send_form.focus == WalletSendField::To;
        let amount_focused = state.wallet_send_form.focus == WalletSendField::Amount;
        let to = input_with_cursor(&state.wallet_send_form.to, to_focused);
        let amount = input_with_cursor(&state.wallet_send_form.amount, amount_focused);
        let field_style = |focused: bool| {
            if focused {
                Style::default()
                    .fg(THEME.accent)
                    .bg(THEME.panel)
                    .add_modifier(Modifier::BOLD)
            } else {
                style_panel()
            }
        };

        lines.push(Line::from(vec![
            Span::styled("To:", style_muted()),
            Span::raw(" "),
            Span::styled(to, field_style(to_focused)),
        ]));
        lines.push(Line::from(vec![
            Span::styled("Amount:", style_muted()),
            Span::raw(" "),
            Span::styled(amount, field_style(amount_focused)),
            Span::raw(" FLUX"),
        ]));
        lines.push(Line::from(vec![
            Span::styled("Subtract fee:", style_muted()),
            Span::raw(if state.wallet_send_form.subtract_fee {
                " yes"
            } else {
                " no"
            }),
        ]));
    }

    if let Some(status) = state.wallet_status.as_ref() {
        lines.push(Line::raw(""));
        lines.push(Line::from(vec![
            Span::styled("Status:", style_warn()),
            Span::raw(" "),
            Span::raw(status),
        ]));
    }

    let widget = Paragraph::new(lines)
        .wrap(Wrap { trim: false })
        .block(panel_block("Send"))
        .style(style_panel());
    frame.render_widget(widget, area);
}

fn draw_wallet_import_watch_modal(frame: &mut ratatui::Frame<'_>, state: &TuiState) {
    let area = centered_rect(80, 55, frame.area());
    frame.render_widget(Clear, area);

    let mut lines: Vec<Line> = Vec::new();
    lines.push(Line::raw(
        "Add a watch-only address (visible, not spendable):",
    ));
    lines.push(Line::raw(""));

    if state.is_remote {
        lines.push(Line::raw("Remote attach mode: wallet unavailable."));
    } else {
        lines.push(Line::from(vec![
            Span::styled("Keys:", style_muted()),
            Span::raw(" Tab switch  "),
            Span::styled("Enter", style_key()),
            Span::raw(" watch  "),
            Span::styled("Esc", style_key()),
            Span::raw(" close"),
        ]));
        lines.push(Line::raw(""));

        let address_focused =
            state.wallet_import_watch_form.focus == WalletImportWatchField::Address;
        let label_focused = state.wallet_import_watch_form.focus == WalletImportWatchField::Label;
        let address = input_with_cursor(&state.wallet_import_watch_form.address, address_focused);
        let label = input_with_cursor(&state.wallet_import_watch_form.label, label_focused);
        let field_style = |focused: bool| {
            if focused {
                Style::default()
                    .fg(THEME.accent)
                    .bg(THEME.panel)
                    .add_modifier(Modifier::BOLD)
            } else {
                style_panel()
            }
        };

        lines.push(Line::from(vec![
            Span::styled("Address:", style_muted()),
            Span::raw(" "),
            Span::styled(address, field_style(address_focused)),
        ]));
        lines.push(Line::from(vec![
            Span::styled("Label:", style_muted()),
            Span::raw(" "),
            Span::styled(label, field_style(label_focused)),
        ]));
        lines.push(Line::raw(""));
        lines.push(Line::from(vec![
            Span::styled("Tip:", style_muted()),
            Span::raw(" use RPC "),
            Span::styled("importaddress", style_key()),
            Span::raw(" for advanced options (rescan/script)."),
        ]));
    }

    if let Some(status) = state.wallet_status.as_ref() {
        lines.push(Line::raw(""));
        lines.push(Line::from(vec![
            Span::styled("Status:", style_warn()),
            Span::raw(" "),
            Span::raw(status),
        ]));
    }

    let widget = Paragraph::new(lines)
        .wrap(Wrap { trim: false })
        .block(panel_block("Watch address"))
        .style(style_panel());
    frame.render_widget(widget, area);
}

fn input_with_cursor(value: &str, focused: bool) -> String {
    if focused {
        format!("{value}▌")
    } else {
        value.to_string()
    }
}

fn centered_rect(percent_x: u16, percent_y: u16, r: Rect) -> Rect {
    let percent_x = percent_x.min(100);
    let percent_y = percent_y.min(100);

    let vertical = Layout::default()
        .direction(Direction::Vertical)
        .constraints([
            Constraint::Percentage((100 - percent_y) / 2),
            Constraint::Percentage(percent_y),
            Constraint::Percentage((100 - percent_y) / 2),
        ])
        .split(r);

    let horizontal = Layout::default()
        .direction(Direction::Horizontal)
        .constraints([
            Constraint::Percentage((100 - percent_x) / 2),
            Constraint::Percentage(percent_x),
            Constraint::Percentage((100 - percent_x) / 2),
        ])
        .split(vertical[1]);

    horizontal[1]
}

fn draw_logs(frame: &mut ratatui::Frame<'_>, state: &TuiState, area: Rect) {
    let chunks = Layout::default()
        .direction(Direction::Vertical)
        .constraints([Constraint::Length(6), Constraint::Min(10)])
        .split(area);

    let paused = if state.logs_paused { "yes" } else { "no" };
    let follow = if state.logs_follow { "yes" } else { "no" };
    let min_level = state.logs_min_level.as_str();
    let total = state.logs.len();

    let mut summary = Vec::new();
    summary.push(Line::from(vec![
        Span::styled("Filter:", style_muted()),
        Span::raw(format!(" <= {min_level}  ")),
        Span::styled("Paused:", style_muted()),
        Span::raw(format!(" {paused}  ")),
        Span::styled("Follow:", style_muted()),
        Span::raw(format!(" {follow}  ")),
        Span::styled("Lines:", style_muted()),
        Span::raw(format!(" {total}")),
    ]));
    if state.is_remote {
        summary.push(Line::from(vec![
            Span::styled("Remote attach:", style_warn()),
            Span::raw(" log capture is in-process only (start the daemon with "),
            Span::styled("--tui", style_key()),
            Span::raw(")."),
        ]));
    } else {
        summary.push(Line::from(vec![
            Span::styled("Keys:", style_muted()),
            Span::raw(" f filter  Space pause/follow  c clear  Up/Down scroll  End follow"),
        ]));
    }

    if let Some(err) = state.last_error.as_ref() {
        summary.push(Line::from(vec![
            Span::styled("Error:", style_error()),
            Span::raw(" "),
            Span::raw(err),
        ]));
    }

    let summary_widget = Paragraph::new(summary)
        .block(panel_block("Log capture"))
        .style(style_panel());
    frame.render_widget(summary_widget, chunks[0]);

    let mut lines: Vec<Line> = Vec::new();
    for entry in state.logs.iter() {
        if (entry.level as u8) > (state.logs_min_level as u8) {
            continue;
        }
        let ts = format_log_ts(entry.ts_ms);
        let level_style = log_level_style(entry.level);
        let level = entry.level.as_str();
        let target = shorten_suffix(entry.target, 36);
        let msg = sanitize_log_message(&entry.msg);
        if state.advanced {
            let location = format!("{}:{}", shorten_suffix(entry.file, 24), entry.line);
            lines.push(Line::from(vec![
                Span::styled(ts, style_muted()),
                Span::raw(" "),
                Span::styled(level, level_style),
                Span::raw(" "),
                Span::styled(
                    target,
                    Style::default().fg(THEME.accent_alt).bg(THEME.panel),
                ),
                Span::raw(" "),
                Span::styled(location, style_muted()),
                Span::raw(" "),
                Span::raw(msg),
            ]));
        } else {
            lines.push(Line::from(vec![
                Span::styled(ts, style_muted()),
                Span::raw(" "),
                Span::styled(level, level_style),
                Span::raw(" "),
                Span::styled(
                    target,
                    Style::default().fg(THEME.accent_alt).bg(THEME.panel),
                ),
                Span::raw(" "),
                Span::raw(msg),
            ]));
        }
    }
    if lines.is_empty() {
        if state.is_remote {
            lines.push(Line::raw("Remote attach mode does not stream logs."));
        } else {
            lines.push(Line::raw("No captured logs."));
        }
    }

    let block = panel_block("Log lines");
    let inner = block.inner(chunks[1]);
    frame.render_widget(block, chunks[1]);

    if inner.width == 0 || inner.height == 0 {
        return;
    }

    let layout = Layout::default()
        .direction(Direction::Horizontal)
        .constraints([Constraint::Min(0), Constraint::Length(1)])
        .split(inner);
    let content_area = layout[0];
    let scrollbar_area = layout[1];

    let view_height = content_area.height.max(1) as usize;
    let content_len = lines.len();
    let max_scroll = content_len.saturating_sub(view_height);
    let scroll = if state.logs_follow {
        max_scroll
    } else {
        (state.logs_scroll as usize).min(max_scroll)
    };
    let scroll_u16 = u16::try_from(scroll).unwrap_or(u16::MAX);

    let widget = Paragraph::new(lines)
        .scroll((scroll_u16, 0))
        .wrap(Wrap { trim: false })
        .style(style_panel());
    frame.render_widget(widget, content_area);
    render_vertical_scrollbar(frame, scrollbar_area, content_len, scroll, view_height);
}

fn wallet_address_kind_sort_key(kind: WalletAddressKind) -> u8 {
    match kind {
        WalletAddressKind::TransparentReceive => 0,
        WalletAddressKind::TransparentChange => 1,
        WalletAddressKind::TransparentWatch => 2,
        WalletAddressKind::Sapling => 3,
        WalletAddressKind::SaplingWatch => 4,
    }
}

fn peer_kind_sort_key(kind: PeerKind) -> u8 {
    match kind {
        PeerKind::Block => 0,
        PeerKind::Header => 1,
        PeerKind::Relay => 2,
    }
}

fn peer_kind_label(kind: PeerKind) -> &'static str {
    match kind {
        PeerKind::Block => "block",
        PeerKind::Header => "header",
        PeerKind::Relay => "relay",
    }
}

fn shorten(value: &str, max: usize) -> String {
    let trimmed = value.trim();
    if trimmed.len() <= max {
        return trimmed.to_string();
    }
    let end = trimmed
        .char_indices()
        .nth(max)
        .map(|(idx, _)| idx)
        .unwrap_or(trimmed.len());
    format!("{}…", trimmed[..end].trim_end())
}

fn shorten_suffix(value: &str, max: usize) -> String {
    let trimmed = value.trim();
    if max == 0 {
        return String::new();
    }
    let char_count = trimmed.chars().count();
    if char_count <= max {
        return trimmed.to_string();
    }
    let keep = max.saturating_sub(1);
    if keep == 0 {
        return "…".to_string();
    }
    let skip = char_count.saturating_sub(keep);
    let start = trimmed
        .char_indices()
        .nth(skip)
        .map(|(idx, _)| idx)
        .unwrap_or(0);
    format!("…{}", &trimmed[start..])
}

fn log_level_style(level: logging::Level) -> Style {
    match level {
        logging::Level::Error => Style::default()
            .fg(THEME.danger)
            .bg(THEME.panel)
            .add_modifier(Modifier::BOLD),
        logging::Level::Warn => Style::default()
            .fg(THEME.warning)
            .bg(THEME.panel)
            .add_modifier(Modifier::BOLD),
        logging::Level::Info => Style::default().fg(THEME.text).bg(THEME.panel),
        logging::Level::Debug => Style::default().fg(THEME.accent_alt).bg(THEME.panel),
        logging::Level::Trace => Style::default().fg(THEME.muted).bg(THEME.panel),
    }
}

fn format_log_ts(ts_ms: u64) -> String {
    const SECS_PER_DAY: u64 = 86_400;
    let secs = ts_ms / 1000;
    let millis = ts_ms % 1000;
    let secs_of_day = secs % SECS_PER_DAY;
    let hour = secs_of_day / 3600;
    let minute = (secs_of_day % 3600) / 60;
    let second = secs_of_day % 60;
    format!("{hour:02}:{minute:02}:{second:02}.{millis:03}")
}

fn sanitize_log_message(msg: &str) -> String {
    let mut out = String::with_capacity(msg.len());
    for ch in msg.chars() {
        match ch {
            '\n' => out.push_str("\\n"),
            '\r' => out.push_str("\\r"),
            '\t' => out.push_str("\\t"),
            _ => out.push(ch),
        }
    }
    out
}

fn fmt_opt_u64(value: Option<u64>) -> String {
    value
        .map(|value| value.to_string())
        .unwrap_or_else(|| "-".to_string())
}

fn fmt_opt_mib(value: Option<u64>) -> String {
    value
        .map(|bytes| bytes as f64 / (1024.0 * 1024.0))
        .map(|mib| format!("{mib:.0} MiB"))
        .unwrap_or_else(|| "-".to_string())
}

fn unix_seconds() -> u64 {
    use std::time::{SystemTime, UNIX_EPOCH};

    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .unwrap_or_default()
        .as_secs()
}

fn compute_mempool_detail(mempool: &Mutex<Mempool>) -> Option<RemoteMempoolSummary> {
    let now = unix_seconds();
    let guard = mempool.lock().ok()?;

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

    Some(RemoteMempoolSummary {
        size: guard.size() as u64,
        bytes: guard.bytes() as u64,
        fee_zero,
        fee_nonzero,
        versions: versions
            .into_iter()
            .map(|(version, count)| RemoteMempoolVersionCount { version, count })
            .collect(),
        age_secs: RemoteMempoolAgeSecs {
            newest_secs,
            median_secs,
            oldest_secs,
        },
    })
}

fn fmt_opt_amount(value: Option<i64>) -> String {
    value
        .map(|value| crate::format_amount(value as i128))
        .unwrap_or_else(|| "-".to_string())
}

fn format_age(secs: u64) -> String {
    if secs < 60 {
        return format!("{secs}s");
    }
    let mins = secs / 60;
    if mins < 60 {
        return format!("{mins}m");
    }
    let hours = mins / 60;
    if hours < 24 {
        return format!("{hours}h");
    }
    let days = hours / 24;
    format!("{days}d")
}

fn format_hms(secs: u64) -> String {
    let hours = secs / 3600;
    let mins = (secs % 3600) / 60;
    let secs = secs % 60;
    format!("{hours:02}:{mins:02}:{secs:02}")
}

#[cfg(test)]
mod tests {
    use super::*;

    fn unique_temp_dir(name: &str) -> PathBuf {
        use std::time::{SystemTime, UNIX_EPOCH};

        let nanos = SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .unwrap_or_default()
            .as_nanos();
        std::env::temp_dir().join(format!("fluxd-rust-{name}-{nanos}"))
    }

    #[test]
    fn setup_create_new_dataset_writes_starter_config() {
        let root = unique_temp_dir("tui-setup");
        fs::create_dir_all(&root).expect("create root");
        let active = root.join("data-active");
        fs::create_dir_all(&active).expect("create active");

        let mut setup = SetupWizard {
            data_dir: active.clone(),
            active_data_dir: active,
            conf_path: root.join("data-active").join("flux.conf"),
            data_sets: Vec::new(),
            data_set_index: 0,
            delete_confirm: None,
            network: Network::Mainnet,
            profile: RunProfile::High,
            header_lead: 123,
            rpc_user: "rpcuser".to_string(),
            rpc_pass: "rpcpass".to_string(),
            show_pass: false,
            status: None,
        };

        setup.create_new_data_set().expect("create dataset");

        let conf_path = root.join("data-new").join("flux.conf");
        let contents = fs::read_to_string(&conf_path).expect("read config");
        assert!(contents.contains("profile=high"));
        assert!(contents.contains("rpcuser=rpcuser"));
        assert!(contents.contains("rpcpassword=rpcpass"));
        assert!(contents.contains("headerlead=123"));

        let _ = fs::remove_dir_all(&root);
    }

    #[test]
    fn setup_write_config_for_data_dir_is_noop_when_config_exists() {
        let root = unique_temp_dir("tui-setup-existing");
        fs::create_dir_all(&root).expect("create root");
        let active = root.join("data-active");
        fs::create_dir_all(&active).expect("create active");
        let existing = root.join("data-existing");
        fs::create_dir_all(&existing).expect("create existing");

        let existing_conf = existing.join("flux.conf");
        fs::write(&existing_conf, "sentinel=1\n").expect("write sentinel");

        let mut setup = SetupWizard {
            data_dir: active.clone(),
            active_data_dir: active,
            conf_path: root.join("data-active").join("flux.conf"),
            data_sets: Vec::new(),
            data_set_index: 0,
            delete_confirm: None,
            network: Network::Mainnet,
            profile: RunProfile::High,
            header_lead: 0,
            rpc_user: "rpcuser".to_string(),
            rpc_pass: "rpcpass".to_string(),
            show_pass: false,
            status: None,
        };

        setup
            .write_config_for_data_dir(existing.clone())
            .expect("write_config_for_data_dir");

        let contents = fs::read_to_string(&existing_conf).expect("read config");
        assert_eq!(contents, "sentinel=1\n");

        let _ = fs::remove_dir_all(&root);
    }

    #[test]
    fn monitor_resample_window_max_maps_full_six_minutes() {
        let width = 180;
        let window_secs = 6.0 * 60.0;
        let now_t = window_secs;
        let window_start_t = now_t - window_secs;

        let mut points = Vec::new();
        for t in 1..=window_secs as u64 {
            let value = if t % 30 == 0 { 1.0 } else { 0.0 };
            points.push((t as f64, value));
        }

        let buckets = resample_window_max(&points, window_start_t, window_secs, width);
        assert_eq!(buckets.len(), width);
        assert_eq!(buckets[width - 1], 1.0);

        let spike_count = buckets.iter().filter(|v| **v > 0.0).count();
        assert_eq!(spike_count, 12);

        let mid = width / 2;
        assert_eq!(buckets[mid], 1.0);
    }
}
