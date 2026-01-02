use fluxd_chainstate::state::ChainState;
use fluxd_consensus::Hash256;
use fluxd_primitives::block::Block;
use fluxd_primitives::hash::sha256d;

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
                    for (index, txid) in txids.into_iter().enumerate() {
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
