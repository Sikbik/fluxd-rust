use std::collections::BTreeMap;
use std::fs;
use std::path::{Path, PathBuf};
use std::sync::{Mutex, OnceLock};
use std::time::{Duration, Instant};

use serde_json::{json, Value};

use fluxd_chainstate::filemeta::{
    parse_block_file_info_key, parse_undo_file_info_key, FlatFileInfo,
    META_BLOCK_FILES_LAST_FILE_KEY, META_BLOCK_FILES_LAST_LEN_KEY, META_UNDO_FILES_LAST_FILE_KEY,
    META_UNDO_FILES_LAST_LEN_KEY,
};
use fluxd_chainstate::state::ChainState;
use fluxd_storage::{Column, KeyValueStore};

use crate::{Backend, Store};

#[derive(Clone, Debug, Default)]
struct FlatfileMetaSummary {
    file_count: u64,
    total_bytes: u64,
    last_file_id: Option<u32>,
    last_file_len: Option<u64>,
}

#[derive(Clone, Debug, Default)]
struct FlatfileFsSummary {
    data_files: u64,
    data_bytes: u64,
    data_max_file_id: Option<u32>,
    data_max_file_len: Option<u64>,
    undo_files: u64,
    undo_bytes: u64,
    undo_max_file_id: Option<u32>,
    undo_max_file_len: Option<u64>,
    other_files: u64,
    other_bytes: u64,
}

