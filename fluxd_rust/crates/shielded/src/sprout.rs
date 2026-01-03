use blake2b_simd::Params as Blake2bParams;
use bls12_381::Bls12;
use chacha20poly1305::aead::{Aead, Payload};
use chacha20poly1305::{ChaCha20Poly1305, KeyInit};
use ed25519_dalek::{Signer, SigningKey};
use fluxd_consensus::money::MAX_MONEY;
use fluxd_consensus::Hash256;
use fluxd_primitives::transaction::{JoinSplit, SproutProof, ZC_NOTE_CIPHERTEXT_SIZE};
use rand_core::{OsRng, RngCore};
use sha2::compress256;
use sha2::digest::generic_array::GenericArray;
use sha2::{Digest, Sha256};
use std::fs::File;
use std::io::BufReader;
use std::path::{Path, PathBuf};
use std::sync::{Arc, OnceLock};
use x25519_dalek::{PublicKey as X25519PublicKey, StaticSecret};

pub const NOTEENCRYPTION_AUTH_BYTES: usize = 16;
pub const ZC_NOTEPLAINTEXT_SIZE: usize = 1 + 8 + 32 + 32 + 512;
pub const SPROUT_ENCRYPTED_NOTE_SIZE: usize = 1 + 32 + ZC_NOTE_CIPHERTEXT_SIZE + 32;
pub const SPROUT_WITNESS_PATH_SIZE: usize = zcash_proofs::sprout::WITNESS_PATH_SIZE;

const SHA256_IV: [u32; 8] = [
    0x6a09e667, 0xbb67ae85, 0x3c6ef372, 0xa54ff53a, 0x510e527f, 0x9b05688c, 0x1f83d9ab, 0x5be0cd19,
];

#[derive(Debug, Clone)]
pub enum SproutError {
    InvalidData(&'static str),
    Crypto(&'static str),
}

impl std::fmt::Display for SproutError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            SproutError::InvalidData(msg) => write!(f, "{msg}"),
            SproutError::Crypto(msg) => write!(f, "{msg}"),
        }
    }
}

impl std::error::Error for SproutError {}

struct SproutProvingKeyState {
    params_path: PathBuf,
    proving_key: Arc<bellman::groth16::Parameters<Bls12>>,
}

static SPROUT_PROVING_KEY_STATE: OnceLock<Result<SproutProvingKeyState, &'static str>> =
    OnceLock::new();

