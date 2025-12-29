use std::collections::VecDeque;
use std::fs;
use std::path::Path;

use fluxd_primitives::encoding::{Decoder, Encoder};

const FEE_ESTIMATES_FILE_VERSION: u32 = 1;
const MIN_SAMPLES_FOR_ESTIMATE: usize = 32;

#[derive(Debug)]
pub struct FeeEstimator {
    samples: VecDeque<i64>,
    max_samples: usize,
    revision: u64,
}

impl FeeEstimator {
    pub fn new(max_samples: usize) -> Self {
        Self {
            samples: VecDeque::new(),
            max_samples: max_samples.max(1),
            revision: 0,
        }
    }

    pub fn revision(&self) -> u64 {
        self.revision
    }

    pub fn observe_tx(&mut self, fee: i64, size: usize) {
        if fee <= 0 {
            return;
        }
        let size = i64::try_from(size.max(1)).unwrap_or(i64::MAX);
        let feerate = fee.saturating_mul(1000).saturating_div(size);
        self.observe_fee_rate_per_kb(feerate);
    }

    pub fn observe_fee_rate_per_kb(&mut self, fee_rate_per_kb: i64) {
        let fee_rate_per_kb = fee_rate_per_kb.max(0);
        if fee_rate_per_kb == 0 {
            return;
        }
        self.samples.push_back(fee_rate_per_kb);
        while self.samples.len() > self.max_samples {
            self.samples.pop_front();
        }
        self.revision = self.revision.saturating_add(1);
    }

    pub fn estimate_fee_per_kb(&self, target_blocks: u32) -> Option<i64> {
        if self.samples.len() < MIN_SAMPLES_FOR_ESTIMATE {
            return None;
        }
        let percentile = percentile_for_target(target_blocks);
        let mut values: Vec<i64> = self.samples.iter().copied().collect();
        values.sort_unstable();
        if values.is_empty() {
            return None;
        }
        let last_index = values.len() - 1;
        let index = ((last_index as f64) * percentile).round() as usize;
        Some(values[index.min(last_index)])
    }

    pub fn snapshot_rates(&self) -> Vec<i64> {
        self.samples.iter().copied().collect()
    }

    pub fn load(path: &Path, max_samples: usize) -> Result<Self, String> {
        let bytes = match fs::read(path) {
            Ok(bytes) => bytes,
            Err(err) if err.kind() == std::io::ErrorKind::NotFound => {
                return Ok(Self::new(max_samples));
            }
            Err(err) => return Err(err.to_string()),
        };

        let mut decoder = Decoder::new(&bytes);
        let version = decoder
            .read_u32_le()
            .map_err(|err| format!("invalid fee estimates file: {err}"))?;
        if version != FEE_ESTIMATES_FILE_VERSION {
            return Err(format!(
                "unsupported fee estimates file version {version} (expected {FEE_ESTIMATES_FILE_VERSION})"
            ));
        }

        let count = decoder
            .read_varint()
            .map_err(|err| format!("invalid fee estimates file: {err}"))?;
        let count =
            usize::try_from(count).map_err(|_| "fee estimates file count too large".to_string())?;

        let mut samples = VecDeque::with_capacity(count.min(max_samples));
        for _ in 0..count {
            let raw = decoder
                .read_u64_le()
                .map_err(|err| format!("invalid fee estimates file: {err}"))?;
            let value = i64::try_from(raw).unwrap_or(i64::MAX);
            if value > 0 {
                samples.push_back(value);
            }
        }
        if !decoder.is_empty() {
            return Err("invalid fee estimates file: trailing bytes".to_string());
        }

        while samples.len() > max_samples {
            samples.pop_front();
        }

        Ok(Self {
            samples,
            max_samples: max_samples.max(1),
            revision: 0,
        })
    }

    pub fn save(&self, path: &Path) -> Result<usize, String> {
        let rates = self.snapshot_rates();
        let mut encoder = Encoder::new();
        encoder.write_u32_le(FEE_ESTIMATES_FILE_VERSION);
        encoder.write_varint(rates.len() as u64);
        for rate in rates {
            encoder.write_u64_le(rate as u64);
        }
        let bytes = encoder.into_inner();
        let len = bytes.len();
        crate::write_file_atomic(path, &bytes)?;
        Ok(len)
    }
}

fn percentile_for_target(target_blocks: u32) -> f64 {
    match target_blocks {
        0 | 1 => 0.90,
        2 => 0.75,
        3..=6 => 0.50,
        7..=12 => 0.25,
        _ => 0.10,
    }
}
