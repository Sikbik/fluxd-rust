use fluxd_consensus::Hash256;
use fluxd_primitives::block::{BlockHeader, PON_VERSION};
use fluxd_primitives::outpoint::OutPoint;
use fluxd_primitives::transaction::{
    FluxnodeStartV5, FluxnodeTx, FluxnodeTxV5, JoinSplit, OutputDescription, SpendDescription,
    SproutProof, Transaction, TxIn, TxOut, FLUXNODE_TX_VERSION, GROTH_PROOF_SIZE, PHGR_PROOF_SIZE,
    SAPLING_ENC_CIPHERTEXT_SIZE, SAPLING_OUT_CIPHERTEXT_SIZE, SAPLING_VERSION_GROUP_ID,
    ZC_NOTE_CIPHERTEXT_SIZE,
};

fn seq_array<const N: usize>(start: u8) -> [u8; N] {
    std::array::from_fn(|i| start.wrapping_add(i as u8))
}

fn seq_hash(start: u8) -> Hash256 {
    seq_array::<32>(start)
}

fn push_hash_le(buffer: &mut Vec<u8>, start: u8) {
    for byte in 0u8..=0x1f {
        buffer.push(start.wrapping_add(byte));
    }
}

#[test]
fn serialize_block_header_pow() {
    let header = BlockHeader {
        version: 4,
        prev_block: seq_hash(0x00),
        merkle_root: seq_hash(0x20),
        final_sapling_root: seq_hash(0x40),
        time: 0x01020304,
        bits: 0x0a0b0c0d,
        nonce: seq_hash(0x60),
        solution: vec![0xaa, 0xbb, 0xcc],
        nodes_collateral: OutPoint::null(),
        block_sig: Vec::new(),
    };

    let encoded = header.consensus_encode();
    let mut expected = Vec::new();
    expected.extend_from_slice(&4i32.to_le_bytes());
    push_hash_le(&mut expected, 0x00);
    push_hash_le(&mut expected, 0x20);
    push_hash_le(&mut expected, 0x40);
    expected.extend_from_slice(&0x01020304u32.to_le_bytes());
    expected.extend_from_slice(&0x0a0b0c0du32.to_le_bytes());
    push_hash_le(&mut expected, 0x60);
    expected.push(3);
    expected.extend_from_slice(&[0xaa, 0xbb, 0xcc]);

    assert_eq!(encoded, expected);

    let decoded = BlockHeader::consensus_decode(&encoded).expect("decode pow header");
    assert_eq!(decoded, header);
}

#[test]
fn serialize_block_header_pon() {
    let header = BlockHeader {
        version: PON_VERSION,
        prev_block: seq_hash(0x00),
        merkle_root: seq_hash(0x20),
        final_sapling_root: seq_hash(0x40),
        time: 0x0f0e0d0c,
        bits: 0x01020304,
        nonce: [0u8; 32],
        solution: Vec::new(),
        nodes_collateral: OutPoint {
            hash: seq_hash(0x80),
            index: 5,
        },
        block_sig: vec![0x99, 0x88],
    };

    let encoded = header.consensus_encode();
    let mut expected = Vec::new();
    expected.extend_from_slice(&(PON_VERSION as i32).to_le_bytes());
    push_hash_le(&mut expected, 0x00);
    push_hash_le(&mut expected, 0x20);
    push_hash_le(&mut expected, 0x40);
    expected.extend_from_slice(&0x0f0e0d0cu32.to_le_bytes());
    expected.extend_from_slice(&0x01020304u32.to_le_bytes());
    push_hash_le(&mut expected, 0x80);
    expected.extend_from_slice(&5u32.to_le_bytes());
    expected.push(2);
    expected.extend_from_slice(&[0x99, 0x88]);

    assert_eq!(encoded, expected);

    let decoded = BlockHeader::consensus_decode(&encoded).expect("decode pon header");
    assert_eq!(decoded, header);
}

#[test]
fn serialize_transaction_v1() {
    let tx = Transaction {
        f_overwintered: false,
        version: 1,
        version_group_id: 0,
        vin: vec![TxIn {
            prevout: OutPoint {
                hash: seq_hash(0x10),
                index: 1,
            },
            script_sig: vec![0x51],
            sequence: 0xffff_ffff,
        }],
        vout: vec![TxOut {
            value: 50,
            script_pubkey: vec![0x51],
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

    let encoded = tx.consensus_encode().expect("encode tx");
    let mut expected = Vec::new();
    expected.extend_from_slice(&1u32.to_le_bytes());
    expected.push(1);
    push_hash_le(&mut expected, 0x10);
    expected.extend_from_slice(&1u32.to_le_bytes());
    expected.push(1);
    expected.push(0x51);
    expected.extend_from_slice(&0xffff_ffffu32.to_le_bytes());
    expected.push(1);
    expected.extend_from_slice(&50i64.to_le_bytes());
    expected.push(1);
    expected.push(0x51);
    expected.extend_from_slice(&0u32.to_le_bytes());

    assert_eq!(encoded, expected);

    let decoded = Transaction::consensus_decode(&encoded).expect("decode v1 tx");
    assert_eq!(decoded, tx);
}

#[test]
fn serialize_fluxnode_v5_start() {
    let tx = Transaction {
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
            collateral: OutPoint {
                hash: seq_hash(0x00),
                index: 2,
            },
            collateral_pubkey: vec![0x02, 0x03],
            pubkey: vec![0x04, 0x05, 0x06],
            sig_time: 0x0a0b0c0d,
            sig: vec![0xaa, 0xbb],
        }))),
    };

    let encoded = tx.consensus_encode().expect("encode fluxnode tx");
    let mut expected = Vec::new();
    expected.extend_from_slice(&(FLUXNODE_TX_VERSION as u32).to_le_bytes());
    expected.push(2);
    push_hash_le(&mut expected, 0x00);
    expected.extend_from_slice(&2u32.to_le_bytes());
    expected.push(2);
    expected.extend_from_slice(&[0x02, 0x03]);
    expected.push(3);
    expected.extend_from_slice(&[0x04, 0x05, 0x06]);
    expected.extend_from_slice(&0x0a0b0c0du32.to_le_bytes());
    expected.push(2);
    expected.extend_from_slice(&[0xaa, 0xbb]);

    assert_eq!(encoded, expected);

    let decoded = Transaction::consensus_decode(&encoded).expect("decode fluxnode v5 tx");
    assert_eq!(decoded, tx);
}

