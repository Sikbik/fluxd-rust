use std::collections::HashMap;
use std::net::SocketAddr;
use std::sync::Mutex;
use std::time::{Duration, Instant, SystemTime};

#[derive(Clone, Debug)]
pub struct BannedPeerInfo {
    pub addr: SocketAddr,
    pub banned_until: SystemTime,
}

#[derive(Default)]
pub struct HeaderPeerBook {
    scores: Mutex<HashMap<SocketAddr, i32>>,
    banned: Mutex<HashMap<SocketAddr, Instant>>,
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
        let now = Instant::now();
        let Ok(mut banned) = self.banned.lock() else {
            return false;
        };
        if let Some(until) = banned.get(&addr).copied() {
            if until > now {
                return true;
            }
            banned.remove(&addr);
        }
        false
    }

    pub fn ban_for(&self, addr: SocketAddr, secs: u64) {
        if let Ok(mut banned) = self.banned.lock() {
            banned.insert(addr, Instant::now() + Duration::from_secs(secs));
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
        let now_instant = Instant::now();
        let now_system = SystemTime::now();
        let mut out = Vec::new();
        let Ok(mut banned) = self.banned.lock() else {
            return out;
        };
        let mut expired = Vec::new();
        for (addr, until) in banned.iter() {
            if *until <= now_instant {
                expired.push(*addr);
                continue;
            }
            let remaining = *until - now_instant;
            let banned_until = now_system + remaining;
            out.push(BannedPeerInfo {
                addr: *addr,
                banned_until,
            });
        }
        for addr in expired {
            banned.remove(&addr);
        }
        out
    }
}
