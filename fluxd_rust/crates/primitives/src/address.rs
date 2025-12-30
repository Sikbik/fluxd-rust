//! Base58 address decoding and script construction.

use fluxd_consensus::Network;

use crate::hash::sha256d;

#[derive(Debug)]
pub enum AddressError {
    InvalidLength,
    InvalidCharacter,
    InvalidChecksum,
    UnknownPrefix,
}

pub fn address_to_script_pubkey(address: &str, network: Network) -> Result<Vec<u8>, AddressError> {
    let payload = base58check_decode(address)?;
    let (pubkey_prefix, script_prefix) = network_prefixes(network);

    if payload.starts_with(pubkey_prefix) {
        let hash = &payload[pubkey_prefix.len()..];
        if hash.len() != 20 {
            return Err(AddressError::InvalidLength);
        }
        return Ok(p2pkh_script(hash));
    }
    if payload.starts_with(script_prefix) {
        let hash = &payload[script_prefix.len()..];
        if hash.len() != 20 {
            return Err(AddressError::InvalidLength);
        }
        return Ok(p2sh_script(hash));
    }

    Err(AddressError::UnknownPrefix)
}

pub fn script_pubkey_to_address(script: &[u8], network: Network) -> Option<String> {
    let (pubkey_prefix, script_prefix) = network_prefixes(network);
    if is_p2pkh(script) {
        let hash = &script[3..23];
        let mut payload = Vec::with_capacity(pubkey_prefix.len() + hash.len());
        payload.extend_from_slice(pubkey_prefix);
        payload.extend_from_slice(hash);
        return Some(base58check_encode(&payload));
    }
    if is_p2sh(script) {
        let hash = &script[2..22];
        let mut payload = Vec::with_capacity(script_prefix.len() + hash.len());
        payload.extend_from_slice(script_prefix);
        payload.extend_from_slice(hash);
        return Some(base58check_encode(&payload));
    }
    None
}

pub fn secret_key_to_wif(secret: &[u8; 32], network: Network, compressed: bool) -> String {
    let prefix = match network {
        Network::Mainnet => 0x80,
        Network::Testnet | Network::Regtest => 0xEF,
    };
    let mut payload = Vec::with_capacity(1 + secret.len() + usize::from(compressed));
    payload.push(prefix);
    payload.extend_from_slice(secret);
    if compressed {
        payload.push(0x01);
    }
    base58check_encode(&payload)
}

pub fn wif_to_secret_key(wif: &str, network: Network) -> Result<([u8; 32], bool), AddressError> {
    let payload = base58check_decode(wif)?;
    if payload.is_empty() {
        return Err(AddressError::InvalidLength);
    }

    let expected_prefix = match network {
        Network::Mainnet => 0x80,
        Network::Testnet | Network::Regtest => 0xEF,
    };
    if payload[0] != expected_prefix {
        return Err(AddressError::UnknownPrefix);
    }

    if payload.len() == 33 {
        let mut secret = [0u8; 32];
        secret.copy_from_slice(&payload[1..33]);
        return Ok((secret, false));
    }

    if payload.len() == 34 && payload[33] == 0x01 {
        let mut secret = [0u8; 32];
        secret.copy_from_slice(&payload[1..33]);
        return Ok((secret, true));
    }

    Err(AddressError::InvalidLength)
}

fn network_prefixes(network: Network) -> (&'static [u8], &'static [u8]) {
    match network {
        Network::Mainnet => (&[0x1C, 0xB8], &[0x1C, 0xBD]),
        Network::Testnet | Network::Regtest => (&[0x1D, 0x25], &[0x1C, 0xBA]),
    }
}

fn p2pkh_script(hash: &[u8]) -> Vec<u8> {
    const OP_DUP: u8 = 0x76;
    const OP_HASH160: u8 = 0xa9;
    const OP_EQUALVERIFY: u8 = 0x88;
    const OP_CHECKSIG: u8 = 0xac;

    let mut script = Vec::with_capacity(25);
    script.push(OP_DUP);
    script.push(OP_HASH160);
    script.push(0x14);
    script.extend_from_slice(hash);
    script.push(OP_EQUALVERIFY);
    script.push(OP_CHECKSIG);
    script
}