pub(crate) fn collect_db_info<S: KeyValueStore>(
    chainstate: &ChainState<S>,
    store: &Store,
    data_dir: &Path,
    backend: Backend,
    compute_stats: bool,
    compute_key_counts: bool,
) -> Result<Value, String> {
    let best_header = chainstate.best_header().map_err(|err| err.to_string())?;
    let best_block = chainstate.best_block().map_err(|err| err.to_string())?;
    let db_schema_version = match store.get(Column::Meta, crate::DB_SCHEMA_VERSION_KEY) {
        Ok(Some(bytes)) => bytes.as_slice().try_into().ok().map(u32::from_le_bytes),
        Ok(None) => None,
        Err(err) => return Err(err.to_string()),
    };

    let db_dir = data_dir.join("db");
    let blocks_dir = data_dir.join("blocks");

    let peers_path = data_dir.join(crate::PEERS_FILE_NAME);
    let banlist_path = data_dir.join(crate::BANLIST_FILE_NAME);
    let mempool_path = data_dir.join(crate::MEMPOOL_FILE_NAME);
    let fee_estimates_path = data_dir.join(crate::FEE_ESTIMATES_FILE_NAME);
    let cookie_path = data_dir.join(crate::rpc::RPC_COOKIE_FILE);
    let reindex_flag_path = data_dir.join(crate::REINDEX_REQUEST_FILE_NAME);

    let block_meta = scan_flatfile_meta_blocks(store)?;
    let undo_meta = scan_flatfile_meta_undo(store)?;
    let blocks_fs = scan_blocks_dir_fs(&blocks_dir)?;

    let db_dir_size = dir_size_cached(&db_dir, Duration::from_secs(30))?;
    let blocks_dir_size = dir_size_cached(&blocks_dir, Duration::from_secs(30))?;

    let mut partitions = Vec::new();
    let partitions_dir = db_dir.join("partitions");
    let mut partitions_total_bytes = 0u64;
    for column in Column::ALL {
        let path = partitions_dir.join(column.as_str());
        let size_bytes = dir_size_cached(&path, Duration::from_secs(30))?;
        partitions_total_bytes = partitions_total_bytes.saturating_add(size_bytes);
        let mut entry = json!({
            "column": column.as_str(),
            "size_bytes": size_bytes,
        });
        if compute_key_counts {
            let stats = column_key_stats(store, column)?;
            if let Some(obj) = entry.as_object_mut() {
                obj.insert("key_count".to_string(), json!(stats.key_count));
                obj.insert("key_bytes".to_string(), json!(stats.key_bytes));
                obj.insert("value_bytes".to_string(), json!(stats.value_bytes));
            }
        }
        partitions.push(entry);
    }
    let journals_size_bytes = dir_size_cached(&db_dir.join("journals"), Duration::from_secs(30))?;

    let utxo_stats = if compute_stats {
        Some(
            chainstate
                .utxo_stats_or_compute()
                .map_err(|err| err.to_string())?,
        )
    } else {
        chainstate.utxo_stats().map_err(|err| err.to_string())?
    };
    let value_pools = if compute_stats {
        Some(
            chainstate
                .value_pools_or_compute()
                .map_err(|err| err.to_string())?,
        )
    } else {
        chainstate.value_pools().map_err(|err| err.to_string())?
    };
    let supply = if let (Some(utxo_stats), Some(value_pools)) = (&utxo_stats, &value_pools) {
        let shielded_total = value_pools
            .sprout
            .checked_add(value_pools.sapling)
            .ok_or_else(|| "shielded value pool overflow".to_string())?;
        let total_supply = utxo_stats
            .total_amount
            .checked_add(shielded_total)
            .ok_or_else(|| "total supply overflow".to_string())?;
        json!({
            "available": true,
            "transparent_zat": utxo_stats.total_amount,
            "sprout_zat": value_pools.sprout,
            "sapling_zat": value_pools.sapling,
            "shielded_zat": shielded_total,
            "total_zat": total_supply,
        })
    } else {
        json!({
            "available": false,
            "utxo_stats_present": utxo_stats.is_some(),
            "value_pools_present": value_pools.is_some(),
        })
    };

    let fjall = store.fjall_telemetry_snapshot().map(|snapshot| {
        json!({
            "write_buffer_bytes": snapshot.write_buffer_bytes,
            "max_write_buffer_bytes": snapshot.max_write_buffer_bytes,
            "journal_count": snapshot.journal_count,
            "journal_disk_space_bytes": snapshot.journal_disk_space_bytes,
            "max_journal_bytes": snapshot.max_journal_bytes,
            "flushes_completed": snapshot.flushes_completed,
            "active_compactions": snapshot.active_compactions,
            "compactions_completed": snapshot.compactions_completed,
            "time_compacting_us": snapshot.time_compacting_us,
            "utxo_segments": snapshot.utxo_segments,
            "utxo_flushes_completed": snapshot.utxo_flushes_completed,
            "tx_index_segments": snapshot.tx_index_segments,
            "tx_index_flushes_completed": snapshot.tx_index_flushes_completed,
            "spent_index_segments": snapshot.spent_index_segments,
            "spent_index_flushes_completed": snapshot.spent_index_flushes_completed,
            "address_outpoint_segments": snapshot.address_outpoint_segments,
            "address_outpoint_flushes_completed": snapshot.address_outpoint_flushes_completed,
            "address_delta_segments": snapshot.address_delta_segments,
            "address_delta_flushes_completed": snapshot.address_delta_flushes_completed,
            "header_index_segments": snapshot.header_index_segments,
            "header_index_flushes_completed": snapshot.header_index_flushes_completed,
        })
    });

    let mut files = BTreeMap::new();
    files.insert("peers.dat".to_string(), file_size_or_zero(&peers_path)?);
    files.insert("banlist.dat".to_string(), file_size_or_zero(&banlist_path)?);
    files.insert("mempool.dat".to_string(), file_size_or_zero(&mempool_path)?);
    files.insert(
        "fee_estimates.dat".to_string(),
        file_size_or_zero(&fee_estimates_path)?,
    );
    files.insert("rpc.cookie".to_string(), file_size_or_zero(&cookie_path)?);
    files.insert(
        "reindex.flag".to_string(),
        file_size_or_zero(&reindex_flag_path)?,
    );

    let approx_data_dir_bytes = db_dir_size
        .saturating_add(block_meta.total_bytes)
        .saturating_add(undo_meta.total_bytes)
        .saturating_add(files.values().copied().sum::<u64>());

    let (integrity_ok, integrity_issues, integrity_warnings) =
        flatfile_integrity_summary(&block_meta, &undo_meta, &blocks_fs);

    Ok(json!({
        "backend": match backend {
            Backend::Fjall => "fjall",
            Backend::Memory => "memory",
        },
        "schema": {
            "db_schema_version": db_schema_version,
            "expected_db_schema_version": crate::DB_SCHEMA_VERSION,
        },
        "paths": {
            "data_dir": data_dir.display().to_string(),
            "db_dir": db_dir.display().to_string(),
            "blocks_dir": blocks_dir.display().to_string(),
        },
        "chain": {
            "best_header_height": best_header.as_ref().map(|tip| tip.height).unwrap_or(-1).max(0),
            "best_block_height": best_block.as_ref().map(|tip| tip.height).unwrap_or(-1).max(0),
        },
        "supply": supply,
        "sizes": {
            "approx_data_dir_bytes": approx_data_dir_bytes,
            "db_dir_bytes": db_dir_size,
            "db_partitions_bytes": partitions_total_bytes,
            "db_journals_bytes": journals_size_bytes,
            "blocks_dir_bytes": blocks_dir_size,
            "blocks_meta_bytes": block_meta.total_bytes,
            "undo_meta_bytes": undo_meta.total_bytes,
        },
        "flatfiles_meta": {
            "blocks": {
                "files": block_meta.file_count,
                "total_bytes": block_meta.total_bytes,
                "last_file_id": block_meta.last_file_id,
                "last_file_len": block_meta.last_file_len,
            },
            "undo": {
                "files": undo_meta.file_count,
                "total_bytes": undo_meta.total_bytes,
                "last_file_id": undo_meta.last_file_id,
                "last_file_len": undo_meta.last_file_len,
            },
        },
        "flatfiles_fs": {
            "data_files": blocks_fs.data_files,
            "data_bytes": blocks_fs.data_bytes,
            "undo_files": blocks_fs.undo_files,
            "undo_bytes": blocks_fs.undo_bytes,
            "other_files": blocks_fs.other_files,
            "other_bytes": blocks_fs.other_bytes,
        },
        "db_partitions": partitions,
        "files": files,
        "fjall": fjall,
        "integrity": {
            "ok": integrity_ok,
            "issues": integrity_issues,
            "warnings": integrity_warnings,
        },
    }))
}

