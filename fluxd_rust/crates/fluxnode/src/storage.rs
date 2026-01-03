use fluxd_consensus::Hash256;
use fluxd_primitives::encoding::{Decodable, DecodeError, Decoder, Encodable, Encoder};
use fluxd_primitives::hash::sha256d;
use fluxd_primitives::outpoint::OutPoint;

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct KeyId(pub Hash256);

pub fn dedupe_key(bytes: &[u8]) -> KeyId {
    KeyId(sha256d(bytes))
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub struct FluxnodeRecord {
    pub collateral: OutPoint,
    pub tier: u8,
    pub start_height: u32,
    pub confirmed_height: u32,
    pub last_confirmed_height: u32,
    pub last_paid_height: u32,
    pub collateral_value: i64,
    pub operator_pubkey: KeyId,
    pub collateral_pubkey: Option<KeyId>,
    pub p2sh_script: Option<KeyId>,
    pub ip: String,
}

impl FluxnodeRecord {
    pub fn encode(&self) -> Vec<u8> {
        let mut encoder = Encoder::new();
        self.collateral.consensus_encode(&mut encoder);
        encoder.write_u8(self.tier);
        encoder.write_u32_le(self.start_height);
        encoder.write_u32_le(self.last_confirmed_height);
        encoder.write_u32_le(self.last_paid_height);
        encoder.write_bytes(&self.operator_pubkey.0);
        write_optional_key(&mut encoder, self.collateral_pubkey);
        write_optional_key(&mut encoder, self.p2sh_script);
        encoder.write_u32_le(self.confirmed_height);
        encoder.write_i64_le(self.collateral_value);
        encoder.write_var_str(&self.ip);
        encoder.into_inner()
    }

    pub fn decode(bytes: &[u8]) -> Result<Self, DecodeError> {
        let mut decoder = Decoder::new(bytes);
        let collateral = OutPoint::consensus_decode(&mut decoder)?;
        let tier = decoder.read_u8()?;
        let start_height = decoder.read_u32_le()?;
        let last_confirmed_height = decoder.read_u32_le()?;
        let last_paid_height = decoder.read_u32_le()?;
        let operator_pubkey = KeyId(decoder.read_fixed::<32>()?);
        let collateral_pubkey = read_optional_key(&mut decoder)?;
        let p2sh_script = read_optional_key(&mut decoder)?;
        let (confirmed_height, collateral_value) = if decoder.is_empty() {
            (
                if last_confirmed_height != start_height {
                    last_confirmed_height
                } else {
                    0
                },
                0,
            )
        } else {
            let confirmed_height = decoder.read_u32_le()?;
            let collateral_value = decoder.read_i64_le()?;
            (confirmed_height, collateral_value)
        };
        let ip = if decoder.is_empty() {
            String::new()
        } else {
            decoder.read_var_str()?
        };
        if !decoder.is_empty() {
            return Err(DecodeError::TrailingBytes);
        }
        Ok(Self {
            collateral,
            tier,
            start_height,
            confirmed_height,
            last_confirmed_height,
            last_paid_height,
            collateral_value,
            operator_pubkey,
            collateral_pubkey,
            p2sh_script,
            ip,
        })
    }
}

fn write_optional_key(encoder: &mut Encoder, key: Option<KeyId>) {
    match key {
        Some(key) => {
            encoder.write_u8(1);
            encoder.write_bytes(&key.0);
        }
        None => encoder.write_u8(0),
    }
}

fn read_optional_key(decoder: &mut Decoder) -> Result<Option<KeyId>, DecodeError> {
    let flag = decoder.read_u8()?;
    if flag == 0 {
        Ok(None)
    } else {
        Ok(Some(KeyId(decoder.read_fixed::<32>()?)))
    }
}
