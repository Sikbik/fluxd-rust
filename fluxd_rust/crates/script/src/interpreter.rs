//! Script interpreter and validation.

use fluxd_primitives::hash::{sha256, sha256d};
use fluxd_primitives::transaction::Transaction;
use ripemd::{Digest as RipemdDigest, Ripemd160};
use secp256k1::{ecdsa::Signature, Message, PublicKey, Secp256k1};
use sha1::Sha1;

use crate::sighash::{signature_hash, SighashType, SIGHASH_NONE, SIGHASH_SINGLE};

pub type ScriptFlags = u32;

pub const SCRIPT_VERIFY_NONE: ScriptFlags = 0;
pub const SCRIPT_VERIFY_P2SH: ScriptFlags = 1 << 0;
pub const SCRIPT_VERIFY_STRICTENC: ScriptFlags = 1 << 1;
pub const SCRIPT_VERIFY_LOW_S: ScriptFlags = 1 << 3;
pub const SCRIPT_VERIFY_NULLDUMMY: ScriptFlags = 1 << 4;
pub const SCRIPT_VERIFY_SIGPUSHONLY: ScriptFlags = 1 << 5;
pub const SCRIPT_VERIFY_MINIMALDATA: ScriptFlags = 1 << 6;
pub const SCRIPT_VERIFY_DISCOURAGE_UPGRADABLE_NOPS: ScriptFlags = 1 << 7;
pub const SCRIPT_VERIFY_CLEANSTACK: ScriptFlags = 1 << 8;
pub const SCRIPT_VERIFY_CHECKLOCKTIMEVERIFY: ScriptFlags = 1 << 9;

pub const MANDATORY_SCRIPT_VERIFY_FLAGS: ScriptFlags = SCRIPT_VERIFY_P2SH;
pub const STANDARD_SCRIPT_VERIFY_FLAGS: ScriptFlags = MANDATORY_SCRIPT_VERIFY_FLAGS
    | SCRIPT_VERIFY_STRICTENC
    | SCRIPT_VERIFY_MINIMALDATA
    | SCRIPT_VERIFY_NULLDUMMY
    | SCRIPT_VERIFY_DISCOURAGE_UPGRADABLE_NOPS
    | SCRIPT_VERIFY_CLEANSTACK
    | SCRIPT_VERIFY_CHECKLOCKTIMEVERIFY
    | SCRIPT_VERIFY_LOW_S;
pub const BLOCK_SCRIPT_VERIFY_FLAGS: ScriptFlags =
    SCRIPT_VERIFY_P2SH | SCRIPT_VERIFY_CHECKLOCKTIMEVERIFY;

const OP_0: u8 = 0x00;
const OP_1NEGATE: u8 = 0x4f;
const OP_PUSHDATA1: u8 = 0x4c;
const OP_PUSHDATA2: u8 = 0x4d;
const OP_PUSHDATA4: u8 = 0x4e;
const OP_1: u8 = 0x51;
const OP_16: u8 = 0x60;
const OP_IF: u8 = 0x63;
const OP_NOTIF: u8 = 0x64;
const OP_ELSE: u8 = 0x67;
const OP_ENDIF: u8 = 0x68;
const OP_2DROP: u8 = 0x6d;
const OP_DUP: u8 = 0x76;
const OP_DROP: u8 = 0x75;
const OP_RIPEMD160: u8 = 0xa6;
const OP_SHA1: u8 = 0xa7;
const OP_SHA256: u8 = 0xa8;
const OP_HASH160: u8 = 0xa9;
const OP_HASH256: u8 = 0xaa;
const OP_CODESEPARATOR: u8 = 0xab;
const OP_EQUAL: u8 = 0x87;
const OP_EQUALVERIFY: u8 = 0x88;
const OP_SIZE: u8 = 0x82;
const OP_CHECKSIG: u8 = 0xac;
const OP_CHECKSIGVERIFY: u8 = 0xad;
const OP_CHECKMULTISIG: u8 = 0xae;
const OP_CHECKMULTISIGVERIFY: u8 = 0xaf;
const OP_VERIFY: u8 = 0x69;
const OP_RETURN: u8 = 0x6a;
const OP_NOP1: u8 = 0xb0;
const OP_CHECKLOCKTIMEVERIFY: u8 = 0xb1;
const OP_NOP3: u8 = 0xb2;
const OP_NOP4: u8 = 0xb3;
const OP_NOP10: u8 = 0xb9;

