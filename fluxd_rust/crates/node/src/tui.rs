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
use ratatui::widgets::{Axis, Block, Borders, Chart, Dataset, GraphType, Paragraph};
use ratatui::Terminal;
use tokio::sync::watch;

use fluxd_chainstate::metrics::ConnectMetrics;
use fluxd_chainstate::state::ChainState;
use fluxd_chainstate::validation::ValidationMetrics;
use fluxd_consensus::params::Network;

use crate::mempool::Mempool;
use crate::stats::{self, HeaderMetrics, MempoolMetrics, StatsSnapshot, SyncMetrics};
use crate::{Backend, Store};

const HISTORY_SAMPLES: usize = 300;
const SAMPLE_INTERVAL: Duration = Duration::from_secs(1);
const UI_TICK: Duration = Duration::from_millis(200);

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
enum Screen {
    Monitor,
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
    advanced: bool,
    last_snapshot: Option<StatsSnapshot>,
    last_rate_snapshot: Option<StatsSnapshot>,
    last_error: Option<String>,
    blocks_per_sec: Option<f64>,
    headers_per_sec: Option<f64>,
    bps_history: RateHistory,
    hps_history: RateHistory,
}

impl TuiState {
    fn new() -> Self {
        Self {
            screen: Screen::Monitor,
            advanced: false,
            last_snapshot: None,
            last_rate_snapshot: None,
            last_error: None,
            blocks_per_sec: None,
            headers_per_sec: None,
            bps_history: RateHistory::new(HISTORY_SAMPLES),
            hps_history: RateHistory::new(HISTORY_SAMPLES),
        }
    }

    fn toggle_help(&mut self) {
        self.screen = match self.screen {
            Screen::Monitor => Screen::Help,
            Screen::Help => Screen::Monitor,
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
                Ok(snapshot) => state.update_snapshot(snapshot),
                Err(err) => state.update_error(err),
            }
            next_sample = now + SAMPLE_INTERVAL;
        }

        terminal
            .draw(|frame| draw(frame, &state))
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
            state.toggle_help();
            Ok(false)
        }
        (KeyCode::Char('a'), _) => {
            state.toggle_advanced();
            Ok(false)
        }
        _ => Ok(false),
    }
}

fn draw(frame: &mut ratatui::Frame<'_>, state: &TuiState) {
    match state.screen {
        Screen::Monitor => draw_monitor(frame, state),
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
        Line::raw("  Tab / ?     Toggle help"),
        Line::raw("  a           Toggle advanced metrics"),
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
