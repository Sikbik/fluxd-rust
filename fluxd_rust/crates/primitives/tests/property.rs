use fluxd_consensus::Hash256;
use fluxd_primitives::encoding::{DecodeError, Decoder, Encoder};
use fluxd_primitives::outpoint::OutPoint;
use fluxd_primitives::transaction::{
    JoinSplit, OutputDescription, SpendDescription, SproutProof, Transaction, TxIn, TxOut,
    GROTH_PROOF_SIZE, OVERWINTER_VERSION_GROUP_ID, PHGR_PROOF_SIZE, SAPLING_ENC_CIPHERTEXT_SIZE,
    SAPLING_OUT_CIPHERTEXT_SIZE, SAPLING_VERSION_GROUP_ID, ZC_NOTE_CIPHERTEXT_SIZE,
};

const MAX_COMPACT_SIZE: u64 = 0x0200_0000;

struct Lcg {
    state: u64,
}

impl Lcg {
    fn new(seed: u64) -> Self {
        Self { state: seed }
    }

    fn next_u64(&mut self) -> u64 {
        self.state = self
            .state
            .wrapping_mul(6364136223846793005)
            .wrapping_add(1);
        self.state
    }

    fn next_u32(&mut self) -> u32 {
        self.next_u64() as u32
    }

    fn next_u8(&mut self) -> u8 {
        self.next_u64() as u8
    }

    fn gen_range(&mut self, max: usize) -> usize {
        if max == 0 {
            0
        } else {
            (self.next_u64() % max as u64) as usize
        }
    }
}

fn fill_bytes<const N: usize>(rng: &mut Lcg) -> [u8; N] {
    std::array::from_fn(|_| rng.next_u8())
}

fn random_hash(rng: &mut Lcg) -> Hash256 {
    fill_bytes::<32>(rng)
}

fn random_vec(rng: &mut Lcg, max_len: usize) -> Vec<u8> {
    let len = rng.gen_range(max_len + 1);
    let mut bytes = Vec::with_capacity(len);
    for _ in 0..len {
        bytes.push(rng.next_u8());
    }
    bytes
}

fn random_spend(rng: &mut Lcg) -> SpendDescription {
    SpendDescription {
        cv: random_hash(rng),
        anchor: random_hash(rng),
        nullifier: random_hash(rng),
        rk: random_hash(rng),
        zkproof: fill_bytes::<GROTH_PROOF_SIZE>(rng),
        spend_auth_sig: fill_bytes::<64>(rng),
    }
}

fn random_output(rng: &mut Lcg) -> OutputDescription {
    OutputDescription {
        cv: random_hash(rng),
        cm: random_hash(rng),
        ephemeral_key: random_hash(rng),
        enc_ciphertext: fill_bytes::<SAPLING_ENC_CIPHERTEXT_SIZE>(rng),
        out_ciphertext: fill_bytes::<SAPLING_OUT_CIPHERTEXT_SIZE>(rng),
        zkproof: fill_bytes::<GROTH_PROOF_SIZE>(rng),
    }
}

fn random_join_split(rng: &mut Lcg, use_groth: bool) -> JoinSplit {
    let proof = if use_groth {
        SproutProof::Groth(fill_bytes::<GROTH_PROOF_SIZE>(rng))
    } else {
        SproutProof::Phgr(fill_bytes::<PHGR_PROOF_SIZE>(rng))
    };
    JoinSplit {
        vpub_old: rng.next_u32() as i64,
        vpub_new: rng.next_u32() as i64,
        anchor: random_hash(rng),
        nullifiers: std::array::from_fn(|_| random_hash(rng)),
        commitments: std::array::from_fn(|_| random_hash(rng)),
        ephemeral_key: random_hash(rng),
        random_seed: random_hash(rng),
        macs: std::array::from_fn(|_| random_hash(rng)),
        proof,
        ciphertexts: std::array::from_fn(|_| fill_bytes::<ZC_NOTE_CIPHERTEXT_SIZE>(rng)),
    }
}

