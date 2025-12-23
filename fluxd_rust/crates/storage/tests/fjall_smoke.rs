#![cfg(feature = "fjall")]

use std::collections::HashSet;
use std::time::{SystemTime, UNIX_EPOCH};

use fluxd_storage::fjall::FjallStore;
use fluxd_storage::{Column, KeyValueStore, WriteBatch};

#[test]
fn fjall_smoke_roundtrip() {
    let mut dir = std::env::temp_dir();
    let nanos = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .expect("clock")
        .as_nanos();
    dir.push(format!("fluxd_fjall_smoke_{nanos}"));

    let store = FjallStore::open(&dir).expect("open fjall");
    store
        .put(Column::Meta, b"key", b"value")
        .expect("put");
    assert_eq!(
        store.get(Column::Meta, b"key").expect("get"),
        Some(b"value".to_vec())
    );

    store
        .put(Column::Meta, b"prefix:1", b"a")
        .expect("put prefix");
    store
        .put(Column::Meta, b"prefix:2", b"b")
        .expect("put prefix");
    let mut keys = HashSet::new();
    for (key, value) in store
        .scan_prefix(Column::Meta, b"prefix:")
        .expect("scan")
    {
        keys.insert((key, value));
    }
    assert_eq!(
        keys,
        HashSet::from([
            (b"prefix:1".to_vec(), b"a".to_vec()),
            (b"prefix:2".to_vec(), b"b".to_vec()),
        ])
    );

    let mut batch = WriteBatch::new();
    batch.put(Column::Meta, b"batch", b"ok");
    batch.delete(Column::Meta, b"key");
    store.write_batch(batch).expect("batch commit");

    assert!(store.get(Column::Meta, b"key").expect("get").is_none());
    assert_eq!(
        store.get(Column::Meta, b"batch").expect("get"),
        Some(b"ok".to_vec())
    );

    drop(store);
    let _ = std::fs::remove_dir_all(&dir);
}
