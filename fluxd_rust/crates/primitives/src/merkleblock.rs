//! Merkle block proofs (gettxoutproof/verifytxoutproof).
//!
//! This module ports Flux/Bitcoin's `CMerkleBlock` and `CPartialMerkleTree`
//! data structures and serialization format.

use fluxd_consensus::constants::MAX_BLOCK_SIZE;
use fluxd_consensus::Hash256;

use crate::block::BlockHeader;
use crate::encoding::{decode, encode, Decodable, DecodeError, Decoder, Encodable, Encoder};
use crate::hash::sha256d;

const MIN_SERIALIZED_TX_SIZE: u32 = 60;

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct PartialMerkleTree {
    pub n_transactions: u32,
    pub bits: Vec<bool>,
    pub hashes: Vec<Hash256>,
}

#[derive(Clone, Debug, PartialEq)]
pub struct MerkleBlock {
    pub header: BlockHeader,
    pub txn: PartialMerkleTree,
}

impl MerkleBlock {
    pub fn consensus_encode(&self) -> Vec<u8> {
        encode(self)
    }

    pub fn consensus_decode(bytes: &[u8]) -> Result<Self, DecodeError> {
        decode(bytes)
    }
}

impl Encodable for MerkleBlock {
    fn consensus_encode(&self, encoder: &mut Encoder) {
        encoder.write_bytes(&self.header.consensus_encode());
        self.txn.consensus_encode(encoder);
    }
}

impl Decodable for MerkleBlock {
    fn consensus_decode(decoder: &mut Decoder) -> Result<Self, DecodeError> {
        let header = BlockHeader::consensus_decode_from(decoder, true)?;
        let txn = PartialMerkleTree::consensus_decode(decoder)?;
        Ok(Self { header, txn })
    }
}

impl PartialMerkleTree {
    pub fn from_txids(txids: &[Hash256], matches: &[bool]) -> Result<Self, DecodeError> {
        if txids.len() != matches.len() {
            return Err(DecodeError::InvalidData("txids/matches length mismatch"));
        }
        let n_transactions = u32::try_from(txids.len()).map_err(|_| DecodeError::SizeTooLarge)?;
        let mut tree = Self {
            n_transactions,
            bits: Vec::new(),
            hashes: Vec::new(),
        };

        let height = calc_tree_height(n_transactions);
        tree.traverse_and_build(height, 0, txids, matches);
        Ok(tree)
    }

    pub fn extract_matches(&self) -> Option<(Hash256, Vec<Hash256>)> {
        let mut matches_out = Vec::new();
        let root = self.extract_matches_into(&mut matches_out)?;
        Some((root, matches_out))
    }

    fn extract_matches_into(&self, matches_out: &mut Vec<Hash256>) -> Option<Hash256> {
        matches_out.clear();

        if self.n_transactions == 0 {
            return None;
        }

        if self.n_transactions > MAX_BLOCK_SIZE / MIN_SERIALIZED_TX_SIZE {
            return None;
        }

        if self.hashes.len() > self.n_transactions as usize {
            return None;
        }

        if self.bits.len() < self.hashes.len() {
            return None;
        }

        let height = calc_tree_height(self.n_transactions);
        let mut bits_used = 0usize;
        let mut hashes_used = 0usize;
        let mut bad = false;
        let root = self.traverse_and_extract(
            height,
            0,
            &mut bits_used,
            &mut hashes_used,
            matches_out,
            &mut bad,
        );

        if bad {
            return None;
        }

        if (bits_used + 7) / 8 != (self.bits.len() + 7) / 8 {
            return None;
        }

        if hashes_used != self.hashes.len() {
            return None;
        }

        Some(root)
    }

    fn traverse_and_build(&mut self, height: u32, pos: u32, txids: &[Hash256], matches: &[bool]) {
        let start = (pos as u64) << height;
        let end = ((pos as u64 + 1) << height).min(self.n_transactions as u64);
        let mut parent_of_match = false;
        for idx in start..end {
            parent_of_match |= matches[idx as usize];
            if parent_of_match {
                break;
            }
        }

        self.bits.push(parent_of_match);

        if height == 0 || !parent_of_match {
            self.hashes.push(self.calc_hash(height, pos, txids));
            return;
        }

        self.traverse_and_build(height - 1, pos.saturating_mul(2), txids, matches);
        if pos.saturating_mul(2).saturating_add(1)
            < calc_tree_width(self.n_transactions, height - 1)
        {
            self.traverse_and_build(
                height - 1,
                pos.saturating_mul(2).saturating_add(1),
                txids,
                matches,
            );
        }
    }

    fn traverse_and_extract(
        &self,
        height: u32,
        pos: u32,
        bits_used: &mut usize,
        hashes_used: &mut usize,
        matches_out: &mut Vec<Hash256>,
        bad: &mut bool,
    ) -> Hash256 {
        if *bits_used >= self.bits.len() {
            *bad = true;
            return [0u8; 32];
        }

        let parent_of_match = self.bits[*bits_used];
        *bits_used += 1;

        if height == 0 || !parent_of_match {
            if *hashes_used >= self.hashes.len() {
                *bad = true;
                return [0u8; 32];
            }
            let hash = self.hashes[*hashes_used];
            *hashes_used += 1;
            if height == 0 && parent_of_match {
                matches_out.push(hash);
            }
            return hash;
        }

        let left = self.traverse_and_extract(
            height - 1,
            pos.saturating_mul(2),
            bits_used,
            hashes_used,
            matches_out,
            bad,
        );

        let mut right = left;
        if pos.saturating_mul(2).saturating_add(1)
            < calc_tree_width(self.n_transactions, height - 1)
        {
            right = self.traverse_and_extract(
                height - 1,
                pos.saturating_mul(2).saturating_add(1),
                bits_used,
                hashes_used,
                matches_out,
                bad,
            );
            if right == left {
                *bad = true;
            }
        }

        merkle_hash_pair(&left, &right)
    }

