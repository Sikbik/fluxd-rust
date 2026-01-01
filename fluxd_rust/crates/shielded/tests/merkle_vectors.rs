use core2::io;

use fluxd_consensus::params::hash256_from_hex;
use incrementalmerkletree::{
    frontier::CommitmentTree, witness::IncrementalWitness, Hashable, Level, MerklePath,
};
use sapling_crypto::note::ExtractedNoteCommitment;
use sapling_crypto::Node as SaplingNode;
use sha2::compress256;
use sha2::digest::generic_array::GenericArray;
use zcash_primitives::merkle_tree::{
    merkle_path_from_slice, read_commitment_tree, read_incremental_witness, write_commitment_tree,
    write_incremental_witness, HashSer,
};

const SHA256_IV: [u32; 8] = [
    0x6a09e667, 0xbb67ae85, 0x3c6ef372, 0xa54ff53a, 0x510e527f, 0x9b05688c, 0x1f83d9ab, 0x5be0cd19,
];

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
struct SproutNode([u8; 32]);

impl Hashable for SproutNode {
    fn empty_leaf() -> Self {
        SproutNode([0u8; 32])
    }

    fn combine(level: Level, lhs: &Self, rhs: &Self) -> Self {
        let _ = level;
        SproutNode(sha256_compress(&lhs.0, &rhs.0))
    }
}

impl HashSer for SproutNode {
    fn read<R: core2::io::Read>(mut reader: R) -> io::Result<Self> {
        let mut bytes = [0u8; 32];
        reader.read_exact(&mut bytes)?;
        Ok(SproutNode(bytes))
    }

    fn write<W: core2::io::Write>(&self, mut writer: W) -> io::Result<()> {
        writer.write_all(&self.0)
    }
}

fn sha256_compress(left: &[u8; 32], right: &[u8; 32]) -> [u8; 32] {
    let mut block = [0u8; 64];
    block[..32].copy_from_slice(left);
    block[32..].copy_from_slice(right);

    let mut state: [u32; 8] = SHA256_IV;
    let block = GenericArray::clone_from_slice(&block);
    compress256(&mut state, &[block]);

    let mut out = [0u8; 32];
    for (i, word) in state.iter().enumerate() {
        out[i * 4..(i + 1) * 4].copy_from_slice(&word.to_be_bytes());
    }
    out
}

fn hex_to_bytes(hex: &str) -> Vec<u8> {
    let mut text = hex.trim();
    if let Some(stripped) = text.strip_prefix("0x").or_else(|| text.strip_prefix("0X")) {
        text = stripped;
    }
    assert!(text.len() % 2 == 0, "hex string has odd length: {text}");
    let mut bytes = Vec::with_capacity(text.len() / 2);
    for i in (0..text.len()).step_by(2) {
        let byte = u8::from_str_radix(&text[i..i + 2], 16).expect("valid hex");
        bytes.push(byte);
    }
    bytes
}

fn hex_to_array32(hex: &str) -> [u8; 32] {
    let bytes = hex_to_bytes(hex);
    assert_eq!(bytes.len(), 32, "expected 32 bytes, got {}", bytes.len());
    let mut out = [0u8; 32];
    out.copy_from_slice(&bytes);
    out
}

fn load_vector(path: &str) -> Vec<String> {
    let data = match path {
        "merkle_commitments.json" => include_str!("vectors/merkle_commitments.json"),
        "merkle_roots.json" => include_str!("vectors/merkle_roots.json"),
        "merkle_serialization.json" => include_str!("vectors/merkle_serialization.json"),
        "merkle_witness_serialization.json" => {
            include_str!("vectors/merkle_witness_serialization.json")
        }
        "merkle_path.json" => include_str!("vectors/merkle_path.json"),
        "merkle_commitments_sapling.json" => {
            include_str!("vectors/merkle_commitments_sapling.json")
        }
        "merkle_roots_sapling.json" => include_str!("vectors/merkle_roots_sapling.json"),
        "merkle_serialization_sapling.json" => {
            include_str!("vectors/merkle_serialization_sapling.json")
        }
        "merkle_witness_serialization_sapling.json" => {
            include_str!("vectors/merkle_witness_serialization_sapling.json")
        }
        "merkle_path_sapling.json" => include_str!("vectors/merkle_path_sapling.json"),
        other => panic!("unknown vector file {other}"),
    };
    serde_json::from_str(data).expect("parse JSON vector")
}

fn assert_tree_roundtrip<Node: HashSer + Clone + PartialEq + core::fmt::Debug, const DEPTH: u8>(
    expected_hex: &str,
    tree: &CommitmentTree<Node, DEPTH>,
) {
    let expected_bytes = hex_to_bytes(expected_hex);

    let mut encoded = Vec::new();
    write_commitment_tree(tree, &mut encoded).expect("encode tree");
    assert_eq!(encoded, expected_bytes);

    let decoded: CommitmentTree<Node, DEPTH> =
        read_commitment_tree(encoded.as_slice()).expect("decode tree");
    assert_eq!(decoded, *tree);

    let mut reencoded = Vec::new();
    write_commitment_tree(&decoded, &mut reencoded).expect("re-encode tree");
    assert_eq!(reencoded, expected_bytes);
}

