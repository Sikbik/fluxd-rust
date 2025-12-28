use std::collections::HashMap;
use std::fs;
use std::net::SocketAddr;
use std::path::Path;
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::Mutex;
use std::time::{Duration, SystemTime, UNIX_EPOCH};

use serde::{Deserialize, Serialize};

#[derive(Clone, Debug)]
pub struct BannedPeerInfo {
    pub addr: SocketAddr,
    pub banned_until: SystemTime,
}

#[derive(Debug, Deserialize, Serialize)]
struct BanListFile {
    version: u32,
    banned: Vec<BanListEntry>,
}

#[derive(Debug, Deserialize, Serialize)]
struct BanListEntry {
    addr: SocketAddr,
    banned_until: u64,
}

const BANLIST_VERSION: u32 = 1;

#[derive(Default)]
pub struct HeaderPeerBook {
    scores: Mutex<HashMap<SocketAddr, i32>>,
    banned: Mutex<HashMap<SocketAddr, SystemTime>>,
    revision: AtomicU64,
}

impl HeaderPeerBook {
    pub fn record_success(&self, addr: SocketAddr) {
        if let Ok(mut scores) = self.scores.lock() {
            let entry = scores.entry(addr).or_insert(0);
            *entry = entry.saturating_add(3);
        }
    }

    pub fn record_failure(&self, addr: SocketAddr) {
        if let Ok(mut scores) = self.scores.lock() {
            let entry = scores.entry(addr).or_insert(0);
            *entry = entry.saturating_sub(1);
        }
    }

    pub fn record_bad_chain(&self, addr: SocketAddr, ban_secs: u64) {
        self.record_failure(addr);
        self.ban_for(addr, ban_secs);
    }

    pub fn is_banned(&self, addr: SocketAddr) -> bool {
        let now = SystemTime::now();
        let Ok(mut banned) = self.banned.lock() else {
            return false;
        };
        if let Some(until) = banned.get(&addr).copied() {
            if until > now {
                return true;
            }
            banned.remove(&addr);
            self.revision.fetch_add(1, Ordering::Relaxed);
        }
        false
    }

    pub fn ban_for(&self, addr: SocketAddr, secs: u64) {
        if let Ok(mut banned) = self.banned.lock() {
            banned.insert(addr, SystemTime::now() + Duration::from_secs(secs));
            self.revision.fetch_add(1, Ordering::Relaxed);
        }
    }

    pub fn preferred(&self, limit: usize) -> Vec<SocketAddr> {
        if limit == 0 {
            return Vec::new();
        }
        let scores = match self.scores.lock() {
            Ok(scores) => scores,
            Err(_) => return Vec::new(),
        };
        let mut entries: Vec<(SocketAddr, i32)> = scores
            .iter()
            .filter(|(addr, score)| **score > 0 && !self.is_banned(**addr))
            .map(|(addr, score)| (*addr, *score))
            .collect();
        entries.sort_by(|a, b| b.1.cmp(&a.1));
        entries.truncate(limit);
        entries.into_iter().map(|(addr, _)| addr).collect()
    }

    pub fn banned_peers(&self) -> Vec<BannedPeerInfo> {
        let now_system = SystemTime::now();
        let mut out = Vec::new();
        let Ok(mut banned) = self.banned.lock() else {
            return out;
        };
        let mut expired = Vec::new();
        for (addr, until) in banned.iter() {
            if *until <= now_system {
                expired.push(*addr);
                continue;
            }
            out.push(BannedPeerInfo {
                addr: *addr,
                banned_until: *until,
            });
        }
        for addr in expired {
            banned.remove(&addr);
            self.revision.fetch_add(1, Ordering::Relaxed);
        }
        out
    }

    pub fn banlist_revision(&self) -> u64 {
        self.revision.load(Ordering::Relaxed)
    }

    pub fn load_banlist(&self, path: &Path) -> Result<usize, String> {
        let bytes = match fs::read(path) {
            Ok(bytes) => bytes,
            Err(err) if err.kind() == std::io::ErrorKind::NotFound => return Ok(0),
            Err(err) => return Err(err.to_string()),
        };
        let file: BanListFile =
            serde_json::from_slice(&bytes).map_err(|err| format!("invalid banlist: {err}"))?;
        if file.version != BANLIST_VERSION {
            return Err(format!(
                "unsupported banlist version {} (expected {})",
                file.version, BANLIST_VERSION
            ));
        }
        let now = SystemTime::now();
        let mut inserted = 0usize;
        if let Ok(mut banned) = self.banned.lock() {
            for entry in file.banned {
                let until = UNIX_EPOCH + Duration::from_secs(entry.banned_until);
                if until <= now {
                    continue;
                }
                banned.insert(entry.addr, until);
                inserted += 1;
            }
        }
        Ok(inserted)
    }

    pub fn save_banlist(&self, path: &Path) -> Result<(), String> {
        let now = SystemTime::now();
        let mut entries = Vec::new();
        if let Ok(mut banned) = self.banned.lock() {
            let mut expired = Vec::new();
            for (addr, until) in banned.iter() {
                if *until <= now {
                    expired.push(*addr);
                    continue;
                }
                let secs = until
                    .duration_since(UNIX_EPOCH)
                    .unwrap_or_default()
                    .as_secs();
                entries.push(BanListEntry {
                    addr: *addr,
                    banned_until: secs,
                });
            }
            for addr in expired {
                banned.remove(&addr);
            }
        }
        entries.sort_by_key(|entry| entry.addr.to_string());
        let file = BanListFile {
            version: BANLIST_VERSION,
            banned: entries,
        };
        let json = serde_json::to_vec_pretty(&file).map_err(|err| err.to_string())?;
        if let Some(parent) = path.parent() {
            fs::create_dir_all(parent).map_err(|err| err.to_string())?;
        }
        let tmp = path.with_extension("tmp");
        fs::write(&tmp, json).map_err(|err| err.to_string())?;
        if fs::rename(&tmp, path).is_err() {
            let _ = fs::remove_file(path);
            fs::rename(&tmp, path).map_err(|err| err.to_string())?;
        }
        Ok(())
    }
}