pub fn sprout_proving_key(
    params_path: &Path,
) -> Result<Arc<bellman::groth16::Parameters<Bls12>>, SproutError> {
    let state = SPROUT_PROVING_KEY_STATE.get_or_init(|| {
        let file = File::open(params_path).map_err(|_| "failed to open Sprout proving params")?;
        let mut reader = BufReader::with_capacity(1024 * 1024, file);
        let proving_key = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| {
            bellman::groth16::Parameters::<Bls12>::read(&mut reader, false)
        }))
        .map_err(|_| "failed to parse Sprout proving params")?
        .map_err(|_| "failed to parse Sprout proving params")?;
        Ok(SproutProvingKeyState {
            params_path: params_path.to_path_buf(),
            proving_key: Arc::new(proving_key),
        })
    });

    match state {
        Ok(state) => {
            if state.params_path != params_path {
                return Err(SproutError::InvalidData(
                    "sprout proving key initialized with different params path",
                ));
            }
            Ok(Arc::clone(&state.proving_key))
        }
        Err(err) => Err(SproutError::Crypto(err)),
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct SproutSpendingKey([u8; 32]);

impl SproutSpendingKey {
    pub fn from_bytes(bytes: [u8; 32]) -> Result<Self, SproutError> {
        if (bytes[0] & 0xF0) != 0 {
            return Err(SproutError::InvalidData(
                "spending key has invalid leading bits",
            ));
        }
        Ok(Self(bytes))
    }

    pub fn to_bytes(self) -> [u8; 32] {
        self.0
    }

    pub fn receiving_key(self) -> [u8; 32] {
        ZCNoteEncryption::generate_privkey(self.0)
    }

    pub fn viewing_key(self) -> SproutViewingKey {
        SproutViewingKey {
            a_pk: prf_addr_a_pk(&self.0),
            sk_enc: self.receiving_key(),
        }
    }

    pub fn address(self) -> SproutPaymentAddress {
        self.viewing_key().address()
    }

    pub fn random() -> Self {
        Self(random_uint252())
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct SproutViewingKey {
    pub a_pk: [u8; 32],
    pub sk_enc: [u8; 32],
}

impl SproutViewingKey {
    pub fn address(self) -> SproutPaymentAddress {
        SproutPaymentAddress {
            a_pk: self.a_pk,
            pk_enc: ZCNoteEncryption::generate_pubkey(&self.sk_enc),
        }
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct SproutPaymentAddress {
    pub a_pk: [u8; 32],
    pub pk_enc: [u8; 32],
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct SproutNote {
    pub a_pk: [u8; 32],
    pub value: u64,
    pub rho: [u8; 32],
    pub r: [u8; 32],
}

impl SproutNote {
    pub fn cm(self) -> [u8; 32] {
        let mut hasher = Sha256::new();
        hasher.update([0xb0]);
        hasher.update(self.a_pk);
        hasher.update(self.value.to_le_bytes());
        hasher.update(self.rho);
        hasher.update(self.r);
        let digest = hasher.finalize();
        let mut out = [0u8; 32];
        out.copy_from_slice(&digest);
        out
    }

    pub fn nullifier(self, a_sk: SproutSpendingKey) -> [u8; 32] {
        prf_nf(&a_sk.0, &self.rho)
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct SproutNotePlaintext {
    pub value: u64,
    pub rho: [u8; 32],
    pub r: [u8; 32],
    pub memo: [u8; 512],
}

impl SproutNotePlaintext {
    pub fn new(note: SproutNote, memo: [u8; 512]) -> Self {
        Self {
            value: note.value,
            rho: note.rho,
            r: note.r,
            memo,
        }
    }

    pub fn to_bytes(self) -> [u8; ZC_NOTEPLAINTEXT_SIZE] {
        let mut out = [0u8; ZC_NOTEPLAINTEXT_SIZE];
        out[0] = 0x00;
        out[1..9].copy_from_slice(&self.value.to_le_bytes());
        out[9..41].copy_from_slice(&self.rho);
        out[41..73].copy_from_slice(&self.r);
        out[73..].copy_from_slice(&self.memo);
        out
    }

    pub fn from_bytes(bytes: &[u8; ZC_NOTEPLAINTEXT_SIZE]) -> Result<Self, SproutError> {
        if bytes[0] != 0x00 {
            return Err(SproutError::InvalidData(
                "lead byte of SproutNotePlaintext is not recognized",
            ));
        }
        let mut value_bytes = [0u8; 8];
        value_bytes.copy_from_slice(&bytes[1..9]);
        let value = u64::from_le_bytes(value_bytes);

        let mut rho = [0u8; 32];
        rho.copy_from_slice(&bytes[9..41]);
        let mut r = [0u8; 32];
        r.copy_from_slice(&bytes[41..73]);
        let mut memo = [0u8; 512];
        memo.copy_from_slice(&bytes[73..]);

        Ok(Self {
            value,
            rho,
            r,
            memo,
        })
    }

    pub fn note(self, addr: SproutPaymentAddress) -> SproutNote {
        SproutNote {
            a_pk: addr.a_pk,
            value: self.value,
            rho: self.rho,
            r: self.r,
        }
    }

    pub fn encrypt(
        self,
        encryptor: &mut ZCNoteEncryption,
        pk_enc: &[u8; 32],
    ) -> Result<[u8; ZC_NOTE_CIPHERTEXT_SIZE], SproutError> {
        encryptor.encrypt(pk_enc, &self.to_bytes())
    }

    pub fn decrypt(
        decryptor: &ZCNoteDecryption,
        ciphertext: &[u8; ZC_NOTE_CIPHERTEXT_SIZE],
        epk: &[u8; 32],
        h_sig: &[u8; 32],
        nonce: u8,
    ) -> Result<Self, SproutError> {
        let pt = decryptor.decrypt(ciphertext, epk, h_sig, nonce)?;
        Self::from_bytes(&pt)
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct SproutEncryptedNote {
    pub nonce: u8,
    pub epk: [u8; 32],
    pub ciphertext: [u8; ZC_NOTE_CIPHERTEXT_SIZE],
    pub h_sig: [u8; 32],
}

impl SproutEncryptedNote {
    pub fn from_bytes(bytes: &[u8; SPROUT_ENCRYPTED_NOTE_SIZE]) -> Result<Self, SproutError> {
        let nonce = bytes[0];
        let mut epk = [0u8; 32];
        epk.copy_from_slice(&bytes[1..33]);
        let mut ciphertext = [0u8; ZC_NOTE_CIPHERTEXT_SIZE];
        ciphertext.copy_from_slice(&bytes[33..33 + ZC_NOTE_CIPHERTEXT_SIZE]);
        let mut h_sig = [0u8; 32];
        h_sig.copy_from_slice(&bytes[33 + ZC_NOTE_CIPHERTEXT_SIZE..]);
        Ok(Self {
            nonce,
            epk,
            ciphertext,
            h_sig,
        })
    }

    pub fn to_bytes(self) -> [u8; SPROUT_ENCRYPTED_NOTE_SIZE] {
        let mut out = [0u8; SPROUT_ENCRYPTED_NOTE_SIZE];
        out[0] = self.nonce;
        out[1..33].copy_from_slice(&self.epk);
        out[33..33 + ZC_NOTE_CIPHERTEXT_SIZE].copy_from_slice(&self.ciphertext);
        out[33 + ZC_NOTE_CIPHERTEXT_SIZE..].copy_from_slice(&self.h_sig);
        out
    }
}

pub struct ZCNoteEncryption {
    epk: [u8; 32],
    esk: [u8; 32],
    nonce: u8,
    h_sig: [u8; 32],
}

impl ZCNoteEncryption {
    pub fn new(h_sig: [u8; 32]) -> Self {
        let esk = random_uint256();
        let epk = generate_pubkey(&esk);
        Self {
            epk,
            esk,
            nonce: 0,
            h_sig,
        }
    }

    pub fn get_epk(&self) -> [u8; 32] {
        self.epk
    }

    pub fn get_esk(&self) -> [u8; 32] {
        self.esk
    }

    pub fn encrypt(
        &mut self,
        pk_enc: &[u8; 32],
        message: &[u8; ZC_NOTEPLAINTEXT_SIZE],
    ) -> Result<[u8; ZC_NOTE_CIPHERTEXT_SIZE], SproutError> {
        let dhsecret = scalarmult(&self.esk, pk_enc);
        let key = kdf(&dhsecret, &self.epk, pk_enc, &self.h_sig, self.nonce)?;
        self.nonce = self.nonce.wrapping_add(1);

        let cipher = ChaCha20Poly1305::new(chacha20poly1305::Key::from_slice(&key));
        let nonce = chacha20poly1305::Nonce::from_slice(&[0u8; 12]);
        let ciphertext = cipher
            .encrypt(
                nonce,
                Payload {
                    msg: message.as_slice(),
                    aad: &[],
                },
            )
            .map_err(|_| SproutError::Crypto("note encryption failed"))?;
        let ciphertext: [u8; ZC_NOTE_CIPHERTEXT_SIZE] = ciphertext
            .as_slice()
            .try_into()
            .map_err(|_| SproutError::Crypto("invalid ciphertext length"))?;
        Ok(ciphertext)
    }

    pub fn generate_privkey(a_sk: [u8; 32]) -> [u8; 32] {
        let mut sk = prf_addr_sk_enc(&a_sk);
        clamp_curve25519(&mut sk);
        sk
    }

    pub fn generate_pubkey(sk_enc: &[u8; 32]) -> [u8; 32] {
        generate_pubkey(sk_enc)
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct ZCNoteDecryption {
    sk_enc: [u8; 32],
    pk_enc: [u8; 32],
}

impl ZCNoteDecryption {
    pub fn new(sk_enc: [u8; 32]) -> Self {
        let pk_enc = generate_pubkey(&sk_enc);
        Self { sk_enc, pk_enc }
    }

    pub fn decrypt(
        &self,
        ciphertext: &[u8; ZC_NOTE_CIPHERTEXT_SIZE],
        epk: &[u8; 32],
        h_sig: &[u8; 32],
        nonce: u8,
    ) -> Result<[u8; ZC_NOTEPLAINTEXT_SIZE], SproutError> {
        let dhsecret = scalarmult(&self.sk_enc, epk);
        let key = kdf(&dhsecret, epk, &self.pk_enc, h_sig, nonce)?;
        let cipher = ChaCha20Poly1305::new(chacha20poly1305::Key::from_slice(&key));
        let nonce = chacha20poly1305::Nonce::from_slice(&[0u8; 12]);
        let plaintext = cipher
            .decrypt(
                nonce,
                Payload {
                    msg: ciphertext.as_slice(),
                    aad: &[],
                },
            )
            .map_err(|_| SproutError::Crypto("Could not decrypt message"))?;
        let plaintext: [u8; ZC_NOTEPLAINTEXT_SIZE] = plaintext
            .as_slice()
            .try_into()
            .map_err(|_| SproutError::Crypto("invalid plaintext length"))?;
        Ok(plaintext)
    }

    pub fn pk_enc(&self) -> [u8; 32] {
        self.pk_enc
    }
}

#[derive(Clone, Debug)]
pub struct JoinSplitKeypair {
    secret_key: [u8; 32],
    pub pubkey: [u8; 32],
}

impl JoinSplitKeypair {
    pub fn generate() -> Self {
        let mut secret = [0u8; 32];
        OsRng.fill_bytes(&mut secret);
        let signing_key = SigningKey::from_bytes(&secret);
        let pubkey = signing_key.verifying_key().to_bytes();
        Self {
            secret_key: secret,
            pubkey,
        }
    }

    pub fn sign(&self, msg: &[u8; 32]) -> [u8; 64] {
        let signing_key = SigningKey::from_bytes(&self.secret_key);
        signing_key.sign(msg).to_bytes()
    }
}

#[derive(Clone, Copy, Debug)]
pub struct SproutJoinSplitInput {
    pub key: SproutSpendingKey,
    pub note: SproutNote,
    pub auth_path: [u8; SPROUT_WITNESS_PATH_SIZE],
}

#[derive(Clone, Copy, Debug)]
pub struct SproutJoinSplitOutput {
    pub addr: SproutPaymentAddress,
    pub value: u64,
    pub memo: [u8; 512],
}

impl SproutJoinSplitOutput {
    pub fn dummy() -> Self {
        let mut memo = [0u8; 512];
        memo[0] = 0xF6;
        let addr = SproutSpendingKey::random().address();
        Self {
            addr,
            value: 0,
            memo,
        }
    }
}

pub struct SproutJoinSplitResult {
    pub joinsplit: JoinSplit,
    pub encrypted_notes: [SproutEncryptedNote; 2],
}

pub fn dummy_auth_path() -> [u8; SPROUT_WITNESS_PATH_SIZE] {
    let mut out = [0u8; SPROUT_WITNESS_PATH_SIZE];
    out[0] = 29;
    let mut cursor = 1usize;
    for level in (0u8..29u8).rev() {
        out[cursor] = 32;
        cursor += 1;
        let sibling = sprout_empty_root(level);
        out[cursor..cursor + 32].copy_from_slice(&sibling);
        cursor += 32;
    }
    out[cursor..cursor + 8].copy_from_slice(&0u64.to_le_bytes());
    out
}

pub fn dummy_joinsplit_input() -> SproutJoinSplitInput {
    let key = SproutSpendingKey::random();
    let addr = key.address();
    SproutJoinSplitInput {
        key,
        note: SproutNote {
            a_pk: addr.a_pk,
            value: 0,
            rho: random_uint256(),
            r: random_uint256(),
        },
        auth_path: dummy_auth_path(),
    }
}

pub fn joinsplit_hsig(
    random_seed: &[u8; 32],
    nullifiers: &[[u8; 32]; 2],
    join_split_pub_key: &[u8; 32],
) -> [u8; 32] {
    let mut data = Vec::with_capacity(32 + 32 * 2 + 32);
    data.extend_from_slice(random_seed);
    data.extend_from_slice(&nullifiers[0]);
    data.extend_from_slice(&nullifiers[1]);
    data.extend_from_slice(join_split_pub_key);

    let hash = Blake2bParams::new()
        .hash_length(32)
        .personal(b"ZcashComputehSig")
        .hash(&data);
    let mut out = [0u8; 32];
    out.copy_from_slice(hash.as_bytes());
    out
}

pub fn prove_joinsplit(
    proving_key: &bellman::groth16::Parameters<Bls12>,
    rt: Hash256,
    join_split_pub_key: [u8; 32],
    inputs: [SproutJoinSplitInput; 2],
    outputs: [SproutJoinSplitOutput; 2],
    vpub_old: u64,
    vpub_new: u64,
) -> Result<SproutJoinSplitResult, SproutError> {
    if vpub_old > u64::try_from(MAX_MONEY).unwrap_or(0) {
        return Err(SproutError::InvalidData("nonsensical vpub_old value"));
    }
    if vpub_new > u64::try_from(MAX_MONEY).unwrap_or(0) {
        return Err(SproutError::InvalidData("nonsensical vpub_new value"));
    }

    for input in &inputs {
        if input.note.a_pk != input.key.address().a_pk {
            return Err(SproutError::InvalidData(
                "input note not authorized to spend with given key",
            ));
        }
        if input.note.value > u64::try_from(MAX_MONEY).unwrap_or(0) {
            return Err(SproutError::InvalidData("nonsensical input note value"));
        }
    }
    for output in &outputs {
        if output.value > u64::try_from(MAX_MONEY).unwrap_or(0) {
            return Err(SproutError::InvalidData("nonsensical output value"));
        }
    }

    let mut lhs_value = vpub_old;
    for input in &inputs {
        lhs_value = lhs_value
            .checked_add(input.note.value)
            .ok_or(SproutError::InvalidData(
                "nonsensical left hand side of joinsplit balance",
            ))?;
        if lhs_value > u64::try_from(MAX_MONEY).unwrap_or(0) {
            return Err(SproutError::InvalidData(
                "nonsensical left hand side of joinsplit balance",
            ));
        }
    }

    let mut rhs_value = vpub_new;
    for output in &outputs {
        rhs_value = rhs_value
            .checked_add(output.value)
            .ok_or(SproutError::InvalidData(
                "nonsensical right hand side of joinsplit balance",
            ))?;
        if rhs_value > u64::try_from(MAX_MONEY).unwrap_or(0) {
            return Err(SproutError::InvalidData(
                "nonsensical right hand side of joinsplit balance",
            ));
        }
    }

    let mut nullifiers = [[0u8; 32]; 2];
    for (i, input) in inputs.iter().enumerate() {
        nullifiers[i] = input.note.nullifier(input.key);
    }

    let random_seed = random_uint256();
    let h_sig = joinsplit_hsig(&random_seed, &nullifiers, &join_split_pub_key);
    let phi = random_uint252();

    let mut out_notes = [SproutNote {
        a_pk: [0u8; 32],
        value: 0,
        rho: [0u8; 32],
        r: [0u8; 32],
    }; 2];
    let mut commitments = [[0u8; 32]; 2];
    for (i, output) in outputs.iter().enumerate() {
        let r = random_uint256();
        let rho = prf_rho(&phi, i, &h_sig);
        out_notes[i] = SproutNote {
            a_pk: output.addr.a_pk,
            value: output.value,
            rho,
            r,
        };
        commitments[i] = out_notes[i].cm();
    }

    let mut encryptor = ZCNoteEncryption::new(h_sig);
    let mut ciphertexts = [[0u8; ZC_NOTE_CIPHERTEXT_SIZE]; 2];
    for (i, output) in outputs.iter().enumerate() {
        let pt = SproutNotePlaintext::new(out_notes[i], output.memo);
        ciphertexts[i] = pt.encrypt(&mut encryptor, &output.addr.pk_enc)?;
    }
    let ephemeral_key = encryptor.get_epk();

    let mut macs = [[0u8; 32]; 2];
    for (i, input) in inputs.iter().enumerate() {
        macs[i] = prf_pk(&input.key.0, i, &h_sig)?;
    }

    let proof = zcash_proofs::sprout::create_proof(
        phi,
        rt,
        h_sig,
        inputs[0].key.0,
        inputs[0].note.value,
        inputs[0].note.rho,
        inputs[0].note.r,
        &inputs[0].auth_path,
        inputs[1].key.0,
        inputs[1].note.value,
        inputs[1].note.rho,
        inputs[1].note.r,
        &inputs[1].auth_path,
        out_notes[0].a_pk,
        out_notes[0].value,
        out_notes[0].r,
        out_notes[1].a_pk,
        out_notes[1].value,
        out_notes[1].r,
        vpub_old,
        vpub_new,
        proving_key,
    );

    let mut proof_bytes = [0u8; 192];
    proof
        .write(&mut &mut proof_bytes[..])
        .map_err(|_| SproutError::Crypto("failed to serialize joinsplit proof"))?;

    let joinsplit = JoinSplit {
        vpub_old: i64::try_from(vpub_old)
            .map_err(|_| SproutError::InvalidData("nonsensical vpub_old value"))?,
        vpub_new: i64::try_from(vpub_new)
            .map_err(|_| SproutError::InvalidData("nonsensical vpub_new value"))?,
        anchor: rt,
        nullifiers,
        commitments,
        ephemeral_key,
        random_seed,
        macs,
        proof: SproutProof::Groth(proof_bytes),
        ciphertexts,
    };

    let encrypted_notes = [
        SproutEncryptedNote {
            nonce: 0,
            epk: ephemeral_key,
            ciphertext: ciphertexts[0],
            h_sig,
        },
        SproutEncryptedNote {
            nonce: 1,
            epk: ephemeral_key,
            ciphertext: ciphertexts[1],
            h_sig,
        },
    ];

    Ok(SproutJoinSplitResult {
        joinsplit,
        encrypted_notes,
    })
}

fn random_uint256() -> [u8; 32] {
    let mut out = [0u8; 32];
    OsRng.fill_bytes(&mut out);
    out
}

fn random_uint252() -> [u8; 32] {
    let mut out = random_uint256();
    out[0] &= 0x0f;
    out
}

fn clamp_curve25519(key: &mut [u8; 32]) {
    key[0] &= 248;
    key[31] &= 127;
    key[31] |= 64;
}

fn generate_pubkey(sk: &[u8; 32]) -> [u8; 32] {
    X25519PublicKey::from(&StaticSecret::from(*sk)).to_bytes()
}

fn scalarmult(sk: &[u8; 32], pk: &[u8; 32]) -> [u8; 32] {
    StaticSecret::from(*sk)
        .diffie_hellman(&X25519PublicKey::from(*pk))
        .to_bytes()
}

fn kdf(
    dhsecret: &[u8; 32],
    epk: &[u8; 32],
    pk_enc: &[u8; 32],
    h_sig: &[u8; 32],
    nonce: u8,
) -> Result<[u8; 32], SproutError> {
    if nonce == 0xff {
        return Err(SproutError::InvalidData(
            "no additional nonce space for KDF",
        ));
    }

    let mut block = [0u8; 128];
    block[..32].copy_from_slice(h_sig);
    block[32..64].copy_from_slice(dhsecret);
    block[64..96].copy_from_slice(epk);
    block[96..].copy_from_slice(pk_enc);

    let mut personal = [0u8; 16];
    personal[..8].copy_from_slice(b"ZcashKDF");
    personal[8] = nonce;

    let hash = Blake2bParams::new()
        .hash_length(32)
        .personal(&personal)
        .hash(&block);
    let mut out = [0u8; 32];
    out.copy_from_slice(hash.as_bytes());
    Ok(out)
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

fn sprout_empty_root(level: u8) -> [u8; 32] {
    let mut out = [0u8; 32];
    for _ in 0..level {
        out = sha256_compress(&out, &out);
    }
    out
}

fn prf(a: bool, b: bool, c: bool, d: bool, x: &[u8; 32], y: &[u8; 32]) -> [u8; 32] {
    let mut blob = [0u8; 64];
    blob[..32].copy_from_slice(x);
    blob[32..].copy_from_slice(y);

    blob[0] &= 0x0f;
    blob[0] |= (u8::from(a) << 7) | (u8::from(b) << 6) | (u8::from(c) << 5) | (u8::from(d) << 4);

    let mut left = [0u8; 32];
    left.copy_from_slice(&blob[..32]);
    let mut right = [0u8; 32];
    right.copy_from_slice(&blob[32..]);
    sha256_compress(&left, &right)
}

fn prf_addr(a_sk: &[u8; 32], t: u8) -> [u8; 32] {
    let mut y = [0u8; 32];
    y[0] = t;
    prf(true, true, false, false, a_sk, &y)
}

fn prf_addr_a_pk(a_sk: &[u8; 32]) -> [u8; 32] {
    prf_addr(a_sk, 0)
}

fn prf_addr_sk_enc(a_sk: &[u8; 32]) -> [u8; 32] {
    prf_addr(a_sk, 1)
}

fn prf_nf(a_sk: &[u8; 32], rho: &[u8; 32]) -> [u8; 32] {
    prf(true, true, true, false, a_sk, rho)
}

fn prf_pk(a_sk: &[u8; 32], i0: usize, h_sig: &[u8; 32]) -> Result<[u8; 32], SproutError> {
    match i0 {
        0 => Ok(prf(false, false, false, false, a_sk, h_sig)),
        1 => Ok(prf(false, true, false, false, a_sk, h_sig)),
        _ => Err(SproutError::InvalidData(
            "PRF_pk invoked with index out of bounds",
        )),
    }
}

fn prf_rho(phi: &[u8; 32], i0: usize, h_sig: &[u8; 32]) -> [u8; 32] {
    match i0 {
        0 => prf(false, false, true, false, phi, h_sig),
        1 => prf(false, true, true, false, phi, h_sig),
        _ => prf(false, true, true, false, phi, h_sig),
    }
}