#[derive(Clone, Copy, Debug, Default)]
struct ColumnKeyStats {
    key_count: u64,
    key_bytes: u64,
    value_bytes: u64,
}

fn column_key_stats<S: KeyValueStore>(store: &S, column: Column) -> Result<ColumnKeyStats, String> {
    let mut stats = ColumnKeyStats::default();
    let mut visitor = |key: &[u8], value: &[u8]| {
        stats.key_count = stats.key_count.saturating_add(1);
        stats.key_bytes = stats.key_bytes.saturating_add(key.len() as u64);
        stats.value_bytes = stats.value_bytes.saturating_add(value.len() as u64);
        Ok(())
    };
    store
        .for_each_prefix(column, &[], &mut visitor)
        .map_err(|err| err.to_string())?;
    Ok(stats)
}

fn flatfile_integrity_summary(
    blocks: &FlatfileMetaSummary,
    undo: &FlatfileMetaSummary,
    fs: &FlatfileFsSummary,
) -> (bool, Vec<String>, Vec<String>) {
    let mut issues = Vec::new();
    let mut warnings = Vec::new();

    if blocks.file_count != fs.data_files {
        issues.push(format!(
            "flatfiles meta mismatch: blocks files {} != fs data_files {}",
            blocks.file_count, fs.data_files
        ));
    }
    if blocks.total_bytes != fs.data_bytes {
        issues.push(format!(
            "flatfiles meta mismatch: blocks bytes {} != fs data_bytes {}",
            blocks.total_bytes, fs.data_bytes
        ));
    }
    if blocks.last_file_id != fs.data_max_file_id {
        issues.push(format!(
            "flatfiles meta mismatch: blocks last_file_id {:?} != fs data_last_file_id {:?}",
            blocks.last_file_id, fs.data_max_file_id
        ));
    }
    if blocks.last_file_len != fs.data_max_file_len {
        issues.push(format!(
            "flatfiles meta mismatch: blocks last_file_len {:?} != fs data_last_file_len {:?}",
            blocks.last_file_len, fs.data_max_file_len
        ));
    }

    if undo.file_count != fs.undo_files {
        issues.push(format!(
            "flatfiles meta mismatch: undo files {} != fs undo_files {}",
            undo.file_count, fs.undo_files
        ));
    }
    if undo.total_bytes != fs.undo_bytes {
        issues.push(format!(
            "flatfiles meta mismatch: undo bytes {} != fs undo_bytes {}",
            undo.total_bytes, fs.undo_bytes
        ));
    }
    if undo.last_file_id != fs.undo_max_file_id {
        issues.push(format!(
            "flatfiles meta mismatch: undo last_file_id {:?} != fs undo_last_file_id {:?}",
            undo.last_file_id, fs.undo_max_file_id
        ));
    }
    if undo.last_file_len != fs.undo_max_file_len {
        issues.push(format!(
            "flatfiles meta mismatch: undo last_file_len {:?} != fs undo_last_file_len {:?}",
            undo.last_file_len, fs.undo_max_file_len
        ));
    }

    if fs.other_files > 0 {
        warnings.push(format!(
            "flatfiles directory contains {} extra file(s) ({} bytes)",
            fs.other_files, fs.other_bytes
        ));
    }

    (issues.is_empty(), issues, warnings)
}

