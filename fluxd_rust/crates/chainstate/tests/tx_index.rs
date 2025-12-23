use std::sync::Arc;

use fluxd_chainstate::flatfiles::FileLocation;
use fluxd_chainstate::txindex::{TxIndex, TxLocation};
use fluxd_storage::memory::MemoryStore;
use fluxd_storage::{KeyValueStore, WriteBatch};

#[test]
fn tx_location_encode_decode() {
    let location = TxLocation {
        block: FileLocation {
            file_id: 3,
            offset: 42,
            len: 99,
        },
        index: 12,
    };
    let encoded = location.encode();
    assert_eq!(TxLocation::decode(&encoded), Some(location));
}

#[test]
fn tx_index_roundtrip() {
    let store = Arc::new(MemoryStore::new());
    let index = TxIndex::new(Arc::clone(&store));
    let txid = [0x22; 32];
    let location = TxLocation {
        block: FileLocation {
            file_id: 1,
            offset: 128,
            len: 256,
        },
        index: 2,
    };

    let mut batch = WriteBatch::new();
    index.insert(&mut batch, &txid, location);
    store.write_batch(batch).expect("commit");

    let fetched = index.get(&txid).expect("get").expect("missing");
    assert_eq!(fetched, location);
}