#[derive(Debug)]
pub enum ScriptError {
    StackUnderflow,
    EvalFalse,
    InvalidOpcode,
    SigEncoding,
    PubkeyEncoding,
    SigHashType,
    SigCheck,
    SigPushOnly,
    NullDummy,
    LockTime,
    MinimalData,
    ScriptError(&'static str),
}

impl std::fmt::Display for ScriptError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            ScriptError::StackUnderflow => write!(f, "script stack underflow"),
            ScriptError::EvalFalse => write!(f, "script evaluated to false"),
            ScriptError::InvalidOpcode => write!(f, "invalid opcode"),
            ScriptError::SigEncoding => write!(f, "invalid signature encoding"),
            ScriptError::PubkeyEncoding => write!(f, "invalid public key encoding"),
            ScriptError::SigHashType => write!(f, "invalid sighash type"),
            ScriptError::SigCheck => write!(f, "signature check failed"),
            ScriptError::SigPushOnly => write!(f, "scriptSig is not push-only"),
            ScriptError::NullDummy => write!(f, "null dummy element required"),
            ScriptError::LockTime => write!(f, "locktime check failed"),
            ScriptError::MinimalData => write!(f, "non-minimal push"),
            ScriptError::ScriptError(message) => write!(f, "{message}"),
        }
    }
}

impl std::error::Error for ScriptError {}

pub fn verify_script(
    script_sig: &[u8],
    script_pubkey: &[u8],
    tx: &Transaction,
    input_index: usize,
    amount: i64,
    flags: ScriptFlags,
    consensus_branch_id: u32,
) -> Result<(), ScriptError> {
    if (flags & SCRIPT_VERIFY_SIGPUSHONLY) != 0 && !is_push_only(script_sig) {
        return Err(ScriptError::SigPushOnly);
    }

    let checker = SignatureChecker {
        tx,
        input_index,
        amount,
        flags,
        consensus_branch_id,
    };

    let mut stack = Vec::new();
    eval_script(script_sig, &mut stack, &checker)?;

    let mut stack_copy = stack.clone();
    eval_script(script_pubkey, &mut stack, &checker)?;

    if stack.is_empty() || !cast_to_bool(stack.last().unwrap()) {
        return Err(ScriptError::EvalFalse);
    }

    if (flags & SCRIPT_VERIFY_P2SH) != 0 && is_p2sh(script_pubkey) {
        if !is_push_only(script_sig) {
            return Err(ScriptError::SigPushOnly);
        }
        if stack_copy.is_empty() {
            return Err(ScriptError::StackUnderflow);
        }
        let redeem_script = stack_copy.pop().ok_or(ScriptError::StackUnderflow)?;
        stack = stack_copy;
        eval_script(&redeem_script, &mut stack, &checker)?;
        if stack.is_empty() || !cast_to_bool(stack.last().unwrap()) {
            return Err(ScriptError::EvalFalse);
        }
    }

    if (flags & SCRIPT_VERIFY_CLEANSTACK) != 0 {
        if stack.len() != 1 || !cast_to_bool(&stack[0]) {
            return Err(ScriptError::EvalFalse);
        }
    }

    Ok(())
}

struct SignatureChecker<'a> {
    tx: &'a Transaction,
    input_index: usize,
    amount: i64,
    flags: ScriptFlags,
    consensus_branch_id: u32,
}