fn assert_witness_roundtrip<Node: HashSer, const DEPTH: u8>(
    expected_hex: &str,
    witness: &IncrementalWitness<Node, DEPTH>,
) {
    let expected_bytes = hex_to_bytes(expected_hex);

    let mut encoded = Vec::new();
    write_incremental_witness(witness, &mut encoded).expect("encode witness");
    assert_eq!(encoded, expected_bytes);

    let decoded: IncrementalWitness<Node, DEPTH> =
        read_incremental_witness(encoded.as_slice()).expect("decode witness");
    let mut reencoded = Vec::new();
    write_incremental_witness(&decoded, &mut reencoded).expect("re-encode witness");
    assert_eq!(reencoded, expected_bytes);
}

fn run_merkle_vectors<
    Node: HashSer + Hashable + Clone + PartialEq + core::fmt::Debug,
    const DEPTH: u8,
>(
    commitments: &[Node],
    expected_roots: &[String],
    expected_tree_ser: &[String],
    expected_witness_ser: &[String],
    expected_paths: &[String],
) {
    assert_eq!(commitments.len(), 1usize << DEPTH);
    assert_eq!(expected_roots.len(), commitments.len());
    assert_eq!(expected_tree_ser.len(), commitments.len());

    let mut witness_ser_index = 0usize;
    let mut path_index = 0usize;
    let mut witnesses: Vec<IncrementalWitness<Node, DEPTH>> = Vec::new();

    let mut tree = CommitmentTree::<Node, DEPTH>::empty();
    assert!(tree.is_empty());

    for (i, commitment) in commitments.iter().enumerate() {
        if i == 0 {
            // C++ test vectors start by witnessing an empty tree, which the Rust API
            // does not allow directly. We reproduce the same state by constructing the
            // post-append witness equivalent: empty tree snapshot + filled leaves.
        } else {
            let witness = IncrementalWitness::from_tree(tree.clone()).expect("tree not empty");
            witnesses.push(witness);
        }

        tree.append(commitment.clone())
            .expect("append commitment into tree");
        assert_eq!(tree.size(), i + 1);

        let expected_root = hex_to_array32(&expected_roots[i]);
        let root_bytes = {
            let mut bytes = Vec::new();
            tree.root().write(&mut bytes).expect("root bytes");
            bytes
        };
        assert_eq!(root_bytes.as_slice(), expected_root.as_slice());

        assert_tree_roundtrip(&expected_tree_ser[i], &tree);

        if i == 0 {
            let witness0 = IncrementalWitness::from_parts(
                CommitmentTree::<Node, DEPTH>::empty(),
                vec![commitment.clone()],
                None,
            )
            .expect("construct witness for empty tree");
            witnesses.push(witness0);
        }

        for (witness_index, witness) in witnesses.iter_mut().enumerate() {
            if !(i == 0 && witness_index == 0) {
                witness
                    .append(commitment.clone())
                    .expect("append commitment into witness");
            }

            if witness_index == 0 {
                assert!(witness.path().is_none());
            } else {
                let path = witness.path().expect("witness has path");
                let expected_path_bytes = hex_to_bytes(&expected_paths[path_index]);
                let expected_path: MerklePath<Node, DEPTH> =
                    merkle_path_from_slice(expected_path_bytes.as_slice())
                        .expect("decode expected merkle path");
                assert_eq!(path, expected_path);
                path_index += 1;
            }

            assert_witness_roundtrip(&expected_witness_ser[witness_ser_index], witness);
            witness_ser_index += 1;

            assert_eq!(witness.root(), tree.root());
        }
    }

    assert_eq!(path_index, expected_paths.len());
    assert_eq!(witness_ser_index, expected_witness_ser.len());

    assert!(tree.append(commitments[0].clone()).is_err());
    for witness in &mut witnesses {
        assert!(witness.append(commitments[0].clone()).is_err());
    }
}

#[test]
fn merkle_vectors_match_cpp_sprout() {
    const DEPTH: u8 = 4;

    let commitment_hex = load_vector("merkle_commitments.json");
    let roots = load_vector("merkle_roots.json");
    let tree_ser = load_vector("merkle_serialization.json");
    let witness_ser = load_vector("merkle_witness_serialization.json");
    let paths = load_vector("merkle_path.json");

    let mut commitments = Vec::with_capacity(commitment_hex.len());
    for hex in commitment_hex {
        let hash = hash256_from_hex(&hex).expect("commitment hex");
        commitments.push(SproutNode(hash));
    }

    run_merkle_vectors::<SproutNode, DEPTH>(&commitments, &roots, &tree_ser, &witness_ser, &paths);
}

#[test]
fn merkle_vectors_match_cpp_sapling() {
    const DEPTH: u8 = 4;

    let commitment_hex = load_vector("merkle_commitments_sapling.json");
    let roots = load_vector("merkle_roots_sapling.json");
    let tree_ser = load_vector("merkle_serialization_sapling.json");
    let witness_ser = load_vector("merkle_witness_serialization_sapling.json");
    let paths = load_vector("merkle_path_sapling.json");

    let mut commitments = Vec::with_capacity(commitment_hex.len());
    for hex in commitment_hex {
        let hash = hash256_from_hex(&hex).expect("commitment hex");
        let cmu = Option::from(ExtractedNoteCommitment::from_bytes(&hash))
            .expect("canonical sapling note commitment");
        commitments.push(SaplingNode::from_cmu(&cmu));
    }

    run_merkle_vectors::<SaplingNode, DEPTH>(&commitments, &roots, &tree_ser, &witness_ser, &paths);
}
