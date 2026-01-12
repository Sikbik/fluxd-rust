use std::collections::{BTreeMap, HashSet, VecDeque};
use std::fmt::Write as FmtWrite;
use std::fs;
use std::io::{self, Read, Write};
use std::net::TcpStream;
use std::path::PathBuf;
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};

use bech32::{Bech32, Hrp};
use crossterm::cursor::{Hide, Show};
use crossterm::event::{self, Event, KeyCode, KeyEvent, KeyEventKind, KeyModifiers};
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
    Axis, Block, Borders, Cell, Chart, Clear, Dataset, Gauge, GraphType, Paragraph, Row, Table,
    Wrap,
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

const HISTORY_SAMPLES: usize = 300;
const SAMPLE_INTERVAL: Duration = Duration::from_secs(1);
const UI_TICK: Duration = Duration::from_millis(200);
const LOG_SNAPSHOT_LIMIT: usize = 4096;
const WALLET_REFRESH_INTERVAL: Duration = Duration::from_secs(5);
const WALLET_RECENT_TXS: usize = 12;
const WALLET_PENDING_OPS: usize = 12;
const PEER_SCROLL_STEP: u16 = 1;
const PEER_PAGE_STEP: u16 = 10;

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
enum Screen {
    Monitor,
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
    bg: Color::Rgb(8, 12, 20),
    panel: Color::Rgb(17, 24, 39),
    border: Color::Rgb(51, 65, 85),
    text: Color::Rgb(226, 232, 240),
    muted: Color::Rgb(148, 163, 184),
    accent: Color::Rgb(45, 212, 191),
    accent_alt: Color::Rgb(56, 189, 248),
    warning: Color::Rgb(251, 191, 36),
    danger: Color::Rgb(248, 113, 113),
    success: Color::Rgb(74, 222, 128),
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

fn style_border() -> Style {
    Style::default().fg(THEME.border).bg(THEME.panel)
}

fn style_title() -> Style {
    Style::default()
        .fg(THEME.accent_alt)
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
        .border_style(style_border())
        .style(Style::default().bg(THEME.panel))
        .title(Span::styled(title.into(), style_title()))
}

fn header_line(state: &TuiState, active: Screen) -> Line<'static> {
    let mut spans: Vec<Span<'static>> = Vec::new();
    spans.push(Span::styled(
        "fluxd-rust",
        Style::default()
            .fg(THEME.accent)
            .bg(THEME.panel)
            .add_modifier(Modifier::BOLD),
    ));
    spans.push(Span::raw("  "));

    if state.is_remote {
        spans.push(Span::styled(
            "REMOTE",
            Style::default()
                .fg(THEME.warning)
                .bg(THEME.panel)
                .add_modifier(Modifier::BOLD),
        ));
        spans.push(Span::raw("  "));
    }

    for (screen, label) in [
        (Screen::Monitor, "Monitor"),
        (Screen::Peers, "Peers"),
        (Screen::Db, "DB"),
        (Screen::Mempool, "Mempool"),
        (Screen::Wallet, "Wallet"),
        (Screen::Logs, "Logs"),
    ] {
        if screen == active {
            spans.push(Span::styled(
                format!("[{label}] "),
                Style::default()
                    .fg(THEME.accent_alt)
                    .bg(THEME.panel)
                    .add_modifier(Modifier::BOLD),
            ));
        } else {
            spans.push(Span::styled(
                format!("{label} "),
                Style::default().fg(THEME.muted).bg(THEME.panel),
            ));
        }
    }

    spans.push(Span::raw("  "));
    spans.push(Span::styled("Tab", style_key()));
    spans.push(Span::raw("/"));
    spans.push(Span::styled("Shift+Tab", style_key()));
    spans.push(Span::raw(" views  "));
    spans.push(Span::styled("?", style_key()));
    spans.push(Span::raw(" help  "));
    spans.push(Span::styled("a", style_key()));
    spans.push(Span::raw(" advanced  "));
    spans.push(Span::styled("q", style_key()));
    spans.push(Span::raw(" quit"));
    Line::from(spans)
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
    conf_path: PathBuf,
    network: Network,
    profile: RunProfile,
    rpc_user: String,
    rpc_pass: String,
    show_pass: bool,
    status: Option<String>,
}

