use std::collections::VecDeque;
use std::io;
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};

use crossterm::cursor::{Hide, Show};
use crossterm::event::{self, Event, KeyCode, KeyEvent, KeyEventKind, KeyModifiers};
use crossterm::execute;
use crossterm::terminal::{
    disable_raw_mode, enable_raw_mode, EnterAlternateScreen, LeaveAlternateScreen,
};
use ratatui::backend::CrosstermBackend;
use ratatui::layout::{Constraint, Direction, Layout};
use ratatui::style::{Color, Modifier, Style};
use ratatui::text::{Line, Span};
use ratatui::widgets::{
    Axis, Block, Borders, Cell, Chart, Dataset, GraphType, Paragraph, Row, Table,
};
use ratatui::Terminal;
use tokio::sync::watch;

use fluxd_log as logging;

use fluxd_chainstate::metrics::ConnectMetrics;
use fluxd_chainstate::state::ChainState;
use fluxd_chainstate::validation::ValidationMetrics;
use fluxd_consensus::params::Network;

use crate::mempool::Mempool;
use crate::p2p::{NetTotals, PeerKind, PeerRegistry};
use crate::stats::{self, HeaderMetrics, MempoolMetrics, StatsSnapshot, SyncMetrics};
use crate::wallet::Wallet;
use crate::{Backend, Store};

const HISTORY_SAMPLES: usize = 300;
const SAMPLE_INTERVAL: Duration = Duration::from_secs(1);
const UI_TICK: Duration = Duration::from_millis(200);
const LOG_SNAPSHOT_LIMIT: usize = 4096;

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
enum Screen {
    Monitor,
    Peers,
    Db,
    Mempool,
    Wallet,
    Logs,
    Help,
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
    advanced: bool,
    last_snapshot: Option<StatsSnapshot>,
    last_rate_snapshot: Option<StatsSnapshot>,
    last_error: Option<String>,
    blocks_per_sec: Option<f64>,
    headers_per_sec: Option<f64>,
    orphan_count: Option<usize>,
    orphan_bytes: Option<usize>,
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
    bps_history: RateHistory,
    hps_history: RateHistory,
}