fn scan_flatfile_meta_blocks(store: &Store) -> Result<FlatfileMetaSummary, String> {
    scan_flatfile_meta(
        store,
        b"flatfiles:blocks:file:",
        META_BLOCK_FILES_LAST_FILE_KEY,
        META_BLOCK_FILES_LAST_LEN_KEY,
        parse_block_file_info_key,
    )
}

fn scan_flatfile_meta_undo(store: &Store) -> Result<FlatfileMetaSummary, String> {
    scan_flatfile_meta(
        store,
        b"flatfiles:undo:file:",
        META_UNDO_FILES_LAST_FILE_KEY,
        META_UNDO_FILES_LAST_LEN_KEY,
        parse_undo_file_info_key,
    )
}

fn scan_flatfile_meta(
    store: &Store,
    prefix: &[u8],
    last_file_key: &[u8],
    last_len_key: &[u8],
    parse_key: fn(&[u8]) -> Option<u32>,
) -> Result<FlatfileMetaSummary, String> {
    let mut summary = FlatfileMetaSummary::default();

    summary.last_file_id = read_u32_le(store, last_file_key)?;
    summary.last_file_len = read_u64_le(store, last_len_key)?;

    let entries = store
        .scan_prefix(Column::Meta, prefix)
        .map_err(|err| err.to_string())?;
    summary.file_count = entries.len() as u64;

    let mut total = 0u64;
    for (key, value) in entries {
        if parse_key(&key).is_none() {
            continue;
        }
        let Some(info) = FlatFileInfo::decode(&value) else {
            continue;
        };
        total = total.saturating_add(info.size);
    }
    summary.total_bytes = total;

    Ok(summary)
}

