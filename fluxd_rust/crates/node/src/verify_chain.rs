use fluxd_chainstate::state::ChainState;
use fluxd_consensus::money::money_range;
use fluxd_consensus::Hash256;
use fluxd_primitives::block::Block;
use fluxd_primitives::hash::sha256d;
use fluxd_primitives::outpoint::OutPoint;

use crate::stats::hash256_to_hex;

pub(crate) fn verify_chain<S: fluxd_storage::KeyValueStore>(
    chainstate: &ChainState<S>,
    checklevel: u32,
    numblocks: u32,
) -> Result<(), String> {
    if checklevel == 0 {
        return Ok(());
    }

    let best = chainstate.best_block().map_err(|err| err.to_string())?;
    let Some(best) = best else {
        return Ok(());
    };
    let best_height = best.height.max(0) as u32;
    let mut remaining = if numblocks == 0 {
        best_height.saturating_add(1)
    } else {
        numblocks.min(best_height.saturating_add(1))
    };

    let mut current_hash = best.hash;
    while remaining > 0 {
        let entry = chainstate
            .header_entry(&current_hash)
            .map_err(|err| err.to_string())?
            .ok_or_else(|| format!("missing header entry {}", hash256_to_hex(&current_hash)))?;
        if !entry.has_block() {
            return Err(format!(
                "missing block data at height {}",
                entry.height.max(0)
            ));
        }
        let main_hash = chainstate
            .height_hash(entry.height)
            .map_err(|err| err.to_string())?;
        if main_hash.as_ref() != Some(&current_hash) {
            return Err(format!("height index mismatch at {}", entry.height.max(0)));
        }

        if checklevel >= 1 {
            let block_location = chainstate
                .block_location(&current_hash)
                .map_err(|err| err.to_string())?
                .ok_or_else(|| format!("missing block index {}", hash256_to_hex(&current_hash)))?;
            let bytes = chainstate
                .read_block(block_location)
                .map_err(|err| err.to_string())?;
            let block = Block::consensus_decode(&bytes).map_err(|err| err.to_string())?;
            if block.header.hash() != current_hash {
                return Err(format!(
                    "block hash mismatch at height {}",
                    entry.height.max(0)
                ));
            }
            if block.header.prev_block != entry.prev_hash {
                return Err(format!(
                    "block prev-hash mismatch at height {}",
                    entry.height.max(0)
                ));
            }

            if checklevel >= 2 {
                let mut txids = Vec::with_capacity(block.transactions.len());
                for tx in &block.transactions {
                    txids.push(tx.txid().map_err(|err| err.to_string())?);
                }
                let root = compute_merkle_root(&txids);
                if root != block.header.merkle_root {
                    return Err(format!(
                        "merkle root mismatch at height {}",
                        entry.height.max(0)
                    ));
                }

                if checklevel >= 3 {
                    for (index, txid) in txids.iter().enumerate() {
                        let tx_location = chainstate
                            .tx_location(&txid)
                            .map_err(|err| err.to_string())?
                            .ok_or_else(|| {
                                format!("missing txindex entry {}", hash256_to_hex(&txid))
                            })?;
                        if tx_location.block != block_location {
                            return Err(format!(
                                "txindex block location mismatch {}",
                                hash256_to_hex(&txid)
                            ));
                        }
                        if tx_location.index != index as u32 {
                            return Err(format!(
                                "txindex position mismatch {}",
                                hash256_to_hex(&txid)
                            ));
                        }
                    }
                }

                if checklevel >= 4 {
                    let height_u32 = entry.height.max(0) as u32;
                    for (tx_index, tx) in block.transactions.iter().enumerate() {
                        let txid = txids
                            .get(tx_index)
                            .copied()
                            .ok_or_else(|| "transaction id cache mismatch".to_string())?;
                        for (vin_index, input) in tx.vin.iter().enumerate() {
                            if input.prevout == OutPoint::null() {
                                continue;
                            }
                            let spent = chainstate
                                .spent_info(&input.prevout)
                                .map_err(|err| err.to_string())?
                                .ok_or_else(|| {
                                    format!(
                                        "missing spent index entry {}:{} (spent by {}) at height {}",
                                        hash256_to_hex(&input.prevout.hash),
                                        input.prevout.index,
                                        hash256_to_hex(&txid),
                                        height_u32
                                    )
                                })?;

                            if spent.txid != txid {
                                return Err(format!(
                                    "spent index mismatch for {}:{} at height {}: expected spender txid {} got {}",
                                    hash256_to_hex(&input.prevout.hash),
                                    input.prevout.index,
                                    height_u32,
                                    hash256_to_hex(&txid),
                                    hash256_to_hex(&spent.txid)
                                ));
                            }
                            if spent.input_index != vin_index as u32 {
                                return Err(format!(
                                    "spent index mismatch for {}:{} at height {}: expected vin {} got {}",
                                    hash256_to_hex(&input.prevout.hash),
                                    input.prevout.index,
                                    height_u32,
                                    vin_index,
                                    spent.input_index
                                ));
                            }
                            if spent.block_height != height_u32 {
                                return Err(format!(
                                    "spent index mismatch for {}:{}: expected spend height {} got {}",
                                    hash256_to_hex(&input.prevout.hash),
                                    input.prevout.index,
                                    height_u32,
                                    spent.block_height
                                ));
                            }

                            if let Some(details) = spent.details {
                                if !money_range(details.satoshis) {
                                    return Err(format!(
                                        "spent index invalid satoshis for {}:{} at height {}: {}",
                                        hash256_to_hex(&input.prevout.hash),
                                        input.prevout.index,
                                        height_u32,
                                        details.satoshis
                                    ));
                                }
                                if !matches!(details.address_type, 0 | 1 | 2) {
                                    return Err(format!(
                                        "spent index invalid address type for {}:{} at height {}: {}",
                                        hash256_to_hex(&input.prevout.hash),
                                        input.prevout.index,
                                        height_u32,
                                        details.address_type
                                    ));
                                }
                                if details.address_type == 0 && details.address_hash != [0u8; 20] {
                                    return Err(format!(
                                        "spent index unexpected address hash for {}:{} at height {}",
                                        hash256_to_hex(&input.prevout.hash),
                                        input.prevout.index,
                                        height_u32
                                    ));
                                }
                            }
                        }
                    }
                }
            }
        }

        remaining = remaining.saturating_sub(1);
        if entry.height == 0 {
            break;
        }
        current_hash = entry.prev_hash;
    }

    Ok(())
}

