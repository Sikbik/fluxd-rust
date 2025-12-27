use std::io::{Cursor, Read};

use blake2b_simd::{Params as Blake2bParams, State as Blake2bState};

#[derive(Clone, Copy)]
struct Params {
    n: u32,
    k: u32,
}

impl Params {
    fn new(n: u32, k: u32) -> Option<Self> {
        let allow_non_octet = n == 125 && k == 4;
        if (n.is_multiple_of(8) || allow_non_octet)
            && (k >= 3)
            && (k < n)
            && n.is_multiple_of(k + 1)
        {
            Some(Self { n, k })
        } else {
            None
        }
    }

    fn indices_per_hash_output(&self) -> u32 {
        512 / self.n
    }

    fn hash_output(&self) -> u8 {
        let byte_len = self.n.div_ceil(8);
        (self.indices_per_hash_output() * byte_len) as u8
    }

    fn use_twist(&self) -> bool {
        self.n == 125 && self.k == 4
    }

    fn collision_bit_length(&self) -> usize {
        (self.n / (self.k + 1)) as usize
    }

    fn collision_byte_length(&self) -> usize {
        self.collision_bit_length().div_ceil(8)
    }
}

#[derive(Clone)]
struct Node {
    hash: Vec<u8>,
    indices: Vec<u32>,
}

impl Node {
    fn new(p: &Params, state: &Blake2bState, i: u32) -> Self {
        let hash = generate_hash(state, i / p.indices_per_hash_output(), p);
        let byte_len = p.n.div_ceil(8);
        let start = (i % p.indices_per_hash_output()) * byte_len;
        let end = start + byte_len;
        Self {
            hash: expand_array(
                &hash[start as usize..end as usize],
                p.collision_bit_length(),
                0,
            ),
            indices: vec![i],
        }
    }

    fn from_children(a: Node, b: Node, trim: usize) -> Self {
        let hash: Vec<_> = a
            .hash
            .iter()
            .zip(b.hash.iter())
            .skip(trim)
            .map(|(a, b)| a ^ b)
            .collect();
        let indices = if a.indices_before(&b) {
            let mut indices = a.indices;
            indices.extend(b.indices.iter());
            indices
        } else {
            let mut indices = b.indices;
            indices.extend(a.indices.iter());
            indices
        };
        Self { hash, indices }
    }

    fn indices_before(&self, other: &Node) -> bool {
        self.indices[0] < other.indices[0]
    }

    fn is_zero(&self, len: usize) -> bool {
        self.hash.iter().take(len).all(|v| *v == 0)
    }
}

#[derive(Debug)]
pub struct VerifyError(VerifyKind);

#[derive(Debug)]
enum VerifyKind {
    InvalidParams,
    Collision,
    OutOfOrder,
    DuplicateIdxs,
    NonZeroRootHash,
}

impl std::fmt::Display for VerifyError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self.0 {
            VerifyKind::InvalidParams => write!(f, "invalid parameters"),
            VerifyKind::Collision => write!(f, "invalid collision length between StepRows"),
            VerifyKind::OutOfOrder => write!(f, "index tree incorrectly ordered"),
            VerifyKind::DuplicateIdxs => write!(f, "duplicate indices"),
            VerifyKind::NonZeroRootHash => write!(f, "root hash of tree is non-zero"),
        }
    }
}

impl std::error::Error for VerifyError {}

fn personalise(n: u32, k: u32) -> [u8; 16] {
    let mut personalization = [0u8; 16];
    if (n == 144 && k == 5) || (n == 125 && k == 4) {
        personalization[..8].copy_from_slice(b"ZelProof");
    } else {
        personalization[..8].copy_from_slice(b"ZcashPoW");
    }
    personalization[8..12].copy_from_slice(&n.to_le_bytes());
    personalization[12..16].copy_from_slice(&k.to_le_bytes());
    personalization
}

fn initialise_state(n: u32, k: u32, digest_len: u8) -> Blake2bState {
    let personalization = personalise(n, k);
    Blake2bParams::new()
        .hash_length(digest_len as usize)
        .personal(&personalization)
        .to_state()
}

fn generate_hash(base_state: &Blake2bState, i: u32, params: &Params) -> Vec<u8> {
    let hash_len = params.hash_output() as usize;
    if !params.use_twist() {
        let mut lei = [0u8; 4];
        lei.copy_from_slice(&i.to_le_bytes());

        let mut state = base_state.clone();
        state.update(&lei);
        let hash = state.finalize();
        return hash.as_bytes()[..hash_len].to_vec();
    }

    let mut my_hash = [0u32; 16];
    let start_index = i & 0xFFFF_FFF0;
    for g2 in start_index..=i {
        let mut lei = [0u8; 4];
        lei.copy_from_slice(&g2.to_le_bytes());

        let mut state = base_state.clone();
        state.update(&lei);
        let hash = state.finalize();
        let bytes = hash.as_bytes();
        for (idx, word_acc) in my_hash.iter_mut().enumerate() {
            let offset = idx * 4;
            let word = u32::from_le_bytes(bytes[offset..offset + 4].try_into().expect("hash word"));
            *word_acc = word_acc.wrapping_add(word);
        }
    }

    let mut out = vec![0u8; hash_len];
    for (idx, word) in my_hash.iter().enumerate() {
        let offset = idx * 4;
        if offset + 4 <= out.len() {
            out[offset..offset + 4].copy_from_slice(&word.to_le_bytes());
        }
    }
    for pos in (15..hash_len).step_by(16) {
        out[pos] &= 0xF8;
    }

    out
}