impl TuiState {
    fn new() -> Self {
        Self {
            screen: Screen::Monitor,
            help_return: Screen::Monitor,
            advanced: false,
            last_snapshot: None,
            last_rate_snapshot: None,
            last_error: None,
            blocks_per_sec: None,
            headers_per_sec: None,
            orphan_count: None,
            orphan_bytes: None,
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
            bps_history: RateHistory::new(HISTORY_SAMPLES),
            hps_history: RateHistory::new(HISTORY_SAMPLES),
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

    fn cycle_screen(&mut self) {
        self.screen = match self.screen {
            Screen::Monitor => Screen::Peers,
            Screen::Peers => Screen::Db,
            Screen::Db => Screen::Mempool,
            Screen::Mempool => Screen::Wallet,
            Screen::Wallet => Screen::Logs,
            Screen::Logs => Screen::Monitor,
            Screen::Help => self.help_return,
        };
    }

    fn toggle_advanced(&mut self) {
        self.advanced = !self.advanced;
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
    sync_metrics: Arc<SyncMetrics>,
    header_metrics: Arc<HeaderMetrics>,
    validation_metrics: Arc<ValidationMetrics>,
    connect_metrics: Arc<ConnectMetrics>,
    mempool: Arc<Mutex<Mempool>>,
    mempool_metrics: Arc<MempoolMetrics>,
    wallet: Arc<Mutex<Wallet>>,
    net_totals: Arc<NetTotals>,
    peer_registry: Arc<PeerRegistry>,
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
    let mut next_sample = Instant::now();

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
            next_sample = now + SAMPLE_INTERVAL;
        }

        terminal
            .draw(|frame| draw(frame, &state, peer_registry.as_ref(), net_totals.as_ref()))
            .map_err(|err| err.to_string())?;

        if event::poll(UI_TICK).map_err(|err| err.to_string())? {
            if let Event::Key(key) = event::read().map_err(|err| err.to_string())? {
                if key.kind == KeyEventKind::Press {
                    if handle_key(key, &mut state, &shutdown_tx)? {
                        break;
                    }
                }
            }
        }
    }

    terminal.show_cursor().map_err(|err| err.to_string())?;
    Ok(())
}

fn handle_key(
    key: KeyEvent,
    state: &mut TuiState,
    shutdown_tx: &watch::Sender<bool>,
) -> Result<bool, String> {
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
        (KeyCode::Tab, _) => {
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
        (KeyCode::Up, _) => {
            if matches!(state.screen, Screen::Logs) {
                state.logs_follow = false;
                state.logs_scroll = state.logs_scroll.saturating_sub(1);
            }
            Ok(false)
        }
        (KeyCode::Down, _) => {
            if matches!(state.screen, Screen::Logs) {
                state.logs_follow = false;
                state.logs_scroll = state.logs_scroll.saturating_add(1);
            }
            Ok(false)
        }
        (KeyCode::PageUp, _) => {
            if matches!(state.screen, Screen::Logs) {
                state.logs_follow = false;
                state.logs_scroll = state.logs_scroll.saturating_sub(10);
            }
            Ok(false)
        }
        (KeyCode::PageDown, _) => {
            if matches!(state.screen, Screen::Logs) {
                state.logs_follow = false;
                state.logs_scroll = state.logs_scroll.saturating_add(10);
            }
            Ok(false)
        }
        (KeyCode::Home, _) => {
            if matches!(state.screen, Screen::Logs) {
                state.logs_follow = false;
                state.logs_paused = true;
                state.logs_scroll = 0;
            }
            Ok(false)
        }
        (KeyCode::End, _) => {
            if matches!(state.screen, Screen::Logs) {
                state.logs_paused = false;
                state.logs_follow = true;
                state.logs_scroll = 0;
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
    match state.screen {
        Screen::Monitor => draw_monitor(frame, state),
        Screen::Peers => draw_peers(frame, state, peer_registry, net_totals),
        Screen::Db => draw_db(frame, state),
        Screen::Mempool => draw_mempool(frame, state),
        Screen::Wallet => draw_wallet(frame, state),
        Screen::Logs => draw_logs(frame, state),
        Screen::Help => draw_help(frame, state),
    }
}

fn draw_help(frame: &mut ratatui::Frame<'_>, state: &TuiState) {
    let title = Line::from(vec![
        Span::styled("fluxd-rust", Style::default().add_modifier(Modifier::BOLD)),
        Span::raw("  "),
        Span::styled("TUI Help", Style::default().fg(Color::Cyan)),
    ]);
    let lines = vec![
        Line::raw(""),
        Line::raw("Keys:"),
        Line::raw("  q / Esc     Quit (requests daemon shutdown)"),
        Line::raw("  Tab         Cycle views"),
        Line::raw("  1 / m       Monitor view"),
        Line::raw("  2 / p       Peers view"),
        Line::raw("  3 / d       DB view"),
        Line::raw("  4 / t       Mempool view"),
        Line::raw("  5 / w       Wallet view"),
        Line::raw("  6 / l       Logs view"),
        Line::raw("  ? / h       Toggle help"),
        Line::raw("  a           Toggle advanced metrics"),
        Line::raw(""),
        Line::raw("Logs view:"),
        Line::raw("  f           Cycle level filter"),
        Line::raw("  Space       Pause/follow toggle"),
        Line::raw("  c           Clear captured logs"),
        Line::raw("  Up/Down     Scroll"),
        Line::raw("  Home/End    Top/Follow"),
        Line::raw(""),
        Line::raw("Notes:"),
        Line::raw("  - This TUI is in-process; it uses internal stats (no HTTP)."),
        Line::raw("  - For a clean display, run with --log-level warn (default under --tui)."),
    ];
    let paragraph = Paragraph::new(lines)
        .block(Block::default().borders(Borders::ALL).title(title))
        .style(Style::default());
    frame.render_widget(paragraph, frame.area());

    if state.advanced {
        // no-op for now; keeps the state meaningful on the help page.
    }
}

fn draw_monitor(frame: &mut ratatui::Frame<'_>, state: &TuiState) {
    let area = frame.area();
    let chunks = Layout::default()
        .direction(Direction::Vertical)
        .constraints([Constraint::Length(7), Constraint::Min(10)])
        .split(area);

    let header_line = Line::from(vec![
        Span::styled("fluxd-rust", Style::default().add_modifier(Modifier::BOLD)),
        Span::raw("  "),
        Span::styled("Monitor", Style::default().fg(Color::Cyan)),
        Span::raw("  "),
        Span::styled("q", Style::default().fg(Color::Yellow)),
        Span::raw(" quit  "),
        Span::styled("Tab", Style::default().fg(Color::Yellow)),
        Span::raw(" views  "),
        Span::styled("1-6", Style::default().fg(Color::Yellow)),
        Span::raw(" jump  "),
        Span::styled("?", Style::default().fg(Color::Yellow)),
        Span::raw(" help  "),
        Span::styled("a", Style::default().fg(Color::Yellow)),
        Span::raw(" advanced"),
    ]);

    let mut summary = Vec::new();
    summary.push(header_line);

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
            Span::styled("Network:", Style::default().fg(Color::DarkGray)),
            Span::raw(format!(" {}  ", snapshot.network)),
            Span::styled("Backend:", Style::default().fg(Color::DarkGray)),
            Span::raw(format!(" {}  ", snapshot.backend)),
            Span::styled("Uptime:", Style::default().fg(Color::DarkGray)),
            Span::raw(format!(" {}s", snapshot.uptime_secs)),
        ]));

        summary.push(Line::from(vec![
            Span::styled("Tip:", Style::default().fg(Color::DarkGray)),
            Span::raw(format!(
                " headers {}  blocks {}  gap {}  ",
                snapshot.best_header_height, snapshot.best_block_height, snapshot.header_gap
            )),
            Span::styled("Rates:", Style::default().fg(Color::DarkGray)),
            Span::raw(format!(" h/s {hps}  b/s {bps}")),
        ]));

        let mempool_mb = snapshot.mempool_bytes as f64 / (1024.0 * 1024.0);
        let mempool_cap_mb = snapshot.mempool_max_bytes as f64 / (1024.0 * 1024.0);
        summary.push(Line::from(vec![
            Span::styled("Mempool:", Style::default().fg(Color::DarkGray)),
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
                Span::styled("DB:", Style::default().fg(Color::DarkGray)),
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
            Span::styled("Error:", Style::default().fg(Color::Red)),
            Span::raw(" "),
            Span::raw(err),
        ]));
    }