impl<'a> SignatureChecker<'a> {
    fn check_sig(&self, sig_bytes: &[u8], pubkey_bytes: &[u8], script_code: &[u8]) -> Result<bool, ScriptError> {
        if sig_bytes.is_empty() {
            return Ok(false);
        }
        let sighash_type = *sig_bytes.last().ok_or(ScriptError::SigEncoding)? as u32;
        if (self.flags & SCRIPT_VERIFY_STRICTENC) != 0 {
            let base_type = sighash_type & 0x1f;
            if base_type != 0x01 && base_type != SIGHASH_NONE && base_type != SIGHASH_SINGLE {
                return Err(ScriptError::SigHashType);
            }
        }

        let der = &sig_bytes[..sig_bytes.len() - 1];
        let sig = Signature::from_der(der).map_err(|_| {
            eprintln!(
                "invalid DER signature (len {}): {}",
                sig_bytes.len(),
                bytes_to_hex(sig_bytes)
            );
            ScriptError::SigEncoding
        })?;

        let mut normalized = sig;
        normalized.normalize_s();
        let sig_for_verify = if (self.flags & SCRIPT_VERIFY_LOW_S) != 0 {
            if normalized != sig {
                return Err(ScriptError::SigEncoding);
            }
            normalized
        } else {
            normalized
        };

        if (self.flags & SCRIPT_VERIFY_STRICTENC) != 0 && !is_valid_pubkey(pubkey_bytes) {
            return Err(ScriptError::PubkeyEncoding);
        }

        let pubkey = PublicKey::from_slice(pubkey_bytes).map_err(|_| ScriptError::PubkeyEncoding)?;
        let sighash = match signature_hash(
            self.tx,
            Some(self.input_index),
            script_code,
            self.amount,
            SighashType(sighash_type),
            self.consensus_branch_id,
        )
        {
            Ok(hash) => hash,
            Err(_) => return Ok(false),
        };

        let msg = Message::from_digest_slice(&sighash).map_err(|_| ScriptError::SigCheck)?;
        let secp = Secp256k1::verification_only();
        Ok(secp.verify_ecdsa(&msg, &sig_for_verify, &pubkey).is_ok())
    }

    fn check_lock_time(&self, lock_time: i64) -> Result<(), ScriptError> {
        const LOCKTIME_THRESHOLD: i64 = 500_000_000;
        let tx_lock_time = self.tx.lock_time as i64;
        if (tx_lock_time < LOCKTIME_THRESHOLD && lock_time >= LOCKTIME_THRESHOLD)
            || (tx_lock_time >= LOCKTIME_THRESHOLD && lock_time < LOCKTIME_THRESHOLD)
        {
            return Err(ScriptError::LockTime);
        }

        if lock_time > tx_lock_time {
            return Err(ScriptError::LockTime);
        }

        if self.tx.vin[self.input_index].sequence == u32::MAX {
            return Err(ScriptError::LockTime);
        }

        Ok(())
    }
}