pub(crate) fn compute_merkle_root(txids: &[Hash256]) -> Hash256 {
    if txids.is_empty() {
        return [0u8; 32];
    }
    let mut layer = txids.to_vec();
    while layer.len() > 1 {
        if layer.len() % 2 == 1 {
            let last = *layer.last().expect("non-empty");
            layer.push(last);
        }
        let mut next = Vec::with_capacity((layer.len() + 1) / 2);
        for pair in layer.chunks(2) {
            next.push(merkle_hash_pair(&pair[0], &pair[1]));
        }
        layer = next;
    }
    layer[0]
}

fn merkle_hash_pair(left: &Hash256, right: &Hash256) -> Hash256 {
    let mut buf = [0u8; 64];
    buf[0..32].copy_from_slice(left);
    buf[32..64].copy_from_slice(right);
    sha256d(&buf)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::ensure_genesis;
    use fluxd_chainstate::flatfiles::FlatFileStore;
    use fluxd_chainstate::utxo::outpoint_key_bytes;
    use fluxd_chainstate::validation::ValidationFlags;
    use fluxd_consensus::money::COIN;
    use fluxd_consensus::params::chain_params;
    use fluxd_consensus::block_subsidy;
    use fluxd_consensus::Network;
    use fluxd_primitives::block::{Block, BlockHeader, CURRENT_VERSION};
    use fluxd_primitives::transaction::{Transaction, TxIn, TxOut};
    use fluxd_storage::memory::MemoryStore;
    use fluxd_storage::{Column, WriteBatch};
    use std::path::PathBuf;
    use std::sync::{Arc, Mutex};
    use std::time::{SystemTime, UNIX_EPOCH};

    fn temp_data_dir(prefix: &str) -> PathBuf {
        let nanos = SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .unwrap_or_default()
            .as_nanos();
        std::env::temp_dir().join(format!("{prefix}-{}-{nanos}", std::process::id()))
    }

    fn p2pkh_script(pubkey_hash: [u8; 20]) -> Vec<u8> {
        let mut out = Vec::with_capacity(25);
        out.extend_from_slice(&[0x76, 0xa9, 0x14]);
        out.extend_from_slice(&pubkey_hash);
        out.extend_from_slice(&[0x88, 0xac]);
        out
    }

    #[test]
    fn verifychain_checklevel4_detects_missing_spentindex_entries() {
        let data_dir = temp_data_dir("fluxd-verifychain-test");
        std::fs::create_dir_all(&data_dir).expect("create data dir");
        let blocks_dir = data_dir.join("blocks");
        let blocks = FlatFileStore::new(&blocks_dir, 10_000_000).expect("flatfiles");
        let undo = FlatFileStore::new_with_prefix(&blocks_dir, "undo", 10_000_000).expect("undo");
        let store = Arc::new(MemoryStore::new());
        let chainstate = ChainState::new(Arc::clone(&store), blocks, undo);

        let params = chain_params(Network::Regtest);
        let flags = ValidationFlags::default();
        let write_lock = Mutex::new(());
        ensure_genesis(&chainstate, &params, &flags, None, &write_lock).expect("insert genesis");

        let seed_outpoint = OutPoint {
            hash: [0x99u8; 32],
            index: 0,
        };
        let seed_script = p2pkh_script([0x11u8; 20]);
        let seed_value = 2 * COIN;

        let utxo_entry = fluxd_chainstate::utxo::UtxoEntry {
            value: seed_value,
            script_pubkey: seed_script.clone(),
            height: 0,
            is_coinbase: false,
        };
        let utxo_key = outpoint_key_bytes(&seed_outpoint);
        let addr_key =
            fluxd_chainstate::address_index::address_outpoint_key(&seed_script, &seed_outpoint)
                .expect("address outpoint key");
        let mut seed_batch = WriteBatch::new();
        seed_batch.put(Column::Utxo, utxo_key.as_bytes(), utxo_entry.encode());
        seed_batch.put(Column::AddressOutpoint, addr_key, []);
        chainstate.commit_batch(seed_batch).expect("seed utxo");

        let spend_tx = Transaction {
            f_overwintered: false,
            version: 1,
            version_group_id: 0,
            vin: vec![TxIn {
                prevout: seed_outpoint.clone(),
                script_sig: Vec::new(),
                sequence: 0,
            }],
            vout: vec![TxOut {
                value: seed_value,
                script_pubkey: p2pkh_script([0x22u8; 20]),
            }],
            lock_time: 0,
            expiry_height: 0,
            value_balance: 0,
            shielded_spends: Vec::new(),
            shielded_outputs: Vec::new(),
            join_splits: Vec::new(),
            join_split_pub_key: [0u8; 32],
            join_split_sig: [0u8; 64],
            binding_sig: [0u8; 64],
            fluxnode: None,
        };

        let coinbase = Transaction {
            f_overwintered: false,
            version: 1,
            version_group_id: 0,
            vin: vec![TxIn {
                prevout: OutPoint::null(),
                script_sig: Vec::new(),
                sequence: u32::MAX,
            }],
            vout: vec![TxOut {
                value: block_subsidy(1, &params.consensus),
                script_pubkey: Vec::new(),
            }],
            lock_time: 0,
            expiry_height: 0,
            value_balance: 0,
            shielded_spends: Vec::new(),
            shielded_outputs: Vec::new(),
            join_splits: Vec::new(),
            join_split_pub_key: [0u8; 32],
            join_split_sig: [0u8; 64],
            binding_sig: [0u8; 64],
            fluxnode: None,
        };

        let txids = [
            coinbase.txid().expect("coinbase txid"),
            spend_tx.txid().expect("spend txid"),
        ];
        let merkle_root = compute_merkle_root(&txids);

        let tip = chainstate
            .best_block()
            .expect("best block")
            .expect("best block present");
        let tip_entry = chainstate
            .header_entry(&tip.hash)
            .expect("header entry")
            .expect("header entry present");
        let height = 1;
        let time = tip_entry.time.saturating_add(1);
        let bits = chainstate
            .next_work_required_bits(&tip.hash, height, time as i64, &params.consensus)
            .expect("next bits");

        let header = BlockHeader {
            version: CURRENT_VERSION,
            prev_block: tip.hash,
            merkle_root,
            final_sapling_root: chainstate.sapling_root().expect("sapling root"),
            time,
            bits,
            nonce: [0u8; 32],
            solution: Vec::new(),
            nodes_collateral: OutPoint::null(),
            block_sig: Vec::new(),
        };

        let mut header_batch = WriteBatch::new();
        chainstate
            .insert_headers_batch_with_pow(
                &[header.clone()],
                &params.consensus,
                &mut header_batch,
                false,
            )
            .expect("insert header");
        chainstate
            .commit_batch(header_batch)
            .expect("commit header");

        let block = Block {
            header,
            transactions: vec![coinbase, spend_tx],
        };
        let block_bytes = block.consensus_encode().expect("encode block");
        let batch = chainstate
            .connect_block(
                &block,
                height,
                &params,
                &flags,
                true,
                None,
                None,
                Some(block_bytes.as_slice()),
                None,
            )
            .expect("connect block");
        chainstate.commit_batch(batch).expect("commit block");

        verify_chain(&chainstate, 4, 1).expect("verifychain checklevel4 ok");

        let mut corrupt = WriteBatch::new();
        let spent_key = outpoint_key_bytes(&seed_outpoint);
        corrupt.delete(Column::SpentIndex, spent_key.as_bytes());
        chainstate
            .commit_batch(corrupt)
            .expect("corrupt spent index");

        let err = verify_chain(&chainstate, 4, 1).unwrap_err();
        assert!(err.contains("missing spent index entry"), "{err}");
    }
}