    let summary_widget =
        Paragraph::new(summary).block(Block::default().borders(Borders::ALL).title("Status"));
    frame.render_widget(summary_widget, chunks[0]);

    let (x_min, x_max) = state.bps_history.bounds();
    let y_max = state.bps_history.max_y().max(state.hps_history.max_y());
    let y_max = (y_max * 1.1).ceil().max(1.0);

    let bps_points = state.bps_history.as_vec();
    let hps_points = state.hps_history.as_vec();

    let datasets = vec![
        Dataset::default()
            .name("b/s")
            .graph_type(GraphType::Line)
            .data(&bps_points)
            .style(Style::default().fg(Color::Cyan)),
        Dataset::default()
            .name("h/s")
            .graph_type(GraphType::Line)
            .data(&hps_points)
            .style(Style::default().fg(Color::Yellow)),
    ];

    let chart = Chart::new(datasets)
        .block(
            Block::default()
                .borders(Borders::ALL)
                .title("Throughput (last ~5m)"),
        )
        .x_axis(
            Axis::default()
                .style(Style::default().fg(Color::DarkGray))
                .bounds([x_min, x_max]),
        )
        .y_axis(
            Axis::default()
                .style(Style::default().fg(Color::DarkGray))
                .bounds([0.0, y_max]),
        );

