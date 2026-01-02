use std::sync::Arc;

use fluxd_chainstate::flatfiles::FlatFileStore;
use fluxd_chainstate::state::ChainState;
use fluxd_chainstate::validation::ValidationFlags;
use fluxd_consensus::params::{chain_params, Checkpoint, Network};
use fluxd_consensus::upgrades::UpgradeIndex;
use fluxd_pow::difficulty::target_to_compact;
use fluxd_primitives::block::{Block, BlockHeader, CURRENT_VERSION};
use fluxd_primitives::outpoint::OutPoint;
use fluxd_primitives::transaction::{Transaction, TxIn, TxOut};
use fluxd_storage::memory::MemoryStore;
use fluxd_storage::WriteBatch;

fn p2pkh_script(tag: u8) -> Vec<u8> {
    let mut script = Vec::with_capacity(25);
    script.extend_from_slice(&[0x76, 0xa9, 0x14]);
    script.extend_from_slice(&[tag; 20]);
    script.extend_from_slice(&[0x88, 0xac]);
    script
}

fn make_tx(vin: Vec<TxIn>, vout: Vec<TxOut>) -> Transaction {
    Transaction {
        f_overwintered: false,
        version: 1,
        version_group_id: 0,
        vin,
        vout,
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
    }
}

fn coinbase_tx(height: u32) -> Transaction {
    make_tx(
        vec![TxIn {
            prevout: OutPoint::null(),
            script_sig: height.to_le_bytes().to_vec(),
            sequence: u32::MAX,
        }],
        vec![TxOut {
            value: 0,
            script_pubkey: vec![0x51],
        }],
    )
}

fn make_header(prev_block: [u8; 32], time: u32, bits: u32, nonce_tag: u8) -> BlockHeader {
    BlockHeader {
        version: CURRENT_VERSION,
        prev_block,
        merkle_root: [0u8; 32],
        final_sapling_root: [0u8; 32],
        time,
        bits,
        nonce: [nonce_tag; 32],
        solution: Vec::new(),
        nodes_collateral: OutPoint::null(),
        block_sig: Vec::new(),
    }
}