#[test]
fn roundtrip_join_split_phgr() {
    let join_split = JoinSplit {
        vpub_old: 5,
        vpub_new: 0,
        anchor: seq_hash(0x01),
        nullifiers: [seq_hash(0x10), seq_hash(0x20)],
        commitments: [seq_hash(0x30), seq_hash(0x40)],
        ephemeral_key: seq_hash(0x50),
        random_seed: seq_hash(0x60),
        macs: [seq_hash(0x70), seq_hash(0x80)],
        proof: SproutProof::Phgr(seq_array::<PHGR_PROOF_SIZE>(0x90)),
        ciphertexts: [
            seq_array::<ZC_NOTE_CIPHERTEXT_SIZE>(0xa0),
            seq_array::<ZC_NOTE_CIPHERTEXT_SIZE>(0xb0),
        ],
    };

    let tx = Transaction {
        f_overwintered: false,
        version: 2,
        version_group_id: 0,
        vin: Vec::new(),
        vout: Vec::new(),
        lock_time: 0,
        expiry_height: 0,
        value_balance: 0,
        shielded_spends: Vec::new(),
        shielded_outputs: Vec::new(),
        join_splits: vec![join_split],
        join_split_pub_key: seq_array::<32>(0xc0),
        join_split_sig: seq_array::<64>(0xd0),
        binding_sig: [0u8; 64],
        fluxnode: None,
    };

    let encoded = tx.consensus_encode().expect("encode joinsplit tx");
    let decoded = Transaction::consensus_decode(&encoded).expect("decode joinsplit tx");
    assert_eq!(decoded, tx);
}

#[test]
fn roundtrip_sapling_v4() {
    let spend = SpendDescription {
        cv: seq_hash(0x01),
        anchor: seq_hash(0x02),
        nullifier: seq_hash(0x03),
        rk: seq_hash(0x04),
        zkproof: seq_array::<GROTH_PROOF_SIZE>(0x05),
        spend_auth_sig: seq_array::<64>(0x06),
    };

    let output = OutputDescription {
        cv: seq_hash(0x11),
        cm: seq_hash(0x12),
        ephemeral_key: seq_hash(0x13),
        enc_ciphertext: seq_array::<SAPLING_ENC_CIPHERTEXT_SIZE>(0x14),
        out_ciphertext: seq_array::<SAPLING_OUT_CIPHERTEXT_SIZE>(0x15),
        zkproof: seq_array::<GROTH_PROOF_SIZE>(0x16),
    };

    let join_split = JoinSplit {
        vpub_old: 1,
        vpub_new: 2,
        anchor: seq_hash(0x21),
        nullifiers: [seq_hash(0x22), seq_hash(0x23)],
        commitments: [seq_hash(0x24), seq_hash(0x25)],
        ephemeral_key: seq_hash(0x26),
        random_seed: seq_hash(0x27),
        macs: [seq_hash(0x28), seq_hash(0x29)],
        proof: SproutProof::Groth(seq_array::<GROTH_PROOF_SIZE>(0x2a)),
        ciphertexts: [
            seq_array::<ZC_NOTE_CIPHERTEXT_SIZE>(0x2b),
            seq_array::<ZC_NOTE_CIPHERTEXT_SIZE>(0x2c),
        ],
    };

    let tx = Transaction {
        f_overwintered: true,
        version: 4,
        version_group_id: SAPLING_VERSION_GROUP_ID,
        vin: Vec::new(),
        vout: Vec::new(),
        lock_time: 0,
        expiry_height: 42,
        value_balance: -10,
        shielded_spends: vec![spend],
        shielded_outputs: vec![output],
        join_splits: vec![join_split],
        join_split_pub_key: seq_array::<32>(0x2d),
        join_split_sig: seq_array::<64>(0x2e),
        binding_sig: seq_array::<64>(0x2f),
        fluxnode: None,
    };

    let encoded = tx.consensus_encode().expect("encode sapling tx");
    let decoded = Transaction::consensus_decode(&encoded).expect("decode sapling tx");
    assert_eq!(decoded, tx);
}