impl SetupWizard {
    fn new(data_dir: PathBuf, network: Network) -> Self {
        let conf_path = data_dir.join("flux.conf");
        let mut wizard = Self {
            data_dir,
            conf_path,
            network,
            profile: RunProfile::Default,
            rpc_user: String::new(),
            rpc_pass: String::new(),
            show_pass: false,
            status: None,
        };
        wizard.regenerate_auth();
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
            let backup_path = self.data_dir.join(backup_name);
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

    fn bounds(&self) -> (f64, f64) {
        let Some(last) = self.points.back() else {
            return (0.0, 1.0);
        };
        let min_t = self.points.front().map(|point| point.t).unwrap_or(last.t);
        (min_t, last.t.max(min_t + 1.0))
    }

    fn max_y(&self) -> f64 {
        self.points
            .iter()
            .map(|point| point.value)
            .fold(0.0, f64::max)
            .max(1.0)
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
            }
        }
    }

    fn cycle_screen(&mut self) {
        self.screen = match self.screen {
            Screen::Monitor => Screen::Peers,
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
            Screen::Peers => Screen::Monitor,
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

struct TerminalGuard;

impl TerminalGuard {
    fn enter() -> Result<Self, String> {
        enable_raw_mode().map_err(|err| err.to_string())?;
        let mut stdout = io::stdout();
        execute!(stdout, EnterAlternateScreen, Hide).map_err(|err| err.to_string())?;
        Ok(Self)
    }
}

impl Drop for TerminalGuard {
    fn drop(&mut self) {
        let _ = disable_raw_mode();
        let mut stdout = io::stdout();
        let _ = execute!(stdout, Show, LeaveAlternateScreen);
    }
}

pub fn run_tui(
    chainstate: Arc<ChainState<Store>>,
    store: Arc<Store>,
    data_dir: PathBuf,
    sync_metrics: Arc<SyncMetrics>,
    header_metrics: Arc<HeaderMetrics>,
    validation_metrics: Arc<ValidationMetrics>,
    connect_metrics: Arc<ConnectMetrics>,
    mempool: Arc<Mutex<Mempool>>,
    mempool_policy: Arc<MempoolPolicy>,
    mempool_metrics: Arc<MempoolMetrics>,
    fee_estimator: Arc<Mutex<FeeEstimator>>,
    tx_confirm_target: u32,
    mempool_flags: ValidationFlags,
    wallet: Arc<Mutex<Wallet>>,
    tx_announce: broadcast::Sender<Hash256>,
    net_totals: Arc<NetTotals>,
    peer_registry: Arc<PeerRegistry>,
    chain_params: Arc<ChainParams>,
    network: Network,
    storage_backend: Backend,
    start_time: Instant,
    shutdown_rx: watch::Receiver<bool>,
    shutdown_tx: watch::Sender<bool>,
) -> Result<(), String> {
    let _guard = TerminalGuard::enter()?;
    let stdout = io::stdout();
    let term_backend = CrosstermBackend::new(stdout);
    let mut terminal = Terminal::new(term_backend).map_err(|err| err.to_string())?;
    terminal.clear().map_err(|err| err.to_string())?;

    let mut state = TuiState::new();
    state.setup = Some(SetupWizard::new(data_dir, network));
    let mut next_sample = Instant::now();
    let mut next_wallet_refresh = Instant::now();
    let wallet_ops = InProcessWalletOps {
        chainstate: chainstate.as_ref(),
        mempool: mempool.as_ref(),
        mempool_policy: mempool_policy.as_ref(),
        mempool_metrics: mempool_metrics.as_ref(),
        fee_estimator: fee_estimator.as_ref(),
        tx_confirm_target,
        mempool_flags: &mempool_flags,
        wallet: wallet.as_ref(),
        chain_params: chain_params.as_ref(),
        tx_announce: &tx_announce,
    };

    loop {
        if *shutdown_rx.borrow() {
            break;
        }

        let now = Instant::now();
        if now >= next_sample {
            match stats::snapshot_stats(
                chainstate.as_ref(),
                Some(store.as_ref()),
                network,
                storage_backend,
                start_time,
                Some(sync_metrics.as_ref()),
                Some(header_metrics.as_ref()),
                Some(validation_metrics.as_ref()),
                Some(connect_metrics.as_ref()),
                Some(mempool.as_ref()),
                Some(mempool_metrics.as_ref()),
            ) {
                Ok(snapshot) => {
                    state.update_snapshot(snapshot);
                    let (orphan_count, orphan_bytes) = match mempool.lock() {
                        Ok(guard) => (Some(guard.orphan_count()), Some(guard.orphan_bytes())),
                        Err(_) => (None, None),
                    };
                    state.update_orphans(orphan_count, orphan_bytes);
                    if matches!(state.screen, Screen::Mempool) && state.advanced {
                        state.mempool_detail = compute_mempool_detail(mempool.as_ref());
                    } else {
                        state.mempool_detail = None;
                    }

                    let wallet_snapshot = match wallet.lock() {
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
                match refresh_wallet_details(
                    chainstate.as_ref(),
                    mempool.as_ref(),
                    wallet.as_ref(),
                    tip_height,
                ) {
                    Ok(details) => {
                        state.wallet_transparent = Some(details.transparent_owned);
                        state.wallet_transparent_watchonly = Some(details.transparent_watchonly);
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
                        if handle_key(key, &mut state, &shutdown_tx, Some(&wallet_ops))? {
                            break;
                        }
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
    Ok(())
}

pub fn run_remote_tui(endpoint: String) -> Result<(), String> {
    let endpoint = endpoint.trim().to_string();
    if endpoint.is_empty() {
        return Err("missing --tui-attach endpoint".to_string());
    }

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
                Event::Resize(_, _) => {
                    terminal.clear().map_err(|err| err.to_string())?;
                }
                _ => {}
            }
        }
    }

    terminal.show_cursor().map_err(|err| err.to_string())?;
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

fn handle_key(
    key: KeyEvent,
    state: &mut TuiState,
    shutdown_tx: &watch::Sender<bool>,
    wallet_ops: Option<&InProcessWalletOps<'_>>,
) -> Result<bool, String> {
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
                    let _ = shutdown_tx.send(true);
                    return Ok(true);
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
                        state.wallet_status =
                            Some("Remote attach mode: wallet send unavailable.".to_string());
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
                    let _ = shutdown_tx.send(true);
                    return Ok(true);
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
                        state.wallet_status =
                            Some("Remote attach mode: wallet unavailable.".to_string());
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

    match (key.code, key.modifiers) {
        (KeyCode::Char('q'), _) | (KeyCode::Esc, _) => {
            let _ = shutdown_tx.send(true);
            Ok(true)
        }
        (KeyCode::Char('c'), KeyModifiers::CONTROL) => {
            let _ = shutdown_tx.send(true);
            Ok(true)
        }
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
                state.wallet_show_qr = !state.wallet_show_qr;
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
                    state.wallet_status =
                        Some("Remote attach mode: wallet unavailable.".to_string());
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
                    state.wallet_status =
                        Some("Remote attach mode: wallet unavailable.".to_string());
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
        (KeyCode::Char('h'), _) => {
            state.toggle_help();
            Ok(false)
        }
        _ => Ok(false),
    }
}

fn draw(
    frame: &mut ratatui::Frame<'_>,
    state: &TuiState,
    peer_registry: &PeerRegistry,
    net_totals: &NetTotals,
) {
    frame.render_widget(Clear, frame.area());
    frame.render_widget(Block::default().style(style_base()), frame.area());
    match state.screen {
        Screen::Monitor => draw_monitor(frame, state),
        Screen::Peers => draw_peers(frame, state, peer_registry, net_totals),
        Screen::Db => draw_db(frame, state),
        Screen::Mempool => draw_mempool(frame, state),
        Screen::Wallet => draw_wallet(frame, state),
        Screen::Logs => draw_logs(frame, state),
        Screen::Setup => draw_setup(frame, state),
        Screen::Help => draw_help(frame, state),
    }
}

fn draw_help(frame: &mut ratatui::Frame<'_>, state: &TuiState) {
    let lines = vec![
        header_line(state, state.help_return),
        Line::raw(""),
        Line::raw("Keys:"),
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
        Line::raw("  ? / h       Toggle help"),
        Line::raw("  s           Toggle setup wizard"),
        Line::raw("  a           Toggle advanced metrics"),
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
        Line::raw("  Enter       Toggle QR code"),
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
    frame.render_widget(paragraph, frame.area());

    if state.advanced {
        // no-op for now; keeps the state meaningful on the help page.
    }
}

fn draw_setup(frame: &mut ratatui::Frame<'_>, state: &TuiState) {
    let mut lines: Vec<Line> = Vec::new();
    lines.push(header_line(state, state.setup_return));
    lines.push(Line::raw(""));
    match state.setup.as_ref() {
        Some(setup) => {
            let network = match setup.network {
                Network::Mainnet => "mainnet",
                Network::Testnet => "testnet",
                Network::Regtest => "regtest",
            };
            lines.push(Line::raw(
                "Writes a starter `flux.conf` with RPC auth + basic settings.",
            ));
            lines.push(Line::raw(""));
            lines.push(Line::from(vec![
                Span::styled("Data dir:", style_muted()),
                Span::raw(format!(" {}", setup.data_dir.display())),
            ]));
            lines.push(Line::from(vec![
                Span::styled("flux.conf:", style_muted()),
                Span::raw(format!(" {}", setup.conf_path.display())),
            ]));
            lines.push(Line::raw(""));
            lines.push(Line::from(vec![
                Span::styled("Network:", style_muted()),
                Span::raw(format!(" {network}")),
            ]));
            lines.push(Line::from(vec![
                Span::styled("Profile:", style_muted()),
                Span::raw(format!(" {}", setup.profile.as_str())),
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
            lines.push(Line::raw(""));
            lines.push(Line::from(vec![
                Span::styled("Keys:", style_muted()),
                Span::raw(" n network  p profile  g regen auth  v show/hide pass  w write flux.conf  Esc back"),
            ]));

            if let Some(status) = setup.status.as_ref() {
                lines.push(Line::raw(""));
                lines.push(Line::from(vec![
                    Span::styled("Status:", style_warn()),
                    Span::raw(" "),
                    Span::raw(status),
                ]));
            }
        }
        None => {
            lines.push(Line::raw("Setup wizard unavailable (remote attach mode)."));
        }
    }

    let paragraph = Paragraph::new(lines)
        .block(panel_block("Setup Wizard"))
        .style(style_panel());
    frame.render_widget(paragraph, frame.area());
}

fn draw_monitor(frame: &mut ratatui::Frame<'_>, state: &TuiState) {
    let area = frame.area();
    let chunks = Layout::default()
        .direction(Direction::Vertical)
        .constraints([Constraint::Length(8), Constraint::Min(10)])
        .split(area);

    let mut summary = Vec::new();
    summary.push(header_line(state, Screen::Monitor));

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
            Span::raw(format!(" {}  ", snapshot.network)),
            Span::styled("Backend:", style_muted()),
            Span::raw(format!(" {}  ", snapshot.backend)),
            Span::styled("Uptime:", style_muted()),
            Span::raw(format!(" {}", format_hms(snapshot.uptime_secs))),
            Span::raw("  "),
            Span::styled("Sync:", style_muted()),
            Span::raw(format!(" {}", snapshot.sync_state)),
        ]));

        summary.push(Line::from(vec![
            Span::styled("Tip:", style_muted()),
            Span::raw(format!(
                " headers {}  blocks {}  gap {}  ",
                snapshot.best_header_height, snapshot.best_block_height, snapshot.header_gap
            )),
            Span::styled("Rates:", style_muted()),
            Span::styled(" h/s ", style_muted()),
            Span::styled(hps, Style::default().fg(THEME.accent_alt).bg(THEME.panel)),
            Span::styled("  b/s ", style_muted()),
            Span::styled(bps, Style::default().fg(THEME.accent).bg(THEME.panel)),
        ]));

        let mempool_mb = snapshot.mempool_bytes as f64 / (1024.0 * 1024.0);
        let mempool_cap_mb = snapshot.mempool_max_bytes as f64 / (1024.0 * 1024.0);
        summary.push(Line::from(vec![
            Span::styled("Mempool:", style_muted()),
            Span::raw(format!(
                " {} tx  {:.1}/{:.0} MiB",
                snapshot.mempool_size, mempool_mb, mempool_cap_mb
            )),
        ]));

        if state.advanced {
            let writebuf_mb = snapshot
                .db_write_buffer_bytes
                .map(|bytes| bytes as f64 / (1024.0 * 1024.0))
                .map(|value| format!("{value:.0}"))
                .unwrap_or_else(|| "-".to_string());
            let writebuf_max_mb = snapshot
                .db_max_write_buffer_bytes
                .map(|bytes| bytes as f64 / (1024.0 * 1024.0))
                .map(|value| format!("{value:.0}"))
                .unwrap_or_else(|| "-".to_string());
            let compactions = snapshot
                .db_active_compactions
                .map(|value| value.to_string())
                .unwrap_or_else(|| "-".to_string());
            summary.push(Line::from(vec![
                Span::styled("DB:", style_muted()),
                Span::raw(format!(
                    " writebuf {writebuf_mb}/{writebuf_max_mb} MiB  compactions {compactions}"
                )),
            ]));
        }
    } else {
        summary.push(Line::raw(""));
        summary.push(Line::raw("Waiting for stats..."));
    }

    if let Some(err) = state.last_error.as_ref() {
        summary.push(Line::from(vec![
            Span::styled("Error:", style_error()),
            Span::raw(" "),
            Span::raw(err),
        ]));
    }

    let summary_widget = Paragraph::new(summary)
        .block(panel_block("Status"))
        .style(style_panel());
    frame.render_widget(summary_widget, chunks[0]);

    let (x_min, x_max) = state.bps_history.bounds();
    let y_max = state.bps_history.max_y().max(state.hps_history.max_y());
    let y_max = (y_max * 1.1).ceil().max(1.0);
    let y_mid = (y_max / 2.0).ceil().max(1.0);
    let window = (x_max - x_min).max(1.0);

    let bps_points = state.bps_history.as_vec();
    let hps_points = state.hps_history.as_vec();

    let hps_now = state
        .headers_per_sec
        .map(|value| format!("{value:.1}"))
        .unwrap_or_else(|| "-".to_string());
    let bps_now = state
        .blocks_per_sec
        .map(|value| format!("{value:.1}"))
        .unwrap_or_else(|| "-".to_string());

    let datasets = vec![
        Dataset::default()
            .name("b/s")
            .marker(symbols::Marker::Braille)
            .graph_type(GraphType::Line)
            .data(&bps_points)
            .style(Style::default().fg(THEME.accent)),
        Dataset::default()
            .name("h/s")
            .marker(symbols::Marker::Braille)
            .graph_type(GraphType::Line)
            .data(&hps_points)
            .style(Style::default().fg(THEME.accent_alt)),
    ];

    let chart = Chart::new(datasets)
        .block(panel_block(format!(
            "Throughput (b/s {bps_now} · h/s {hps_now})"
        )))
        .style(style_panel())
        .x_axis(
            Axis::default()
                .style(style_muted())
                .labels(vec![
                    Span::styled(format!("-{}", fmt_window(window)), style_muted()),
                    Span::styled(format!("-{}", fmt_window(window / 2.0)), style_muted()),
                    Span::styled("now", style_muted()),
                ])
                .bounds([x_min, x_max]),
        )
        .y_axis(
            Axis::default()
                .style(style_muted())
                .labels(vec![
                    Span::styled("0", style_muted()),
                    Span::styled(format!("{y_mid:.0}"), style_muted()),
                    Span::styled(format!("{y_max:.0}"), style_muted()),
                ])
                .bounds([0.0, y_max]),
        );

    let bottom = Layout::default()
        .direction(Direction::Horizontal)
        .constraints([Constraint::Percentage(70), Constraint::Percentage(30)])
        .split(chunks[1]);
    frame.render_widget(chart, bottom[0]);

    let side = Layout::default()
        .direction(Direction::Vertical)
        .constraints([
            Constraint::Length(3),
            Constraint::Length(3),
            Constraint::Min(0),
        ])
        .split(bottom[1]);

    if let Some(snapshot) = state.last_snapshot.as_ref() {
        let header_height = snapshot.best_header_height.max(0) as f64;
        let block_height = snapshot.best_block_height.max(0) as f64;
        let ratio = if header_height > 0.0 {
            (block_height / header_height).clamp(0.0, 1.0)
        } else {
            0.0
        };
        let indexing_label = format!(
            "{} / {}",
            snapshot.best_block_height, snapshot.best_header_height
        );
        let indexing = Gauge::default()
            .block(panel_block("Indexing"))
            .ratio(ratio)
            .label(Span::styled(
                indexing_label,
                Style::default().fg(THEME.text),
            ))
            .style(style_panel())
            .gauge_style(Style::default().fg(THEME.bg).bg(THEME.accent));
        frame.render_widget(indexing, side[0]);

        let mempool_ratio = if snapshot.mempool_max_bytes > 0 {
            (snapshot.mempool_bytes as f64 / snapshot.mempool_max_bytes as f64).clamp(0.0, 1.0)
        } else {
            0.0
        };
        let mempool_mb = snapshot.mempool_bytes as f64 / (1024.0 * 1024.0);
        let mempool_cap_mb = snapshot.mempool_max_bytes as f64 / (1024.0 * 1024.0);
        let mempool_label = format!("{:.1}/{:.0} MiB", mempool_mb, mempool_cap_mb);
        let mempool = Gauge::default()
            .block(panel_block("Mempool"))
            .ratio(mempool_ratio)
            .label(Span::styled(mempool_label, Style::default().fg(THEME.text)))
            .style(style_panel())
            .gauge_style(Style::default().fg(THEME.bg).bg(THEME.accent_alt));
        frame.render_widget(mempool, side[1]);

        let helper = Paragraph::new(vec![
            Line::from(vec![
                Span::styled("Gap:", style_muted()),
                Span::raw(" headers - blocks (indexing lag)"),
            ]),
            Line::from(vec![
                Span::styled("Tip:", style_muted()),
                Span::raw(" best known header height"),
            ]),
        ])
        .block(panel_block("Help"))
        .style(style_panel())
        .wrap(Wrap { trim: false });
        frame.render_widget(helper, side[2]);
    } else {
        let helper = Paragraph::new(vec![Line::raw("Waiting for stats...")])
            .block(panel_block("Status"))
            .style(style_panel());
        frame.render_widget(helper, bottom[1]);
    }
}

fn draw_peers(
    frame: &mut ratatui::Frame<'_>,
    state: &TuiState,
    peer_registry: &PeerRegistry,
    net_totals: &NetTotals,
) {
    let area = frame.area();
    let chunks = Layout::default()
        .direction(Direction::Vertical)
        .constraints([Constraint::Length(7), Constraint::Min(10)])
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

    let (bytes_recv, bytes_sent, connections, mut peers) = if state.is_remote {
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

    let mut block_peers = 0usize;
    let mut header_peers = 0usize;
    let mut relay_peers = 0usize;
    for peer in &peers {
        match peer.kind.as_str() {
            "block" => block_peers += 1,
            "header" => header_peers += 1,
            "relay" => relay_peers += 1,
            _ => {}
        }
    }

    let header = header_line(state, Screen::Peers);

    let recv_mb = bytes_recv
        .map(|value| format!("{:.1}", value as f64 / (1024.0 * 1024.0)))
        .unwrap_or_else(|| "-".to_string());
    let sent_mb = bytes_sent
        .map(|value| format!("{:.1}", value as f64 / (1024.0 * 1024.0)))
        .unwrap_or_else(|| "-".to_string());
    let connections = connections
        .map(|value| value.to_string())
        .unwrap_or_else(|| "-".to_string());

    peers.sort_by(|a, b| {
        a.kind_sort
            .cmp(&b.kind_sort)
            .then_with(|| a.inbound.cmp(&b.inbound))
            .then_with(|| a.addr.cmp(&b.addr))
    });

    let max_rows = chunks[1].height.saturating_sub(3) as usize;
    let max_scroll = peers.len().saturating_sub(max_rows);
    let scroll = (state.peers_scroll as usize).min(max_scroll);
    let shown_start = if peers.is_empty() {
        0
    } else {
        scroll.saturating_add(1)
    };
    let shown_end = (scroll + max_rows).min(peers.len());
    let mut summary = Vec::new();
    summary.push(header);
    summary.push(Line::from(vec![
        Span::styled("Connections:", style_muted()),
        Span::raw(format!(
            " {}  (block {block_peers}  header {header_peers}  relay {relay_peers})",
            connections
        )),
    ]));
    summary.push(Line::from(vec![
        Span::styled("Net totals:", style_muted()),
        Span::raw(format!(" recv {recv_mb} MiB  sent {sent_mb} MiB")),
    ]));
    if let Some(snapshot) = state.last_snapshot.as_ref() {
        summary.push(Line::from(vec![
            Span::styled("Tip:", style_muted()),
            Span::raw(format!(
                " headers {}  blocks {}",
                snapshot.best_header_height, snapshot.best_block_height
            )),
        ]));
    }
    summary.push(Line::from(vec![
        Span::styled("Showing:", style_muted()),
        Span::raw(format!(
            " {shown_start}-{shown_end} of {}  (Up/Down, PgUp/PgDn)",
            peers.len()
        )),
    ]));

    let summary_widget = Paragraph::new(summary)
        .block(panel_block("Network"))
        .style(style_panel());
    frame.render_widget(summary_widget, chunks[0]);

    if peers.is_empty() {
        let mut lines = vec![
            Line::raw(""),
            Line::from(vec![Span::styled("No peers to display.", style_muted())]),
        ];
        if state.is_remote {
            lines.push(Line::raw("Remote attach mode may be waiting on /peers."));
        }
        let widget = Paragraph::new(lines)
            .block(panel_block("Peer list"))
            .style(style_panel());
        frame.render_widget(widget, chunks[1]);
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
        .block(panel_block("Peer list"))
        .style(style_panel())
        .column_spacing(1);
    frame.render_widget(table, chunks[1]);
}

fn draw_db(frame: &mut ratatui::Frame<'_>, state: &TuiState) {
    let area = frame.area();
    let chunks = Layout::default()
        .direction(Direction::Vertical)
        .constraints([Constraint::Length(7), Constraint::Min(10)])
        .split(area);

    let header = header_line(state, Screen::Db);

    let mut summary = Vec::new();
    summary.push(header);
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

fn draw_mempool(frame: &mut ratatui::Frame<'_>, state: &TuiState) {
    let header = header_line(state, Screen::Mempool);

    let mut lines = Vec::new();
    lines.push(header);

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
    frame.render_widget(widget, frame.area());
}

fn draw_wallet(frame: &mut ratatui::Frame<'_>, state: &TuiState) {
    let area = frame.area();
    let chunks = Layout::default()
        .direction(Direction::Vertical)
        .constraints([Constraint::Length(13), Constraint::Min(10)])
        .split(area);

    let header = header_line(state, Screen::Wallet);

    let mut lines = Vec::new();
    lines.push(header);

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
            Span::raw(" QR  "),
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
    let tx_rows = state.wallet_recent_txs.iter().map(|entry| {
        let age = unix_seconds().saturating_sub(entry.received_at);
        Row::new(vec![
            Cell::from(format_age(age)),
            Cell::from(shorten_suffix(&stats::hash256_to_hex(&entry.txid), 40)),
        ])
    });
    let tx_table = Table::new(tx_rows, [Constraint::Length(10), Constraint::Min(10)])
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
        let view_height = right_chunks[0].height.saturating_sub(2) as usize;
        let view_height = view_height.max(1);
        let max_start = visible.len().saturating_sub(view_height);
        let mut start = selected_pos.saturating_sub(view_height / 2);
        start = start.min(max_start);
        let end = (start + view_height).min(visible.len());

        if start > 0 {
            addr_lines.push(Line::styled("↑ more…", style_muted()));
        }

        let max_addr = right_chunks[0].width.saturating_sub(28).max(16) as usize;
        for pos in start..end {
            let idx = visible[pos];
            let row = &state.wallet_addresses[idx];
            let selected = pos == selected_pos;
            let prefix = if selected { "▶" } else { " " };
            let kind = row.kind.label();
            let addr = shorten_suffix(&row.address, max_addr);
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
            let label = row
                .label
                .as_ref()
                .map(|value| shorten(value, 16))
                .filter(|value| !value.is_empty());
            let label = label
                .as_ref()
                .map(|value| format!(" ({value})"))
                .unwrap_or_default();
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

        if end < visible.len() {
            addr_lines.push(Line::styled("↓ more…", style_muted()));
        }
    }

    let addr_widget = Paragraph::new(addr_lines)
        .block(panel_block("Addresses"))
        .style(style_panel());
    frame.render_widget(addr_widget, right_chunks[0]);

    let qr_inner_width = right_chunks[1].width.saturating_sub(2) as usize;
    let qr_inner_height = right_chunks[1].height.saturating_sub(2) as usize;

    let mut qr_lines: Vec<Line> = Vec::new();
    if state.is_remote {
        qr_lines.push(Line::raw("Remote attach mode: wallet unavailable."));
    } else if !state.wallet_show_qr {
        qr_lines.push(Line::raw("QR hidden. Press Enter to show."));
    } else if let Some(selected) = state.wallet_addresses.get(state.wallet_selected_address) {
        qr_lines.push(Line::from(vec![
            Span::styled("Selected:", style_muted()),
            Span::raw(" "),
            Span::raw(shorten_suffix(&selected.address, 64)),
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
        let reserved = qr_lines.len();
        let qr_budget_height = qr_inner_height.saturating_sub(reserved).max(1);
        match QrCode::new(selected.address.as_bytes()) {
            Ok(code) => {
                let qr = code.render::<unicode::Dense1x2>().quiet_zone(false).build();
                let qr_height = qr.lines().count();
                let qr_width = qr
                    .lines()
                    .map(|line| line.chars().count())
                    .max()
                    .unwrap_or(0);
                if qr_width > qr_inner_width || qr_height > qr_budget_height {
                    qr_lines.push(Line::from(vec![
                        Span::styled("Terminal too small for QR.", style_warn()),
                        Span::raw(" "),
                        Span::styled("Enter", style_key()),
                        Span::raw(" hide."),
                    ]));
                    qr_lines.push(Line::from(vec![
                        Span::styled("Need:", style_muted()),
                        Span::raw(format!(" {qr_width}x{qr_height}")),
                        Span::raw("  "),
                        Span::styled("Have:", style_muted()),
                        Span::raw(format!(" {qr_inner_width}x{qr_inner_height}")),
                    ]));
                } else {
                    for line in qr.lines().take(qr_budget_height) {
                        qr_lines.push(Line::styled(
                            line.to_string(),
                            Style::default().fg(THEME.accent).bg(THEME.panel),
                        ));
                    }
                }
            }
            Err(_) => qr_lines.push(Line::raw("Failed to render QR.")),
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

fn draw_logs(frame: &mut ratatui::Frame<'_>, state: &TuiState) {
    let area = frame.area();
    let chunks = Layout::default()
        .direction(Direction::Vertical)
        .constraints([Constraint::Length(6), Constraint::Min(10)])
        .split(area);

    let header = header_line(state, Screen::Logs);

    let paused = if state.logs_paused { "yes" } else { "no" };
    let follow = if state.logs_follow { "yes" } else { "no" };
    let min_level = state.logs_min_level.as_str();
    let total = state.logs.len();

    let mut summary = Vec::new();
    summary.push(header);
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

    let view_height = chunks[1].height.saturating_sub(2) as usize;
    let max_scroll = lines.len().saturating_sub(view_height);
    let scroll = if state.logs_follow {
        max_scroll
    } else {
        (state.logs_scroll as usize).min(max_scroll)
    };
    let scroll_u16 = u16::try_from(scroll).unwrap_or(u16::MAX);

    let widget = Paragraph::new(lines)
        .scroll((scroll_u16, 0))
        .block(panel_block("Log lines"))
        .style(style_panel());
    frame.render_widget(widget, chunks[1]);
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

fn fmt_window(window_secs: f64) -> String {
    let secs = window_secs.round().max(0.0) as u64;
    if secs < 60 {
        return format!("{secs}s");
    }
    if secs < 3600 {
        let mins = secs / 60;
        let rem = secs % 60;
        if rem == 0 {
            return format!("{mins}m");
        }
        return format!("{mins}m{rem:02}s");
    }
    let hours = secs / 3600;
    let mins = (secs % 3600) / 60;
    if mins == 0 {
        format!("{hours}h")
    } else {
        format!("{hours}h{mins}m")
    }
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