    fn calc_hash(&self, height: u32, pos: u32, txids: &[Hash256]) -> Hash256 {
        if height == 0 {
            return txids[pos as usize];
        }

        let left = self.calc_hash(height - 1, pos.saturating_mul(2), txids);
        let right = if pos.saturating_mul(2).saturating_add(1)
            < calc_tree_width(self.n_transactions, height - 1)
        {
            self.calc_hash(height - 1, pos.saturating_mul(2).saturating_add(1), txids)
        } else {
            left
        };

        merkle_hash_pair(&left, &right)
    }
}

impl Encodable for PartialMerkleTree {
    fn consensus_encode(&self, encoder: &mut Encoder) {
        encoder.write_u32_le(self.n_transactions);
        encoder.write_varint(self.hashes.len() as u64);
        for hash in &self.hashes {
            encoder.write_hash_le(hash);
        }

        let mut flag_bytes = vec![0u8; (self.bits.len() + 7) / 8];
        for (idx, bit) in self.bits.iter().copied().enumerate() {
            if bit {
                flag_bytes[idx / 8] |= 1u8 << (idx % 8);
            }
        }
        encoder.write_var_bytes(&flag_bytes);
    }
}

impl Decodable for PartialMerkleTree {
    fn consensus_decode(decoder: &mut Decoder) -> Result<Self, DecodeError> {
        let n_transactions = decoder.read_u32_le()?;
        if n_transactions > MAX_BLOCK_SIZE / MIN_SERIALIZED_TX_SIZE {
            return Err(DecodeError::InvalidData(
                "too many transactions in merkle tree",
            ));
        }

        let hash_count = decoder.read_varint()?;
        let hash_count = usize::try_from(hash_count).map_err(|_| DecodeError::SizeTooLarge)?;
        if hash_count > n_transactions as usize {
            return Err(DecodeError::InvalidData("too many hashes in merkle tree"));
        }
        let mut hashes = Vec::with_capacity(hash_count);
        for _ in 0..hash_count {
            hashes.push(decoder.read_hash_le()?);
        }

        let bytes = decoder.read_var_bytes()?;
        let max_flag_bytes = (n_transactions as usize)
            .saturating_mul(2)
            .saturating_add(7)
            / 8;
        if bytes.len() > max_flag_bytes {
            return Err(DecodeError::InvalidData(
                "too many flag bytes in merkle tree",
            ));
        }

        let mut bits = Vec::with_capacity(bytes.len().saturating_mul(8));
        for byte in &bytes {
            for bit in 0..8 {
                bits.push((byte & (1u8 << bit)) != 0);
            }
        }

        Ok(Self {
            n_transactions,
            bits,
            hashes,
        })
    }
}

fn calc_tree_height(n_transactions: u32) -> u32 {
    let mut height = 0u32;
    while calc_tree_width(n_transactions, height) > 1 {
        height = height.saturating_add(1);
    }
    height
}

fn calc_tree_width(n_transactions: u32, height: u32) -> u32 {
    let n = n_transactions as u64;
    let shift = 1u64.checked_shl(height).unwrap_or(0);
    if shift == 0 {
        return 0;
    }
    let width = (n.saturating_add(shift.saturating_sub(1))) >> height;
    u32::try_from(width).unwrap_or(u32::MAX)
}

fn merkle_hash_pair(left: &Hash256, right: &Hash256) -> Hash256 {
    let mut buf = [0u8; 64];
    buf[0..32].copy_from_slice(left);
    buf[32..64].copy_from_slice(right);
    sha256d(&buf)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn merkle_root(txids: &[Hash256]) -> Hash256 {
        if txids.is_empty() {
            return [0u8; 32];
        }
        let mut layer = txids.to_vec();
        while layer.len() > 1 {
            if layer.len() % 2 == 1 {
                let last = *layer.last().expect("non-empty");
                layer.push(last);
            }
            let mut next = Vec::with_capacity((layer.len() + 1) / 2);
            for pair in layer.chunks(2) {
                let left = pair[0];
                let right = pair[1];
                next.push(merkle_hash_pair(&left, &right));
            }
            layer = next;
        }
        layer[0]
    }

    #[test]
    fn partial_merkle_tree_roundtrip_extract() {
        let txids: Vec<Hash256> = (0u8..7u8)
            .map(|i| {
                let mut h = [0u8; 32];
                h[0] = i;
                h
            })
            .collect();
        let matches = vec![false, true, false, true, true, false, false];

        let tree = PartialMerkleTree::from_txids(&txids, &matches).expect("build");
        let (root, extracted) = tree.extract_matches().expect("extract");
        assert_eq!(root, merkle_root(&txids));

        let expected: Vec<Hash256> = txids
            .iter()
            .zip(matches.iter().copied())
            .filter_map(|(txid, matched)| matched.then_some(*txid))
            .collect();
        assert_eq!(extracted, expected);

        let encoded = encode(&tree);
        let decoded: PartialMerkleTree = decode(&encoded).expect("decode");
        assert_eq!(encode(&decoded), encoded);
        let (decoded_root, decoded_matches) = decoded.extract_matches().expect("extract decoded");
        assert_eq!(decoded_root, root);
        assert_eq!(decoded_matches, extracted);
    }
}
