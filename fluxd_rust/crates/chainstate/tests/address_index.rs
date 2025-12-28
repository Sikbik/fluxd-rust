use std::sync::Arc;

use fluxd_chainstate::address_index::AddressIndex;
use fluxd_primitives::hash::hash160;
use fluxd_primitives::outpoint::OutPoint;
use fluxd_storage::memory::MemoryStore;
use fluxd_storage::{KeyValueStore, WriteBatch};

#[test]
fn address_index_roundtrip_p2pkh() {
    let store = Arc::new(MemoryStore::new());
    let index = AddressIndex::new(Arc::clone(&store));
    let mut script = Vec::with_capacity(25);
    script.extend_from_slice(&[0x76, 0xa9, 0x14]);
    script.extend_from_slice(&[0x11; 20]);
    script.extend_from_slice(&[0x88, 0xac]);
    let outpoint = OutPoint {
        hash: [0x11; 32],
        index: 7,
    };

    let mut batch = WriteBatch::new();
    index.insert(&mut batch, &script, &outpoint);
    store.write_batch(&batch).expect("commit");

    let outpoints = index.scan(&script).expect("scan");
    assert_eq!(outpoints, vec![outpoint.clone()]);

    let mut batch = WriteBatch::new();
    index.delete(&mut batch, &script, &outpoint);
    store.write_batch(&batch).expect("commit");

    let outpoints = index.scan(&script).expect("scan");
    assert!(outpoints.is_empty());
}

#[test]
fn address_index_normalizes_p2pk_to_p2pkh() {
    let store = Arc::new(MemoryStore::new());
    let index = AddressIndex::new(Arc::clone(&store));

    let pubkey = vec![0x02; 33];
    let pubkey_hash = hash160(&pubkey);

    let mut p2pkh = Vec::with_capacity(25);
    p2pkh.extend_from_slice(&[0x76, 0xa9, 0x14]);
    p2pkh.extend_from_slice(&pubkey_hash);
    p2pkh.extend_from_slice(&[0x88, 0xac]);

    let mut p2pk = Vec::with_capacity(35);
    p2pk.push(33);
    p2pk.extend_from_slice(&pubkey);
    p2pk.push(0xac);

    let outpoint = OutPoint {
        hash: [0x22; 32],
        index: 1,
    };

    let mut batch = WriteBatch::new();
    index.insert(&mut batch, &p2pk, &outpoint);
    store.write_batch(&batch).expect("commit");

    let outpoints = index.scan(&p2pkh).expect("scan");
    assert_eq!(outpoints, vec![outpoint]);
}