fn eval_script(
    script: &[u8],
    stack: &mut Vec<Vec<u8>>,
    checker: &SignatureChecker<'_>,
) -> Result<(), ScriptError> {
    let mut cursor = 0usize;
    let mut script_code_start = 0usize;
    let mut exec_stack: Vec<bool> = Vec::new();
    while cursor < script.len() {
        let opcode = script[cursor];
        cursor += 1;
        let exec = exec_stack.iter().all(|v| *v);

        match opcode {
            OP_0 => {
                if exec {
                    stack.push(Vec::new());
                }
            }
            OP_1NEGATE => {
                if exec {
                    stack.push(script_num_to_vec(-1));
                }
            }
            0x01..=0x4b => {
                let len = opcode as usize;
                let data = read_bytes(script, &mut cursor, len)?;
                if exec {
                    if (checker.flags & SCRIPT_VERIFY_MINIMALDATA) != 0
                        && !check_minimal_push(&data, opcode)
                    {
                        return Err(ScriptError::MinimalData);
                    }
                    stack.push(data);
                }
            }
            OP_PUSHDATA1 => {
                let len = read_u8(script, &mut cursor)? as usize;
                let data = read_bytes(script, &mut cursor, len)?;
                if exec {
                    if (checker.flags & SCRIPT_VERIFY_MINIMALDATA) != 0
                        && !check_minimal_push(&data, opcode)
                    {
                        return Err(ScriptError::MinimalData);
                    }
                    stack.push(data);
                }
            }
            OP_PUSHDATA2 => {
                let len = read_u16(script, &mut cursor)? as usize;
                let data = read_bytes(script, &mut cursor, len)?;
                if exec {
                    if (checker.flags & SCRIPT_VERIFY_MINIMALDATA) != 0
                        && !check_minimal_push(&data, opcode)
                    {
                        return Err(ScriptError::MinimalData);
                    }
                    stack.push(data);
                }
            }
            OP_PUSHDATA4 => {
                let len = read_u32(script, &mut cursor)? as usize;
                let data = read_bytes(script, &mut cursor, len)?;
                if exec {
                    if (checker.flags & SCRIPT_VERIFY_MINIMALDATA) != 0
                        && !check_minimal_push(&data, opcode)
                    {
                        return Err(ScriptError::MinimalData);
                    }
                    stack.push(data);
                }
            }
            OP_1..=OP_16 => {
                if exec {
                    let value = (opcode - OP_1 + 1) as i64;
                    stack.push(script_num_to_vec(value));
                }
            }
            OP_IF | OP_NOTIF => {
                if exec {
                    let value = cast_to_bool(&pop(stack)?);
                    let branch = if opcode == OP_NOTIF { !value } else { value };
                    exec_stack.push(branch);
                } else {
                    exec_stack.push(false);
                }
            }
            OP_ELSE => {
                if exec_stack.is_empty() {
                    return Err(ScriptError::InvalidOpcode);
                }
                let current = exec_stack.pop().unwrap();
                exec_stack.push(!current);
            }
            OP_ENDIF => {
                if exec_stack.pop().is_none() {
                    return Err(ScriptError::InvalidOpcode);
                }
            }
            OP_DUP => {
                if !exec {
                    continue;
                }
                let top = stack.last().ok_or(ScriptError::StackUnderflow)?.clone();
                stack.push(top);
            }
            OP_DROP => {
                if !exec {
                    continue;
                }
                let _ = pop(stack)?;
            }
            OP_2DROP => {
                if !exec {
                    continue;
                }
                let _ = pop(stack)?;
                let _ = pop(stack)?;
            }
            OP_SIZE => {
                if !exec {
                    continue;
                }
                let len = stack.last().ok_or(ScriptError::StackUnderflow)?.len();
                stack.push(script_num_to_vec(len as i64));
            }
            OP_RIPEMD160 => {
                if !exec {
                    continue;
                }
                let data = pop(stack)?;
                let mut hasher = Ripemd160::new();
                hasher.update(data);
                stack.push(hasher.finalize().to_vec());
            }
            OP_SHA1 => {
                if !exec {
                    continue;
                }
                let data = pop(stack)?;
                let mut hasher = Sha1::new();
                hasher.update(data);
                stack.push(hasher.finalize().to_vec());
            }
            OP_SHA256 => {
                if !exec {
                    continue;
                }
                let data = pop(stack)?;
                stack.push(sha256(&data).to_vec());
            }
            OP_HASH160 => {
                if !exec {
                    continue;
                }
                let data = pop(stack)?;
                stack.push(hash160(&data));
            }
            OP_HASH256 => {
                if !exec {
                    continue;
                }
                let data = pop(stack)?;
                stack.push(sha256d(&data).to_vec());
            }
            OP_CODESEPARATOR => {
                if exec {
                    script_code_start = cursor;
                }
            }
            OP_EQUAL => {
                if !exec {
                    continue;
                }
                let a = pop(stack)?;
                let b = pop(stack)?;
                stack.push(bool_to_vec(a == b));
            }
            OP_EQUALVERIFY => {
                if !exec {
                    continue;
                }
                let a = pop(stack)?;
                let b = pop(stack)?;
                if a != b {
                    return Err(ScriptError::EvalFalse);
                }
            }
            OP_VERIFY => {
                if !exec {
                    continue;
                }
                let value = pop(stack)?;
                if !cast_to_bool(&value) {
                    return Err(ScriptError::EvalFalse);
                }
            }
            OP_CHECKLOCKTIMEVERIFY => {
                if !exec {
                    continue;
                }
                if (checker.flags & SCRIPT_VERIFY_CHECKLOCKTIMEVERIFY) != 0 {
                    let locktime_bytes = stack.last().ok_or(ScriptError::StackUnderflow)?;
                    let locktime = decode_script_num(locktime_bytes)?;
                    checker.check_lock_time(locktime)?;
                } else if (checker.flags & SCRIPT_VERIFY_DISCOURAGE_UPGRADABLE_NOPS) != 0 {
                    return Err(ScriptError::InvalidOpcode);
                }
            }
            OP_NOP1 | OP_NOP3 | OP_NOP4..=OP_NOP10 => {
                if !exec {
                    continue;
                }
                if (checker.flags & SCRIPT_VERIFY_DISCOURAGE_UPGRADABLE_NOPS) != 0 {
                    return Err(ScriptError::InvalidOpcode);
                }
            }
            OP_CHECKSIG | OP_CHECKSIGVERIFY => {
                if !exec {
                    continue;
                }
                let pubkey = pop(stack)?;
                let sig = pop(stack)?;
                let script_code = &script[script_code_start..];
                let ok = checker.check_sig(&sig, &pubkey, script_code)?;
                if opcode == OP_CHECKSIGVERIFY {
                    if !ok {
                        return Err(ScriptError::SigCheck);
                    }
                } else {
                    stack.push(bool_to_vec(ok));
                }
            }
            OP_CHECKMULTISIG | OP_CHECKMULTISIGVERIFY => {
                if !exec {
                    continue;
                }
                let n = decode_script_num(&pop(stack)?)? as i64;
                if n < 0 || n > 20 {
                    return Err(ScriptError::InvalidOpcode);
                }
                let mut pubkeys = Vec::with_capacity(n as usize);
                for _ in 0..n {
                    pubkeys.push(pop(stack)?);
                }
                pubkeys.reverse();
                let m = decode_script_num(&pop(stack)?)? as i64;
                if m < 0 || m > n {
                    return Err(ScriptError::InvalidOpcode);
                }
                let mut sigs = Vec::with_capacity(m as usize);
                for _ in 0..m {
                    sigs.push(pop(stack)?);
                }
                sigs.reverse();

                let dummy = pop(stack)?;
                if (checker.flags & SCRIPT_VERIFY_NULLDUMMY) != 0 && !dummy.is_empty() {
                    return Err(ScriptError::NullDummy);
                }

                let mut sig_index = 0usize;
                let mut key_index = 0usize;
                while sig_index < sigs.len() && key_index < pubkeys.len() {
                    let sig = &sigs[sig_index];
                    let key = &pubkeys[key_index];
                    let script_code = &script[script_code_start..];
                    let ok = checker.check_sig(sig, key, script_code)?;
                    if ok {
                        sig_index += 1;
                    }
                    key_index += 1;
                    if pubkeys.len() - key_index < sigs.len() - sig_index {
                        break;
                    }
                }

                let success = sig_index == sigs.len();
                if opcode == OP_CHECKMULTISIGVERIFY {
                    if !success {
                        return Err(ScriptError::SigCheck);
                    }
                } else {
                    stack.push(bool_to_vec(success));
                }
            }
            OP_RETURN => {
                if exec {
                    return Err(ScriptError::EvalFalse);
                }
            }
            _ => {
                if exec {
                    eprintln!(
                        "invalid opcode 0x{opcode:02x} in script {}",
                        bytes_to_hex(script)
                    );
                    return Err(ScriptError::InvalidOpcode);
                }
            }
        }
    }

    if !exec_stack.is_empty() {
        return Err(ScriptError::ScriptError("unbalanced conditional"));
    }

    Ok(())
}

