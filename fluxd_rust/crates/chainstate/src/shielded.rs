use core2::io;
use std::io::Cursor;

use fluxd_consensus::Hash256;
use incrementalmerkletree::{frontier::CommitmentTree, Hashable, Level};
use sapling_crypto::note::ExtractedNoteCommitment;
use sapling_crypto::{
    Anchor as SaplingAnchor, CommitmentTree as SaplingCommitmentTree, Node as SaplingNode,
};
use sha2::compress256;
use sha2::digest::generic_array::GenericArray;
use zcash_primitives::merkle_tree::{read_commitment_tree, write_commitment_tree, HashSer};

pub const SPROUT_TREE_DEPTH: u8 = 29;
const SHA256_IV: [u32; 8] = [
    0x6a09e667, 0xbb67ae85, 0x3c6ef372, 0xa54ff53a, 0x510e527f, 0x9b05688c, 0x1f83d9ab, 0x5be0cd19,
];

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct SproutNode([u8; 32]);

impl SproutNode {
    pub fn from_hash(hash: &Hash256) -> Self {
        Self(hash256_le_bytes(hash))
    }

    pub fn to_hash(self) -> Hash256 {
        hash256_from_le_bytes(&self.0)
    }
}

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

pub type SproutTree = CommitmentTree<SproutNode, SPROUT_TREE_DEPTH>;
pub type SaplingTree = SaplingCommitmentTree;

pub fn empty_sprout_tree() -> SproutTree {
    SproutTree::empty()
}

pub fn empty_sapling_tree() -> SaplingTree {
    SaplingTree::empty()
}

pub fn sprout_root_hash(tree: &SproutTree) -> Hash256 {
    tree.root().to_hash()
}

pub fn sapling_root_hash(tree: &SaplingTree) -> Hash256 {
    hash256_from_le_bytes(&tree.root().to_bytes())
}

pub fn sprout_empty_root_hash() -> Hash256 {
    SproutNode::empty_root(Level::from(SPROUT_TREE_DEPTH)).to_hash()
}

pub fn sapling_empty_root_hash() -> Hash256 {
    hash256_from_le_bytes(&SaplingAnchor::empty_tree().to_bytes())
}

pub fn sprout_tree_from_bytes(bytes: &[u8]) -> io::Result<SproutTree> {
    read_commitment_tree(Cursor::new(bytes))
}

pub fn sprout_tree_to_bytes(tree: &SproutTree) -> io::Result<Vec<u8>> {
    let mut out = Vec::new();
    write_commitment_tree(tree, &mut out)?;
    Ok(out)
}

pub fn sapling_tree_from_bytes(bytes: &[u8]) -> io::Result<SaplingTree> {
    read_commitment_tree(Cursor::new(bytes))
}

pub fn sapling_tree_to_bytes(tree: &SaplingTree) -> io::Result<Vec<u8>> {
    let mut out = Vec::new();
    write_commitment_tree(tree, &mut out)?;
    Ok(out)
}

pub fn sapling_node_from_hash(hash: &Hash256) -> Option<SaplingNode> {
    let bytes = hash256_le_bytes(hash);
    let cmu = Option::from(ExtractedNoteCommitment::from_bytes(&bytes))?;
    Some(SaplingNode::from_cmu(&cmu))
}

pub fn hash256_le_bytes(hash: &Hash256) -> [u8; 32] {
    *hash
}

pub fn hash256_from_le_bytes(bytes: &[u8; 32]) -> Hash256 {
    *bytes
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
