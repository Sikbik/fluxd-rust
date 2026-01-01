use std::fmt;
use std::io::{self, Write};
use std::sync::atomic::{AtomicBool, AtomicU8, Ordering};
use std::time::{SystemTime, UNIX_EPOCH};

use serde_json::json;

#[derive(Clone, Copy, Debug, Eq, PartialEq, Ord, PartialOrd)]
pub enum Level {
    Error = 1,
    Warn = 2,
    Info = 3,
    Debug = 4,
    Trace = 5,
}

impl Level {
    pub fn as_str(self) -> &'static str {
        match self {
            Self::Error => "ERROR",
            Self::Warn => "WARN",
            Self::Info => "INFO",
            Self::Debug => "DEBUG",
            Self::Trace => "TRACE",
        }
    }

    pub fn parse(raw: &str) -> Option<Self> {
        match raw.trim().to_ascii_lowercase().as_str() {
            "error" => Some(Self::Error),
            "warn" | "warning" => Some(Self::Warn),
            "info" => Some(Self::Info),
            "debug" => Some(Self::Debug),
            "trace" => Some(Self::Trace),
            _ => None,
        }
    }
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum Format {
    Text = 0,
    Json = 1,
}

impl Format {
    pub fn parse(raw: &str) -> Option<Self> {
        match raw.trim().to_ascii_lowercase().as_str() {
            "text" => Some(Self::Text),
            "json" => Some(Self::Json),
            _ => None,
        }
    }
}

#[derive(Clone, Copy, Debug)]
pub struct LogConfig {
    pub level: Level,
    pub format: Format,
    pub timestamps: bool,
}

static LOG_LEVEL: AtomicU8 = AtomicU8::new(Level::Info as u8);
static LOG_FORMAT: AtomicU8 = AtomicU8::new(Format::Text as u8);
static LOG_TIMESTAMPS: AtomicBool = AtomicBool::new(true);

pub fn init(config: LogConfig) {
    LOG_LEVEL.store(config.level as u8, Ordering::Relaxed);
    LOG_FORMAT.store(config.format as u8, Ordering::Relaxed);
    LOG_TIMESTAMPS.store(config.timestamps, Ordering::Relaxed);
}

pub fn enabled(level: Level) -> bool {
    level as u8 <= LOG_LEVEL.load(Ordering::Relaxed)
}

pub fn log(
    level: Level,
    target: &'static str,
    file: &'static str,
    line: u32,
    args: fmt::Arguments<'_>,
) {
    if !enabled(level) {
        return;
    }

    let format = match LOG_FORMAT.load(Ordering::Relaxed) {
        0 => Format::Text,
        1 => Format::Json,
        _ => Format::Text,
    };
    let timestamps = LOG_TIMESTAMPS.load(Ordering::Relaxed);
    let now = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .unwrap_or_default();
    let ts_ms: u64 = now.as_millis().try_into().unwrap_or(u64::MAX);

    let mut out = io::stderr().lock();
    match format {
        Format::Text => {
            if timestamps {
                let ts = Timestamp {
                    unix_seconds: now.as_secs(),
                    millis: now.subsec_millis(),
                };
                let _ = write!(out, "{ts} ");
            }
            let _ = write!(out, "{} {}: ", level.as_str(), target);
            let _ = writeln!(out, "{args}");
        }
        Format::Json => {
            let msg = args.to_string();
            let line = json!({
                "ts_ms": ts_ms,
                "level": level.as_str(),
                "target": target,
                "file": file,
                "line": line,
                "msg": msg,
            });
            let _ = writeln!(out, "{line}");
        }
    }
}

#[macro_export]
macro_rules! log_at {
    ($level:expr, $($arg:tt)*) => {{
        if $crate::enabled($level) {
            $crate::log($level, module_path!(), file!(), line!(), format_args!($($arg)*));
        }
    }};
}

#[macro_export]
macro_rules! log_error {
    ($($arg:tt)*) => {{
        $crate::log_at!($crate::Level::Error, $($arg)*);
    }};
}

#[macro_export]
macro_rules! log_warn {
    ($($arg:tt)*) => {{
        $crate::log_at!($crate::Level::Warn, $($arg)*);
    }};
}

#[macro_export]
macro_rules! log_info {
    ($($arg:tt)*) => {{
        $crate::log_at!($crate::Level::Info, $($arg)*);
    }};
}

#[macro_export]
macro_rules! log_debug {
    ($($arg:tt)*) => {{
        $crate::log_at!($crate::Level::Debug, $($arg)*);
    }};
}

#[macro_export]
macro_rules! log_trace {
    ($($arg:tt)*) => {{
        $crate::log_at!($crate::Level::Trace, $($arg)*);
    }};
}

struct Timestamp {
    unix_seconds: u64,
    millis: u32,
}

impl fmt::Display for Timestamp {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        const SECS_PER_DAY: u64 = 86_400;
        let days = (self.unix_seconds / SECS_PER_DAY) as i64;
        let secs_of_day = self.unix_seconds % SECS_PER_DAY;
        let hour = secs_of_day / 3600;
        let minute = (secs_of_day % 3600) / 60;
        let second = secs_of_day % 60;
        let (year, month, day) = civil_from_days(days);
        write!(
            f,
            "{year:04}-{month:02}-{day:02}T{hour:02}:{minute:02}:{second:02}.{millis:03}Z",
            millis = self.millis
        )
    }
}

fn civil_from_days(days_since_unix_epoch: i64) -> (i32, u32, u32) {
    // From Howard Hinnant's "civil_from_days" algorithm (public domain).
    let z = days_since_unix_epoch + 719_468;
    let era = if z >= 0 { z } else { z - 146_096 } / 146_097;
    let doe = (z - era * 146_097) as u32;
    let yoe = (doe - doe / 1460 + doe / 36_524 - doe / 146_096) / 365;
    let y = (yoe as i32) + (era as i32) * 400;
    let doy = doe - (365 * yoe + yoe / 4 - yoe / 100);
    let mp = (5 * doy + 2) / 153;
    let d = doy - (153 * mp + 2) / 5 + 1;
    let m = if mp < 10 { mp + 3 } else { mp - 9 };
    let year = y + if m <= 2 { 1 } else { 0 };
    (year, m, d)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parse_level() {
        assert_eq!(Level::parse("info"), Some(Level::Info));
        assert_eq!(Level::parse("WARN"), Some(Level::Warn));
        assert_eq!(Level::parse("warning"), Some(Level::Warn));
        assert_eq!(Level::parse("debug"), Some(Level::Debug));
        assert_eq!(Level::parse("nope"), None);
    }

    #[test]
    fn parse_format() {
        assert_eq!(Format::parse("text"), Some(Format::Text));
        assert_eq!(Format::parse("JSON"), Some(Format::Json));
        assert_eq!(Format::parse("nope"), None);
    }
}