fn pop(stack: &mut Vec<Vec<u8>>) -> Result<Vec<u8>, ScriptError> {
    stack.pop().ok_or(ScriptError::StackUnderflow)
}

fn bool_to_vec(value: bool) -> Vec<u8> {
    if value {
        vec![1]
    } else {
        Vec::new()
    }
}

fn cast_to_bool(data: &[u8]) -> bool {
    for (index, byte) in data.iter().enumerate() {
        if *byte != 0 {
            return !(index == data.len() - 1 && *byte == 0x80);
        }
    }
    false
}

fn is_p2sh(script_pubkey: &[u8]) -> bool {
    script_pubkey.len() == 23
        && script_pubkey[0] == OP_HASH160
        && script_pubkey[1] == 0x14
        && script_pubkey[22] == OP_EQUAL
}

fn is_push_only(script: &[u8]) -> bool {
    let mut cursor = 0usize;
    while cursor < script.len() {
        let opcode = script[cursor];
        cursor += 1;
        let len = match opcode {
            0x01..=0x4b => opcode as usize,
            OP_PUSHDATA1 => read_u8(script, &mut cursor).map(|v| v as usize).unwrap_or(usize::MAX),
            OP_PUSHDATA2 => read_u16(script, &mut cursor).map(|v| v as usize).unwrap_or(usize::MAX),
            OP_PUSHDATA4 => read_u32(script, &mut cursor).map(|v| v as usize).unwrap_or(usize::MAX),
            OP_0 | OP_1NEGATE | OP_1..=OP_16 => 0,
            _ => return false,
        };
        if len > 0 {
            if cursor + len > script.len() {
                return false;
            }
            cursor += len;
        }
    }
    true
}