fn has_collision(a: &Node, b: &Node, len: usize) -> bool {
    a.hash
        .iter()
        .zip(b.hash.iter())
        .take(len)
        .all(|(a, b)| a == b)
}

fn distinct_indices(a: &Node, b: &Node) -> bool {
    for i in &a.indices {
        for j in &b.indices {
            if i == j {
                return false;
            }
        }
    }
    true
}

fn validate_subtrees(p: &Params, a: &Node, b: &Node) -> Result<(), VerifyKind> {
    if !has_collision(a, b, p.collision_byte_length()) {
        Err(VerifyKind::Collision)
    } else if b.indices_before(a) {
        Err(VerifyKind::OutOfOrder)
    } else if !distinct_indices(a, b) {
        Err(VerifyKind::DuplicateIdxs)
    } else {
        Ok(())
    }
}

fn tree_validator(p: &Params, state: &Blake2bState, indices: &[u32]) -> Result<Node, VerifyError> {
    if indices.len() > 1 {
        let mid = indices.len() / 2;
        let a = tree_validator(p, state, &indices[..mid])?;
        let b = tree_validator(p, state, &indices[mid..])?;
        validate_subtrees(p, &a, &b).map_err(VerifyError)?;
        Ok(Node::from_children(a, b, p.collision_byte_length()))
    } else {
        Ok(Node::new(p, state, indices[0]))
    }
}

pub fn is_valid_solution(
    n: u32,
    k: u32,
    input: &[u8],
    nonce: &[u8],
    soln: &[u8],
) -> Result<(), VerifyError> {
    let p = Params::new(n, k).ok_or(VerifyError(VerifyKind::InvalidParams))?;
    let indices = indices_from_minimal(p, soln).ok_or(VerifyError(VerifyKind::InvalidParams))?;

    let mut state = initialise_state(p.n, p.k, p.hash_output());
    state.update(input);
    state.update(nonce);

    let root = tree_validator(&p, &state, &indices)?;
    if root.is_zero(p.collision_byte_length()) {
        Ok(())
    } else {
        Err(VerifyError(VerifyKind::NonZeroRootHash))
    }
}

fn expand_array(vin: &[u8], bit_len: usize, byte_pad: usize) -> Vec<u8> {
    let out_width = bit_len.div_ceil(8) + byte_pad;
    let out_len = 8 * out_width * vin.len() / bit_len;

    if out_len == vin.len() {
        return vin.to_vec();
    }

    let mut vout: Vec<u8> = vec![0; out_len];
    let bit_len_mask: u32 = (1 << bit_len) - 1;

    let mut acc_bits = 0;
    let mut acc_value: u32 = 0;

    let mut j = 0;
    for b in vin {
        acc_value = (acc_value << 8) | u32::from(*b);
        acc_bits += 8;

        if acc_bits >= bit_len {
            acc_bits -= bit_len;
            for x in byte_pad..out_width {
                vout[j + x] = ((acc_value >> (acc_bits + (8 * (out_width - x - 1))))
                    & ((bit_len_mask >> (8 * (out_width - x - 1))) & 0xFF))
                    as u8;
            }
            j += out_width;
        }
    }

    vout
}

fn read_u32_be(csr: &mut Cursor<Vec<u8>>) -> std::io::Result<u32> {
    let mut n = [0; 4];
    csr.read_exact(&mut n)?;
    Ok(u32::from_be_bytes(n))
}

fn indices_from_minimal(p: Params, minimal: &[u8]) -> Option<Vec<u32>> {
    let c_bit_len = p.collision_bit_length();
    if minimal.len() != ((1 << p.k) * (c_bit_len + 1)) / 8 {
        return None;
    }

    let len_indices = u32::BITS as usize * minimal.len() / (c_bit_len + 1);
    let byte_pad = std::mem::size_of::<u32>() - (c_bit_len + 1).div_ceil(8);

    let mut csr = Cursor::new(expand_array(minimal, c_bit_len + 1, byte_pad));
    let mut ret = Vec::with_capacity(len_indices);

    while let Ok(i) = read_u32_be(&mut csr) {
        ret.push(i);
    }

    Some(ret)
}
