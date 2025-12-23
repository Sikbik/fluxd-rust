use std::sync::Arc;

use fluxd_chainstate::address_index::AddressIndex;
use fluxd_primitives::outpoint::OutPoint;
use fluxd_storage::memory::MemoryStore;
use fluxd_storage::{KeyValueStore, WriteBatch};

#[test]
fn address_index_roundtrip() {
    let store = Arc::new(MemoryStore::new());
    let index = AddressIndex::new(Arc::clone(&store));
    let script = vec![0x51];
    let outpoint = OutPoint {
        hash: [0x11; 32],
        index: 7,
    };

    let mut batch = WriteBatch::new();
    index.insert(&mut batch, &script, &outpoint);
    store.write_batch(batch).expect("commit");

    let outpoints = index.scan(&script).expect("scan");
    assert_eq!(outpoints, vec![outpoint.clone()]);

    let mut batch = WriteBatch::new();
    index.delete(&mut batch, &script, &outpoint);
    store.write_batch(batch).expect("commit");

    let outpoints = index.scan(&script).expect("scan");
    assert!(outpoints.is_empty());
}