    frame.render_widget(chart, chunks[1]);
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
        .constraints([Constraint::Length(6), Constraint::Min(10)])
        .split(area);

    let totals = net_totals.snapshot();
    let peers = peer_registry.snapshot();
    let mut block_peers = 0usize;
    let mut header_peers = 0usize;
    let mut relay_peers = 0usize;
    for peer in &peers {
        match peer.kind {
            PeerKind::Block => block_peers += 1,
            PeerKind::Header => header_peers += 1,
            PeerKind::Relay => relay_peers += 1,
        }
    }

    let header = Line::from(vec![
        Span::styled("fluxd-rust", Style::default().add_modifier(Modifier::BOLD)),
        Span::raw("  "),
        Span::styled("Peers", Style::default().fg(Color::Cyan)),
        Span::raw("  "),
        Span::styled("Tab", Style::default().fg(Color::Yellow)),
        Span::raw(" views  "),
        Span::styled("1-6", Style::default().fg(Color::Yellow)),
        Span::raw(" jump  "),
        Span::styled("?", Style::default().fg(Color::Yellow)),
        Span::raw(" help  "),
        Span::styled("q", Style::default().fg(Color::Yellow)),
        Span::raw(" quit"),
    ]);

    let recv_mb = totals.bytes_recv as f64 / (1024.0 * 1024.0);
    let sent_mb = totals.bytes_sent as f64 / (1024.0 * 1024.0);
    let mut summary = Vec::new();
    summary.push(header);
    summary.push(Line::from(vec![
        Span::styled("Connections:", Style::default().fg(Color::DarkGray)),
        Span::raw(format!(
            " {}  (block {block_peers}  header {header_peers}  relay {relay_peers})",
            totals.connections
        )),
    ]));
    summary.push(Line::from(vec![
        Span::styled("Net totals:", Style::default().fg(Color::DarkGray)),
        Span::raw(format!(" recv {:.1} MiB  sent {:.1} MiB", recv_mb, sent_mb)),
    ]));
    if let Some(snapshot) = state.last_snapshot.as_ref() {
        summary.push(Line::from(vec![
            Span::styled("Tip:", Style::default().fg(Color::DarkGray)),
            Span::raw(format!(
                " headers {}  blocks {}",
                snapshot.best_header_height, snapshot.best_block_height
            )),
        ]));
    }

    let summary_widget =
        Paragraph::new(summary).block(Block::default().borders(Borders::ALL).title("Network"));
    frame.render_widget(summary_widget, chunks[0]);

    let mut rows = peers;
    rows.sort_by(|a, b| {
        let kind_a = peer_kind_sort_key(a.kind);
        let kind_b = peer_kind_sort_key(b.kind);
        kind_a
            .cmp(&kind_b)
            .then_with(|| a.inbound.cmp(&b.inbound))
            .then_with(|| a.addr.cmp(&b.addr))
    });

    let max_rows = chunks[1].height.saturating_sub(3) as usize;
    rows.truncate(max_rows);

    let header_row = Row::new(vec![
        Cell::from("kind"),
        Cell::from("dir"),
        Cell::from("addr"),
        Cell::from("height"),
        Cell::from("ver"),
        Cell::from("ua"),
    ])
    .style(Style::default().add_modifier(Modifier::BOLD))
    .bottom_margin(1);