fn p2sh_script(hash: &[u8]) -> Vec<u8> {
    const OP_HASH160: u8 = 0xa9;
    const OP_EQUAL: u8 = 0x87;

    let mut script = Vec::with_capacity(23);
    script.push(OP_HASH160);
    script.push(0x14);
    script.extend_from_slice(hash);
    script.push(OP_EQUAL);
    script
}

fn is_p2pkh(script: &[u8]) -> bool {
    script.len() == 25
        && script[0] == 0x76
        && script[1] == 0xa9
        && script[2] == 0x14
        && script[23] == 0x88
        && script[24] == 0xac
}

fn is_p2sh(script: &[u8]) -> bool {
    script.len() == 23 && script[0] == 0xa9 && script[1] == 0x14 && script[22] == 0x87
}

fn base58check_decode(input: &str) -> Result<Vec<u8>, AddressError> {
    let bytes = base58_decode(input)?;
    if bytes.len() < 4 {
        return Err(AddressError::InvalidLength);
    }
    let (payload, checksum) = bytes.split_at(bytes.len() - 4);
    let digest = sha256d(payload);
    if checksum != &digest[..4] {
        return Err(AddressError::InvalidChecksum);
    }
    Ok(payload.to_vec())
}

fn base58check_encode(payload: &[u8]) -> String {
    let mut data = Vec::with_capacity(payload.len() + 4);
    data.extend_from_slice(payload);
    let checksum = sha256d(payload);
    data.extend_from_slice(&checksum[..4]);
    base58_encode(&data)
}

fn base58_decode(input: &str) -> Result<Vec<u8>, AddressError> {
    if input.is_empty() {
        return Err(AddressError::InvalidLength);
    }
    let mut bytes = Vec::new();
    for ch in input.bytes() {
        let value = base58_value(ch).ok_or(AddressError::InvalidCharacter)? as u32;
        let mut carry = value;
        for byte in bytes.iter_mut().rev() {
            let val = (*byte as u32) * 58 + carry;
            *byte = (val & 0xff) as u8;
            carry = val >> 8;
        }
        while carry > 0 {
            bytes.insert(0, (carry & 0xff) as u8);
            carry >>= 8;
        }
    }

    let leading_zeros = input.bytes().take_while(|b| *b == b'1').count();
    let mut out = vec![0u8; leading_zeros];
    out.extend_from_slice(&bytes);
    Ok(out)
}

fn base58_encode(data: &[u8]) -> String {
    const ALPHABET: &[u8; 58] = b"123456789ABCDEFGHJKLMNPQRSTUVWXYZabcdefghijkmnopqrstuvwxyz";
    if data.is_empty() {
        return String::new();
    }
    let mut digits = vec![0u8];
    for byte in data {
        let mut carry = *byte as u32;
        for digit in digits.iter_mut().rev() {
            let value = (*digit as u32) * 256 + carry;
            *digit = (value % 58) as u8;
            carry = value / 58;
        }
        while carry > 0 {
            digits.insert(0, (carry % 58) as u8);
            carry /= 58;
        }
    }
    let leading_zeros = data.iter().take_while(|b| **b == 0u8).count();
    let mut out = String::with_capacity(leading_zeros + digits.len());
    for _ in 0..leading_zeros {
        out.push('1');
    }
    for digit in digits {
        out.push(ALPHABET[digit as usize] as char);
    }
    out
}

fn base58_value(byte: u8) -> Option<u8> {
    const ALPHABET: &[u8; 58] = b"123456789ABCDEFGHJKLMNPQRSTUVWXYZabcdefghijkmnopqrstuvwxyz";
    ALPHABET
        .iter()
        .position(|value| *value == byte)
        .map(|pos| pos as u8)
}
