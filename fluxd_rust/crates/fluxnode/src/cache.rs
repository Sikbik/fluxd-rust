//! Fluxnode cache and persistence.

use fluxd_primitives::encoding::{Encodable, Encoder};
use fluxd_primitives::outpoint::OutPoint;
use fluxd_primitives::transaction::{
    FluxnodeStartVariantV6, FluxnodeTx, FluxnodeTxV5, FluxnodeTxV6, Transaction,
};
use fluxd_storage::{Column, KeyValueStore, StoreError, WriteBatch};

use crate::storage::{dedupe_key, FluxnodeRecord, KeyId};

pub fn apply_fluxnode_tx<S: KeyValueStore>(
    store: &S,
    batch: &mut WriteBatch,
    tx: &Transaction,
    height: u32,
) -> Result<(), StoreError> {
    let Some(fluxnode) = tx.fluxnode.as_ref() else {
        return Ok(());
    };

    match fluxnode {
        FluxnodeTx::V5(FluxnodeTxV5::Start(start)) => {
            let operator_pubkey = start.pubkey.as_slice();
            let collateral_pubkey = Some(start.collateral_pubkey.as_slice());
            store_fluxnode_start(
                batch,
                &start.collateral,
                operator_pubkey,
                collateral_pubkey,
                None,
                height,
            )?;
        }
        FluxnodeTx::V6(FluxnodeTxV6::Start(start)) => match &start.variant {
            FluxnodeStartVariantV6::Normal {
                collateral,
                collateral_pubkey,
                pubkey,
                ..
            } => {
                store_fluxnode_start(
                    batch,
                    collateral,
                    pubkey.as_slice(),
                    Some(collateral_pubkey.as_slice()),
                    None,
                    height,
                )?;
            }
            FluxnodeStartVariantV6::P2sh {
                collateral,
                pubkey,
                redeem_script,
                ..
            } => {
                store_fluxnode_start(
                    batch,
                    collateral,
                    pubkey.as_slice(),
                    None,
                    Some(redeem_script.as_slice()),
                    height,
                )?;
            }
        },
        FluxnodeTx::V5(FluxnodeTxV5::Confirm(confirm))
        | FluxnodeTx::V6(FluxnodeTxV6::Confirm(confirm)) => {
            update_fluxnode_confirm(store, batch, &confirm.collateral, height)?;
        }
    }

    Ok(())
}

pub fn lookup_operator_pubkey<S: KeyValueStore>(
    store: &S,
    outpoint: &OutPoint,
) -> Result<Option<Vec<u8>>, StoreError> {
    let Some(record) = load_fluxnode_record(store, outpoint)? else {
        return Ok(None);
    };
    load_key(store, record.operator_pubkey)
}

fn store_fluxnode_start(
    batch: &mut WriteBatch,
    collateral: &OutPoint,
    operator_pubkey: &[u8],
    collateral_pubkey: Option<&[u8]>,
    redeem_script: Option<&[u8]>,
    height: u32,
) -> Result<(), StoreError> {
    let operator_key = store_key(batch, operator_pubkey);
    let collateral_key = collateral_pubkey.map(|key| store_key(batch, key));
    let p2sh_key = redeem_script.map(|script| store_key(batch, script));

    let record = FluxnodeRecord {
        collateral: collateral.clone(),
        tier: 0,
        start_height: height,
        last_confirmed_height: height,
        last_paid_height: 0,
        operator_pubkey: operator_key,
        collateral_pubkey: collateral_key,
        p2sh_script: p2sh_key,
    };

    batch.put(Column::Fluxnode, outpoint_key(collateral), record.encode());
    Ok(())
}

fn update_fluxnode_confirm<S: KeyValueStore>(
    store: &S,
    batch: &mut WriteBatch,
    collateral: &OutPoint,
    height: u32,
) -> Result<(), StoreError> {
    let Some(mut record) = load_fluxnode_record(store, collateral)? else {
        return Ok(());
    };
    record.last_confirmed_height = height;
    batch.put(Column::Fluxnode, outpoint_key(collateral), record.encode());
    Ok(())
}

fn load_fluxnode_record<S: KeyValueStore>(
    store: &S,
    outpoint: &OutPoint,
) -> Result<Option<FluxnodeRecord>, StoreError> {
    let Some(bytes) = store.get(Column::Fluxnode, &outpoint_key(outpoint))? else {
        return Ok(None);
    };
    FluxnodeRecord::decode(&bytes)
        .map(Some)
        .map_err(|err| StoreError::Backend(err.to_string()))
}

fn store_key(batch: &mut WriteBatch, bytes: &[u8]) -> KeyId {
    let key = dedupe_key(bytes);
    batch.put(Column::FluxnodeKey, key.0.to_vec(), bytes.to_vec());
    key
}

fn load_key<S: KeyValueStore>(store: &S, key: KeyId) -> Result<Option<Vec<u8>>, StoreError> {
    store.get(Column::FluxnodeKey, &key.0)
}

fn outpoint_key(outpoint: &OutPoint) -> Vec<u8> {
    let mut encoder = Encoder::new();
    outpoint.consensus_encode(&mut encoder);
    encoder.into_inner()
}

#[cfg(test)]
mod tests {
    use super::{apply_fluxnode_tx, lookup_operator_pubkey};
    use fluxd_primitives::outpoint::OutPoint;
    use fluxd_primitives::transaction::{
        FluxnodeStartV5, FluxnodeTx, FluxnodeTxV5, Transaction, FLUXNODE_TX_VERSION,
    };
    use fluxd_storage::{memory::MemoryStore, KeyValueStore, WriteBatch};

    fn make_start_tx(outpoint: OutPoint, pubkey: Vec<u8>) -> Transaction {
        Transaction {
            f_overwintered: false,
            version: FLUXNODE_TX_VERSION,
            version_group_id: 0,
            vin: Vec::new(),
            vout: Vec::new(),
            lock_time: 0,
            expiry_height: 0,
            value_balance: 0,
            shielded_spends: Vec::new(),
            shielded_outputs: Vec::new(),
            join_splits: Vec::new(),
            join_split_pub_key: [0u8; 32],
            join_split_sig: [0u8; 64],
            binding_sig: [0u8; 64],
            fluxnode: Some(FluxnodeTx::V5(FluxnodeTxV5::Start(FluxnodeStartV5 {
                collateral: outpoint,
                collateral_pubkey: vec![0x02, 0x01],
                pubkey,
                sig_time: 0,
                sig: Vec::new(),
            }))),
        }
    }

    #[test]
    fn stores_and_loads_operator_pubkey() {
        let store = MemoryStore::new();
        let outpoint = OutPoint {
            hash: [0x11; 32],
            index: 1,
        };
        let pubkey = vec![0x02, 0x12, 0x34];
        let tx = make_start_tx(outpoint.clone(), pubkey.clone());

        let mut batch = WriteBatch::new();
        apply_fluxnode_tx(&store, &mut batch, &tx, 100).expect("apply tx");
        store.write_batch(&batch).expect("write batch");

        let loaded = lookup_operator_pubkey(&store, &outpoint)
            .expect("lookup")
            .expect("operator pubkey");
        assert_eq!(loaded, pubkey);
    }
}