    let table_rows = rows.into_iter().map(|peer| {
        let dir = if peer.inbound { "in" } else { "out" };
        let ua = shorten(&peer.user_agent, 32);
        Row::new(vec![
            Cell::from(peer_kind_label(peer.kind)),
            Cell::from(dir),
            Cell::from(peer.addr.to_string()),
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
        .block(Block::default().borders(Borders::ALL).title("Peer list"))
        .column_spacing(1);
    frame.render_widget(table, chunks[1]);
}

fn draw_db(frame: &mut ratatui::Frame<'_>, state: &TuiState) {
    let area = frame.area();
    let chunks = Layout::default()
        .direction(Direction::Vertical)
        .constraints([Constraint::Length(7), Constraint::Min(10)])
        .split(area);

    let header = Line::from(vec![
        Span::styled("fluxd-rust", Style::default().add_modifier(Modifier::BOLD)),
        Span::raw("  "),
        Span::styled("DB", Style::default().fg(Color::Cyan)),
        Span::raw("  "),
        Span::styled("Tab", Style::default().fg(Color::Yellow)),
        Span::raw(" views  "),
        Span::styled("1-6", Style::default().fg(Color::Yellow)),
        Span::raw(" jump  "),
        Span::styled("?", Style::default().fg(Color::Yellow)),
        Span::raw(" help  "),
        Span::styled("q", Style::default().fg(Color::Yellow)),
        Span::raw(" quit"),
    ]);

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
                Span::styled("Write buffer:", Style::default().fg(Color::DarkGray)),
                Span::raw(format!(" {writebuf}/{writebuf_max}")),
            ]));
            summary.push(Line::from(vec![
                Span::styled("Journal:", Style::default().fg(Color::DarkGray)),
                Span::raw(format!(" {journals}  {journal_bytes}/{journal_max}")),
            ]));
            summary.push(Line::from(vec![
                Span::styled("Flushes:", Style::default().fg(Color::DarkGray)),
                Span::raw(format!(" {flushes}")),
                Span::raw("  "),
                Span::styled("Compactions:", Style::default().fg(Color::DarkGray)),
                Span::raw(format!(
                    " active {compactions_active}  done {compactions_done}  time {compact_s}"
                )),
            ]));
        }
        None => summary.push(Line::raw("Waiting for stats...")),
    }

    if let Some(err) = state.last_error.as_ref() {
        summary.push(Line::from(vec![
            Span::styled("Error:", Style::default().fg(Color::Red)),
            Span::raw(" "),
            Span::raw(err),
        ]));
    }

    let summary_widget =
        Paragraph::new(summary).block(Block::default().borders(Borders::ALL).title("Fjall status"));
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
    .style(Style::default().add_modifier(Modifier::BOLD))
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
        .block(Block::default().borders(Borders::ALL).title("Partitions"))
        .column_spacing(1);
    frame.render_widget(table, chunks[1]);
}