#[test]
fn reorg_reverts_utxo_spent_and_address_indexes() {
    let store = Arc::new(MemoryStore::new());
    let dir = tempfile::tempdir().expect("tempdir");
    let blocks = FlatFileStore::new(dir.path(), 10_000_000).expect("flatfiles");
    let undo = FlatFileStore::new_with_prefix(dir.path(), "undo", 10_000_000).expect("flatfiles");
    let chainstate = ChainState::new(Arc::clone(&store), blocks, undo);

    let mut params = chain_params(Network::Regtest);
    params.funding.exchange_height = i64::MAX;
    params.funding.foundation_height = i64::MAX;
    params.swap_pool.start_height = i64::MAX;
    params.fluxnode.start_payments_height = i64::MAX;
    params.consensus.digishield_averaging_window = 10_000;
    params.consensus.upgrades[UpgradeIndex::Lwma.as_usize()].activation_height = i32::MAX;
    params.consensus.upgrades[UpgradeIndex::Equi144_5.as_usize()].activation_height = i32::MAX;
    params.consensus.upgrades[UpgradeIndex::Acadia.as_usize()].activation_height = i32::MAX;
    params.consensus.upgrades[UpgradeIndex::Kamiooka.as_usize()].activation_height = i32::MAX;

    let now = 1_700_000_000u32;
    let bits = target_to_compact(&params.consensus.pow_limit);

    let mut headers = Vec::new();
    let header0 = make_header([0u8; 32], now, bits, 0);
    let hash0 = header0.hash();
    params.consensus.hash_genesis_block = hash0;
    params.consensus.checkpoints = vec![Checkpoint {
        height: 0,
        hash: hash0,
    }];
    headers.push(header0.clone());

    let mut prev = hash0;
    for height in 1u32..=101u32 {
        let header = make_header(prev, now + height, bits, (height & 0xff) as u8);
        prev = header.hash();
        headers.push(header);
    }
    let hash101 = prev;

    let header102a = make_header(hash101, now + 102, bits, 0xaa);
    let hash102a = header102a.hash();
    let header102b = make_header(hash101, now + 102, bits, 0xbb);
    let hash102b = header102b.hash();
    let header103a = make_header(hash102a, now + 103, bits, 0xca);
    let hash103a = header103a.hash();
    let header103b = make_header(hash102b, now + 103, bits, 0xcb);

    headers.extend_from_slice(&[
        header102a.clone(),
        header102b.clone(),
        header103a.clone(),
        header103b.clone(),
    ]);

    let mut header_batch = WriteBatch::new();
    chainstate
        .insert_headers_batch_with_pow(&headers, &params.consensus, &mut header_batch, false)
        .expect("insert headers");
    chainstate
        .commit_batch(header_batch)
        .expect("commit headers");

    let flags = ValidationFlags::default();

    let coinbase0 = coinbase_tx(0);
    let coinbase0_txid = coinbase0.txid().expect("coinbase txid");

    for height in 0u32..=100u32 {
        let coinbase = if height == 0 {
            coinbase0.clone()
        } else {
            coinbase_tx(height)
        };
        let header = headers[height as usize].clone();
        let block = Block {
            header,
            transactions: vec![coinbase],
        };
        let batch = chainstate
            .connect_block(
                &block,
                height as i32,
                &params,
                &flags,
                true,
                None,
                None,
                None,
                None,
            )
            .expect("connect block");
        chainstate.commit_batch(batch).expect("commit block");
    }

    let script_a = p2pkh_script(0x11);
    let script_b = p2pkh_script(0x22);
    let script_c = p2pkh_script(0x33);

    let fund_tx = make_tx(
        vec![TxIn {
            prevout: OutPoint {
                hash: coinbase0_txid,
                index: 0,
            },
            script_sig: vec![0x01],
            sequence: u32::MAX,
        }],
        vec![TxOut {
            value: 0,
            script_pubkey: script_a.clone(),
        }],
    );
    let fund_txid = fund_tx.txid().expect("fund txid");
    let fund_outpoint = OutPoint {
        hash: fund_txid,
        index: 0,
    };

    let block101 = Block {
        header: headers[101].clone(),
        transactions: vec![coinbase_tx(101), fund_tx],
    };
    let batch = chainstate
        .connect_block(
            &block101, 101, &params, &flags, true, None, None, None, None,
        )
        .expect("connect block 101");
    chainstate.commit_batch(batch).expect("commit 101");

    assert_eq!(
        chainstate
            .utxo_entry(&fund_outpoint)
            .expect("utxo")
            .is_some(),
        true
    );
    assert_eq!(
        chainstate
            .address_outpoints(&script_a)
            .expect("address outpoints"),
        vec![fund_outpoint.clone()]
    );

    let spend_a = make_tx(
        vec![TxIn {
            prevout: fund_outpoint.clone(),
            script_sig: vec![0x02],
            sequence: u32::MAX,
        }],
        vec![TxOut {
            value: 0,
            script_pubkey: script_b.clone(),
        }],
    );
    let spend_a_txid = spend_a.txid().expect("spend a txid");
    let spend_a_outpoint = OutPoint {
        hash: spend_a_txid,
        index: 0,
    };
    let block102a = Block {
        header: header102a,
        transactions: vec![coinbase_tx(102), spend_a],
    };
    let batch = chainstate
        .connect_block(
            &block102a, 102, &params, &flags, true, None, None, None, None,
        )
        .expect("connect 102a");
    chainstate.commit_batch(batch).expect("commit 102a");

    let batch = chainstate
        .connect_block(
            &Block {
                header: header103a,
                transactions: vec![coinbase_tx(103)],
            },
            103,
            &params,
            &flags,
            true,
            None,
            None,
            None,
            None,
        )
        .expect("connect 103a");
    chainstate.commit_batch(batch).expect("commit 103a");

    assert_eq!(
        chainstate
            .utxo_entry(&fund_outpoint)
            .expect("utxo")
            .is_some(),
        false
    );
    assert_eq!(
        chainstate
            .address_outpoints(&script_a)
            .expect("address outpoints"),
        Vec::<OutPoint>::new()
    );
    assert_eq!(
        chainstate
            .address_outpoints(&script_b)
            .expect("address outpoints"),
        vec![spend_a_outpoint.clone()]
    );
    assert_eq!(
        chainstate
            .address_outpoints(&script_c)
            .expect("address outpoints"),
        Vec::<OutPoint>::new()
    );

    let spent = chainstate
        .spent_info(&fund_outpoint)
        .expect("spent index")
        .expect("missing spent entry");
    assert_eq!(spent.txid, spend_a_txid);
    assert_eq!(spent.block_height, 102);

    let deltas_a = chainstate.address_deltas(&script_a).expect("deltas");
    assert!(deltas_a
        .iter()
        .any(|delta| delta.height == 101 && !delta.spending && delta.txid == fund_txid));
    assert!(deltas_a
        .iter()
        .any(|delta| delta.height == 102 && delta.spending && delta.txid == spend_a_txid));

    let deltas_b = chainstate.address_deltas(&script_b).expect("deltas");
    assert!(deltas_b
        .iter()
        .any(|delta| delta.height == 102 && !delta.spending && delta.txid == spend_a_txid));

    let batch = chainstate
        .disconnect_block(&hash103a)
        .expect("disconnect 103a");
    chainstate
        .commit_batch(batch)
        .expect("commit disconnect 103a");
    let batch = chainstate
        .disconnect_block(&hash102a)
        .expect("disconnect 102a");
    chainstate
        .commit_batch(batch)
        .expect("commit disconnect 102a");

    assert_eq!(
        chainstate
            .utxo_entry(&fund_outpoint)
            .expect("utxo")
            .is_some(),
        true
    );
    assert_eq!(
        chainstate.spent_info(&fund_outpoint).expect("spent index"),
        None
    );
    assert_eq!(
        chainstate
            .address_outpoints(&script_a)
            .expect("address outpoints"),
        vec![fund_outpoint.clone()]
    );
    assert_eq!(
        chainstate
            .address_outpoints(&script_b)
            .expect("address outpoints"),
        Vec::<OutPoint>::new()
    );

    let spend_b = make_tx(
        vec![TxIn {
            prevout: fund_outpoint.clone(),
            script_sig: vec![0x03],
            sequence: u32::MAX,
        }],
        vec![TxOut {
            value: 0,
            script_pubkey: script_c.clone(),
        }],
    );
    let spend_b_txid = spend_b.txid().expect("spend b txid");
    let spend_b_outpoint = OutPoint {
        hash: spend_b_txid,
        index: 0,
    };
    let block102b = Block {
        header: header102b,
        transactions: vec![coinbase_tx(102), spend_b],
    };
    let batch = chainstate
        .connect_block(
            &block102b, 102, &params, &flags, true, None, None, None, None,
        )
        .expect("connect 102b");
    chainstate.commit_batch(batch).expect("commit 102b");

    let batch = chainstate
        .connect_block(
            &Block {
                header: header103b,
                transactions: vec![coinbase_tx(103)],
            },
            103,
            &params,
            &flags,
            true,
            None,
            None,
            None,
            None,
        )
        .expect("connect 103b");
    chainstate.commit_batch(batch).expect("commit 103b");

    assert_eq!(
        chainstate
            .utxo_entry(&fund_outpoint)
            .expect("utxo")
            .is_some(),
        false
    );
    assert_eq!(
        chainstate
            .address_outpoints(&script_c)
            .expect("address outpoints"),
        vec![spend_b_outpoint]
    );
    assert_eq!(
        chainstate
            .address_outpoints(&script_b)
            .expect("address outpoints"),
        Vec::<OutPoint>::new()
    );

    let spent = chainstate
        .spent_info(&fund_outpoint)
        .expect("spent index")
        .expect("missing spent entry");
    assert_eq!(spent.txid, spend_b_txid);
    assert_eq!(spent.block_height, 102);

    let deltas_a = chainstate.address_deltas(&script_a).expect("deltas");
    assert!(deltas_a
        .iter()
        .any(|delta| delta.height == 102 && delta.spending && delta.txid == spend_b_txid));
    let deltas_c = chainstate.address_deltas(&script_c).expect("deltas");
    assert!(deltas_c
        .iter()
        .any(|delta| delta.height == 102 && !delta.spending && delta.txid == spend_b_txid));
}