fn random_transaction(rng: &mut Lcg) -> Transaction {
    let mode = rng.gen_range(4);
    let (f_overwintered, version, version_group_id) = match mode {
        0 => (false, 1, 0),
        1 => (false, 2, 0),
        2 => (true, 3, OVERWINTER_VERSION_GROUP_ID),
        _ => (true, 4, SAPLING_VERSION_GROUP_ID),
    };

    let vin = (0..rng.gen_range(3))
        .map(|_| TxIn {
            prevout: OutPoint {
                hash: random_hash(rng),
                index: rng.next_u32(),
            },
            script_sig: random_vec(rng, 16),
            sequence: rng.next_u32(),
        })
        .collect();

    let vout = (0..rng.gen_range(3))
        .map(|_| TxOut {
            value: rng.next_u32() as i64,
            script_pubkey: random_vec(rng, 16),
        })
        .collect();

    let lock_time = rng.next_u32();
    let expiry_height = if f_overwintered { rng.next_u32() } else { 0 };

    let (value_balance, shielded_spends, shielded_outputs) = if version == 4 {
        let spend_count = rng.gen_range(2);
        let output_count = rng.gen_range(2);
        let shielded_spends = (0..spend_count).map(|_| random_spend(rng)).collect();
        let shielded_outputs = (0..output_count).map(|_| random_output(rng)).collect();
        let value_balance = (rng.next_u32() as i64 % 2000) - 1000;
        (value_balance, shielded_spends, shielded_outputs)
    } else {
        (0, Vec::new(), Vec::new())
    };

    let join_split_count = if version >= 2 { rng.gen_range(2) } else { 0 };
    let use_groth = f_overwintered && version >= 4;
    let join_splits = (0..join_split_count)
        .map(|_| random_join_split(rng, use_groth))
        .collect::<Vec<_>>();
    let join_split_pub_key = if join_splits.is_empty() {
        [0u8; 32]
    } else {
        fill_bytes::<32>(rng)
    };
    let join_split_sig = if join_splits.is_empty() {
        [0u8; 64]
    } else {
        fill_bytes::<64>(rng)
    };
    let binding_sig = if version == 4
        && !(shielded_spends.is_empty() && shielded_outputs.is_empty())
    {
        fill_bytes::<64>(rng)
    } else {
        [0u8; 64]
    };

    Transaction {
        f_overwintered,
        version,
        version_group_id,
        vin,
        vout,
        lock_time,
        expiry_height,
        value_balance,
        shielded_spends,
        shielded_outputs,
        join_splits,
        join_split_pub_key,
        join_split_sig,
        binding_sig,
        fluxnode: None,
    }
}

#[test]
fn compactsize_roundtrip_random() {
    let mut rng = Lcg::new(0x5eed);
    for _ in 0..1_000 {
        let value = rng.next_u64() % MAX_COMPACT_SIZE;
        let mut encoder = Encoder::new();
        encoder.write_varint(value);
        let bytes = encoder.into_inner();
        let mut decoder = Decoder::new(&bytes);
        let decoded = decoder.read_varint().expect("decode compactsize");
        assert_eq!(decoded, value);
        assert!(decoder.is_empty());
    }
}

#[test]
fn compactsize_rejects_noncanonical() {
    let cases = [
        vec![0xfd, 0xfc, 0x00],
        vec![0xfe, 0xff, 0x00, 0x00, 0x00],
        vec![0xff, 0x01, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00],
    ];
    for bytes in cases {
        let mut decoder = Decoder::new(&bytes);
        let err = decoder.read_varint().expect_err("noncanonical compactsize");
        assert_eq!(err, DecodeError::NonCanonicalVarInt);
    }
}

#[test]
fn compactsize_rejects_oversized() {
    let bytes = [0xfe, 0x01, 0x00, 0x00, 0x02];
    let mut decoder = Decoder::new(&bytes);
    let err = decoder.read_varint().expect_err("oversized compactsize");
    assert_eq!(err, DecodeError::SizeTooLarge);
}

#[test]
fn randomized_transaction_roundtrip() {
    let mut rng = Lcg::new(0x1234_5678);
    for _ in 0..200 {
        let tx = random_transaction(&mut rng);
        let encoded = tx.consensus_encode().expect("encode random tx");
        let decoded = Transaction::consensus_decode(&encoded).expect("decode random tx");
        assert_eq!(decoded, tx);
    }
}