fn draw_mempool(frame: &mut ratatui::Frame<'_>, state: &TuiState) {
    let header = Line::from(vec![
        Span::styled("fluxd-rust", Style::default().add_modifier(Modifier::BOLD)),
        Span::raw("  "),
        Span::styled("Mempool", Style::default().fg(Color::Cyan)),
        Span::raw("  "),
        Span::styled("Tab", Style::default().fg(Color::Yellow)),
        Span::raw(" views  "),
        Span::styled("1-6", Style::default().fg(Color::Yellow)),
        Span::raw(" jump  "),
        Span::styled("?", Style::default().fg(Color::Yellow)),
        Span::raw(" help  "),
        Span::styled("q", Style::default().fg(Color::Yellow)),
        Span::raw(" quit"),
    ]);

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
                Span::styled("Mempool:", Style::default().fg(Color::DarkGray)),
                Span::raw(format!(
                    " {} tx  {:.1}/{:.0} MiB",
                    snapshot.mempool_size, mempool_mb, mempool_cap_mb
                )),
                Span::raw("  "),
                Span::styled("Orphans:", Style::default().fg(Color::DarkGray)),
                Span::raw(format!(" {orphan_count} tx  {orphan_mb} MiB")),
            ]));

            lines.push(Line::from(vec![
                Span::styled("RPC:", Style::default().fg(Color::DarkGray)),
                Span::raw(format!(
                    " accept {}  reject {}",
                    snapshot.mempool_rpc_accept, snapshot.mempool_rpc_reject
                )),
                Span::raw("  "),
                Span::styled("Relay:", Style::default().fg(Color::DarkGray)),
                Span::raw(format!(
                    " accept {}  reject {}",
                    snapshot.mempool_relay_accept, snapshot.mempool_relay_reject
                )),
            ]));

            if state.advanced {
                let evicted_mb = snapshot.mempool_evicted_bytes as f64 / (1024.0 * 1024.0);
                let persisted_mb = snapshot.mempool_persisted_bytes as f64 / (1024.0 * 1024.0);
                lines.push(Line::from(vec![
                    Span::styled("Evicted:", Style::default().fg(Color::DarkGray)),
                    Span::raw(format!(
                        " {} ({:.2} MiB)",
                        snapshot.mempool_evicted, evicted_mb
                    )),
                    Span::raw("  "),
                    Span::styled("Loaded:", Style::default().fg(Color::DarkGray)),
                    Span::raw(format!(
                        " {}  (reject {})",
                        snapshot.mempool_loaded, snapshot.mempool_load_reject
                    )),
                ]));
                lines.push(Line::from(vec![
                    Span::styled("Persist:", Style::default().fg(Color::DarkGray)),
                    Span::raw(format!(
                        " writes {}  {:.2} MiB",
                        snapshot.mempool_persisted_writes, persisted_mb
                    )),
                ]));
            }
        }
        None => lines.push(Line::raw("Waiting for stats...")),
    }

    if let Some(err) = state.last_error.as_ref() {
        lines.push(Line::from(vec![
            Span::styled("Error:", Style::default().fg(Color::Red)),
            Span::raw(" "),
            Span::raw(err),
        ]));
    }

    let widget = Paragraph::new(lines).block(Block::default().borders(Borders::ALL).title("Pool"));
    frame.render_widget(widget, frame.area());
}

fn draw_wallet(frame: &mut ratatui::Frame<'_>, state: &TuiState) {
    let header = Line::from(vec![
        Span::styled("fluxd-rust", Style::default().add_modifier(Modifier::BOLD)),
        Span::raw("  "),
        Span::styled("Wallet", Style::default().fg(Color::Cyan)),
        Span::raw("  "),
        Span::styled("Tab", Style::default().fg(Color::Yellow)),
        Span::raw(" views  "),
        Span::styled("1-6", Style::default().fg(Color::Yellow)),
        Span::raw(" jump  "),
        Span::styled("?", Style::default().fg(Color::Yellow)),
        Span::raw(" help  "),
        Span::styled("q", Style::default().fg(Color::Yellow)),
        Span::raw(" quit"),
    ]);

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
        Span::styled("Encrypted:", Style::default().fg(Color::DarkGray)),
        Span::raw(format!(" {encrypted_label}  ")),
        Span::styled("Status:", Style::default().fg(Color::DarkGray)),
        Span::raw(format!(" {status}  ")),
        Span::styled("Unlocked for:", Style::default().fg(Color::DarkGray)),
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
        Span::styled("Keys:", Style::default().fg(Color::DarkGray)),
        Span::raw(format!(" {key_count}  ")),
        Span::styled("Keypool:", Style::default().fg(Color::DarkGray)),
        Span::raw(format!(" {keypool}  ")),
        Span::styled("Sapling:", Style::default().fg(Color::DarkGray)),
        Span::raw(format!(" {sapling}  ")),
        Span::styled("Wallet txs:", Style::default().fg(Color::DarkGray)),
        Span::raw(format!(" {tx_count}")),
    ]));
    lines.push(Line::from(vec![
        Span::styled("paytxfee:", Style::default().fg(Color::DarkGray)),
        Span::raw(format!(" {paytxfee}")),
    ]));

    if let Some(snapshot) = state.last_snapshot.as_ref() {
        lines.push(Line::from(vec![
            Span::styled("Tip:", Style::default().fg(Color::DarkGray)),
            Span::raw(format!(
                " headers {}  blocks {}",
                snapshot.best_header_height, snapshot.best_block_height
            )),
        ]));
    }

    if let Some(err) = state.last_error.as_ref() {
        lines.push(Line::from(vec![
            Span::styled("Error:", Style::default().fg(Color::Red)),
            Span::raw(" "),
            Span::raw(err),
        ]));
    }

    let widget = Paragraph::new(lines).block(Block::default().borders(Borders::ALL).title("Info"));
    frame.render_widget(widget, frame.area());
}