fn scan_blocks_dir_fs(blocks_dir: &Path) -> Result<FlatfileFsSummary, String> {
    let entries = match fs::read_dir(blocks_dir) {
        Ok(entries) => entries,
        Err(err) if err.kind() == std::io::ErrorKind::NotFound => {
            return Ok(FlatfileFsSummary::default());
        }
        Err(err) => return Err(err.to_string()),
    };

    let mut summary = FlatfileFsSummary::default();
    for entry in entries {
        let entry = entry.map_err(|err| err.to_string())?;
        let meta = entry.metadata().map_err(|err| err.to_string())?;
        if !meta.is_file() {
            continue;
        }
        let name = entry.file_name();
        let name = name.to_string_lossy();
        let len = meta.len();
        if name.starts_with("data") && name.ends_with(".dat") {
            summary.data_files = summary.data_files.saturating_add(1);
            summary.data_bytes = summary.data_bytes.saturating_add(len);
            if let Some(file_id) = parse_flatfile_id(name.as_ref(), "data") {
                if summary
                    .data_max_file_id
                    .map(|current| file_id > current)
                    .unwrap_or(true)
                {
                    summary.data_max_file_id = Some(file_id);
                    summary.data_max_file_len = Some(len);
                }
            }
        } else if name.starts_with("undo") && name.ends_with(".dat") {
            summary.undo_files = summary.undo_files.saturating_add(1);
            summary.undo_bytes = summary.undo_bytes.saturating_add(len);
            if let Some(file_id) = parse_flatfile_id(name.as_ref(), "undo") {
                if summary
                    .undo_max_file_id
                    .map(|current| file_id > current)
                    .unwrap_or(true)
                {
                    summary.undo_max_file_id = Some(file_id);
                    summary.undo_max_file_len = Some(len);
                }
            }
        } else {
            summary.other_files = summary.other_files.saturating_add(1);
            summary.other_bytes = summary.other_bytes.saturating_add(len);
        }
    }

    Ok(summary)
}

fn parse_flatfile_id(name: &str, prefix: &str) -> Option<u32> {
    let suffix = ".dat";
    if !name.starts_with(prefix) || !name.ends_with(suffix) {
        return None;
    }
    let numeric = name.strip_prefix(prefix)?.strip_suffix(suffix)?.trim();
    numeric.parse::<u32>().ok()
}

pub(crate) fn dir_size_cached(path: &Path, ttl: Duration) -> Result<u64, String> {
    static CACHE: OnceLock<Mutex<BTreeMap<PathBuf, (Instant, u64)>>> = OnceLock::new();
    let cache = CACHE.get_or_init(|| Mutex::new(BTreeMap::new()));

    if let Ok(guard) = cache.lock() {
        if let Some((ts, size)) = guard.get(path) {
            if ts.elapsed() <= ttl {
                return Ok(*size);
            }
        }
    }

    let size = dir_size(path)?;
    if let Ok(mut guard) = cache.lock() {
        guard.insert(path.to_path_buf(), (Instant::now(), size));
    }
    Ok(size)
}

pub(crate) fn dir_size(path: &Path) -> Result<u64, String> {
    let entries = match fs::read_dir(path) {
        Ok(entries) => entries,
        Err(err) if err.kind() == std::io::ErrorKind::NotFound => return Ok(0),
        Err(err) => return Err(err.to_string()),
    };

    let mut total = 0u64;
    for entry in entries {
        let entry = entry.map_err(|err| err.to_string())?;
        let meta = entry.metadata().map_err(|err| err.to_string())?;
        if meta.is_dir() {
            total = total.saturating_add(dir_size(&entry.path())?);
        } else {
            total = total.saturating_add(meta.len());
        }
    }
    Ok(total)
}

fn file_size_or_zero(path: &Path) -> Result<u64, String> {
    match fs::metadata(path) {
        Ok(meta) => Ok(meta.len()),
        Err(err) if err.kind() == std::io::ErrorKind::NotFound => Ok(0),
        Err(err) => Err(err.to_string()),
    }
}

fn read_u32_le<S: KeyValueStore>(store: &S, key: &[u8]) -> Result<Option<u32>, String> {
    let Some(bytes) = store
        .get(Column::Meta, key)
        .map_err(|err| err.to_string())?
    else {
        return Ok(None);
    };
    if bytes.len() != 4 {
        return Ok(None);
    }
    let mut raw = [0u8; 4];
    raw.copy_from_slice(&bytes);
    Ok(Some(u32::from_le_bytes(raw)))
}

fn read_u64_le<S: KeyValueStore>(store: &S, key: &[u8]) -> Result<Option<u64>, String> {
    let Some(bytes) = store
        .get(Column::Meta, key)
        .map_err(|err| err.to_string())?
    else {
        return Ok(None);
    };
    if bytes.len() != 8 {
        return Ok(None);
    }
    let mut raw = [0u8; 8];
    raw.copy_from_slice(&bytes);
    Ok(Some(u64::from_le_bytes(raw)))
}