fn read_bytes(script: &[u8], cursor: &mut usize, len: usize) -> Result<Vec<u8>, ScriptError> {
    if *cursor + len > script.len() {
        return Err(ScriptError::StackUnderflow);
    }
    let out = script[*cursor..*cursor + len].to_vec();
    *cursor += len;
    Ok(out)
}

fn read_u8(script: &[u8], cursor: &mut usize) -> Result<u8, ScriptError> {
    if *cursor >= script.len() {
        return Err(ScriptError::StackUnderflow);
    }
    let out = script[*cursor];
    *cursor += 1;
    Ok(out)
}

fn read_u16(script: &[u8], cursor: &mut usize) -> Result<u16, ScriptError> {
    let bytes = read_bytes(script, cursor, 2)?;
    Ok(u16::from_le_bytes([bytes[0], bytes[1]]))
}

fn read_u32(script: &[u8], cursor: &mut usize) -> Result<u32, ScriptError> {
    let bytes = read_bytes(script, cursor, 4)?;
    Ok(u32::from_le_bytes([bytes[0], bytes[1], bytes[2], bytes[3]]))
}

fn decode_script_num(data: &[u8]) -> Result<i64, ScriptError> {
    if data.is_empty() {
        return Ok(0);
    }
    if data.len() > 4 {
        return Err(ScriptError::InvalidOpcode);
    }
    let mut result: i64 = 0;
    for (i, byte) in data.iter().enumerate() {
        result |= (*byte as i64) << (8 * i);
    }
    let last = *data.last().unwrap();
    if (last & 0x80) != 0 {
        let mask = !(0x80i64 << (8 * (data.len() - 1)));
        result &= mask;
        result = -result;
    }
    Ok(result)
}

fn script_num_to_vec(value: i64) -> Vec<u8> {
    if value == 0 {
        return Vec::new();
    }
    let mut abs = value.abs() as u64;
    let mut result = Vec::new();
    while abs > 0 {
        result.push((abs & 0xff) as u8);
        abs >>= 8;
    }
    let sign_bit = 0x80u8;
    if let Some(last) = result.last_mut() {
        if (*last & sign_bit) != 0 {
            result.push(if value < 0 { sign_bit } else { 0 });
        } else if value < 0 {
            *last |= sign_bit;
        }
    }
    result
}

fn hash160(data: &[u8]) -> Vec<u8> {
    let sha = sha256(data);
    let mut hasher = Ripemd160::new();
    hasher.update(sha);
    hasher.finalize().to_vec()
}

fn is_valid_pubkey(data: &[u8]) -> bool {
    match data.len() {
        33 => data[0] == 0x02 || data[0] == 0x03,
        65 => data[0] == 0x04,
        _ => false,
    }
}

fn check_minimal_push(data: &[u8], opcode: u8) -> bool {
    if data.is_empty() {
        return opcode == OP_0;
    }
    if data.len() == 1 && (1..=16).contains(&data[0]) {
        return opcode == OP_1 + (data[0] - 1);
    }
    if data.len() == 1 && data[0] == 0x81 {
        return opcode == OP_1NEGATE;
    }
    if data.len() <= 75 {
        return opcode == data.len() as u8;
    }
    if data.len() <= 255 {
        return opcode == OP_PUSHDATA1;
    }
    if data.len() <= 65535 {
        return opcode == OP_PUSHDATA2;
    }
    true
}

fn bytes_to_hex(bytes: &[u8]) -> String {
    use std::fmt::Write;

    let mut out = String::with_capacity(bytes.len() * 2);
    for byte in bytes {
        let _ = write!(out, "{:02x}", byte);
    }
    out
}