fn draw_logs(frame: &mut ratatui::Frame<'_>, state: &TuiState) {
    let area = frame.area();
    let chunks = Layout::default()
        .direction(Direction::Vertical)
        .constraints([Constraint::Length(6), Constraint::Min(10)])
        .split(area);

    let header = Line::from(vec![
        Span::styled("fluxd-rust", Style::default().add_modifier(Modifier::BOLD)),
        Span::raw("  "),
        Span::styled("Logs", Style::default().fg(Color::Cyan)),
        Span::raw("  "),
        Span::styled("Tab", Style::default().fg(Color::Yellow)),
        Span::raw(" views  "),
        Span::styled("1-6", Style::default().fg(Color::Yellow)),
        Span::raw(" jump  "),
        Span::styled("q", Style::default().fg(Color::Yellow)),
        Span::raw(" quit"),
    ]);

    let paused = if state.logs_paused { "yes" } else { "no" };
    let follow = if state.logs_follow { "yes" } else { "no" };
    let min_level = state.logs_min_level.as_str();
    let total = state.logs.len();

    let mut summary = Vec::new();
    summary.push(header);
    summary.push(Line::from(vec![
        Span::styled("Filter:", Style::default().fg(Color::DarkGray)),
        Span::raw(format!(" <= {min_level}  ")),
        Span::styled("Paused:", Style::default().fg(Color::DarkGray)),
        Span::raw(format!(" {paused}  ")),
        Span::styled("Follow:", Style::default().fg(Color::DarkGray)),
        Span::raw(format!(" {follow}  ")),
        Span::styled("Lines:", Style::default().fg(Color::DarkGray)),
        Span::raw(format!(" {total}")),
    ]));
    summary.push(Line::from(vec![
        Span::styled("Keys:", Style::default().fg(Color::DarkGray)),
        Span::raw(" f filter  Space pause/follow  c clear  Up/Down scroll  End follow"),
    ]));

    if let Some(err) = state.last_error.as_ref() {
        summary.push(Line::from(vec![
            Span::styled("Error:", Style::default().fg(Color::Red)),
            Span::raw(" "),
            Span::raw(err),
        ]));
    }

    let summary_widget =
        Paragraph::new(summary).block(Block::default().borders(Borders::ALL).title("Log capture"));
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
                Span::styled(ts, Style::default().fg(Color::DarkGray)),
                Span::raw(" "),
                Span::styled(level, level_style),
                Span::raw(" "),
                Span::styled(target, Style::default().fg(Color::LightBlue)),
                Span::raw(" "),
                Span::styled(location, Style::default().fg(Color::DarkGray)),
                Span::raw(" "),
                Span::raw(msg),
            ]));
        } else {
            lines.push(Line::from(vec![
                Span::styled(ts, Style::default().fg(Color::DarkGray)),
                Span::raw(" "),
                Span::styled(level, level_style),
                Span::raw(" "),
                Span::styled(target, Style::default().fg(Color::LightBlue)),
                Span::raw(" "),
                Span::raw(msg),
            ]));
        }
    }
    if lines.is_empty() {
        lines.push(Line::raw("No captured logs."));
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
        .block(Block::default().borders(Borders::ALL).title("Log lines"));
    frame.render_widget(widget, chunks[1]);
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
        logging::Level::Error => Style::default().fg(Color::Red).add_modifier(Modifier::BOLD),
        logging::Level::Warn => Style::default()
            .fg(Color::Yellow)
            .add_modifier(Modifier::BOLD),
        logging::Level::Info => Style::default().fg(Color::White),
        logging::Level::Debug => Style::default().fg(Color::Cyan),
        logging::Level::Trace => Style::default().fg(Color::DarkGray),
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
