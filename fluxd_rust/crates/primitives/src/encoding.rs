use fluxd_consensus::Hash256;

#[derive(Default)]
pub struct Encoder {
    buf: Vec<u8>,
}

impl Encoder {
    pub fn new() -> Self {
        Self::default()
    }

    pub fn into_inner(self) -> Vec<u8> {
        self.buf
    }

    pub fn write_u8(&mut self, value: u8) {
        self.buf.push(value);
    }

    pub fn write_i8(&mut self, value: i8) {
        self.buf.push(value as u8);
    }

    pub fn write_u16_le(&mut self, value: u16) {
        self.buf.extend_from_slice(&value.to_le_bytes());
    }

    pub fn write_u32_le(&mut self, value: u32) {
        self.buf.extend_from_slice(&value.to_le_bytes());
    }

    pub fn write_i32_le(&mut self, value: i32) {
        self.buf.extend_from_slice(&value.to_le_bytes());
    }

    pub fn write_u64_le(&mut self, value: u64) {
        self.buf.extend_from_slice(&value.to_le_bytes());
    }

    pub fn write_i64_le(&mut self, value: i64) {
        self.buf.extend_from_slice(&value.to_le_bytes());
    }

    pub fn write_bytes(&mut self, bytes: &[u8]) {
        self.buf.extend_from_slice(bytes);
    }

    pub fn write_varint(&mut self, value: u64) {
        if value < 0xfd {
            self.write_u8(value as u8);
        } else if value <= 0xffff {
            self.write_u8(0xfd);
            self.write_u16_le(value as u16);
        } else if value <= 0xffff_ffff {
            self.write_u8(0xfe);
            self.write_u32_le(value as u32);
        } else {
            self.write_u8(0xff);
            self.write_u64_le(value);
        }
    }

    pub fn write_var_bytes(&mut self, bytes: &[u8]) {
        self.write_varint(bytes.len() as u64);
        self.write_bytes(bytes);
    }

    pub fn write_var_str(&mut self, value: &str) {
        self.write_var_bytes(value.as_bytes());
    }

    pub fn write_hash_le(&mut self, hash: &Hash256) {
        self.buf.extend_from_slice(hash);
    }
}

const MAX_COMPACT_SIZE: u64 = 0x0200_0000;

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum DecodeError {
    UnexpectedEof,
    NonCanonicalVarInt,
    SizeTooLarge,
    InvalidData(&'static str),
    TrailingBytes,
}

impl std::fmt::Display for DecodeError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            DecodeError::UnexpectedEof => write!(f, "unexpected end of input"),
            DecodeError::NonCanonicalVarInt => write!(f, "non-canonical CompactSize"),
            DecodeError::SizeTooLarge => write!(f, "compact size exceeds maximum"),
            DecodeError::InvalidData(message) => write!(f, "{message}"),
            DecodeError::TrailingBytes => write!(f, "trailing bytes after decode"),
        }
    }
}

impl std::error::Error for DecodeError {}

pub struct Decoder<'a> {
    input: &'a [u8],
    cursor: usize,
}

impl<'a> Decoder<'a> {
    pub fn new(input: &'a [u8]) -> Self {
        Self { input, cursor: 0 }
    }

    pub fn remaining(&self) -> usize {
        self.input.len().saturating_sub(self.cursor)
    }

    pub fn is_empty(&self) -> bool {
        self.cursor >= self.input.len()
    }

    fn read_slice(&mut self, len: usize) -> Result<&'a [u8], DecodeError> {
        if self.remaining() < len {
            return Err(DecodeError::UnexpectedEof);
        }
        let start = self.cursor;
        self.cursor += len;
        Ok(&self.input[start..start + len])
    }

    pub fn read_u8(&mut self) -> Result<u8, DecodeError> {
        Ok(self.read_slice(1)?[0])
    }

    pub fn read_i8(&mut self) -> Result<i8, DecodeError> {
        Ok(self.read_u8()? as i8)
    }

    pub fn read_u16_le(&mut self) -> Result<u16, DecodeError> {
        let bytes = self.read_slice(2)?;
        Ok(u16::from_le_bytes([bytes[0], bytes[1]]))
    }

    pub fn read_u32_le(&mut self) -> Result<u32, DecodeError> {
        let bytes = self.read_slice(4)?;
        Ok(u32::from_le_bytes([bytes[0], bytes[1], bytes[2], bytes[3]]))
    }

    pub fn read_i32_le(&mut self) -> Result<i32, DecodeError> {
        Ok(self.read_u32_le()? as i32)
    }

    pub fn read_u64_le(&mut self) -> Result<u64, DecodeError> {
        let bytes = self.read_slice(8)?;
        Ok(u64::from_le_bytes([
            bytes[0], bytes[1], bytes[2], bytes[3], bytes[4], bytes[5], bytes[6], bytes[7],
        ]))
    }

    pub fn read_i64_le(&mut self) -> Result<i64, DecodeError> {
        Ok(self.read_u64_le()? as i64)
    }

    pub fn read_fixed<const N: usize>(&mut self) -> Result<[u8; N], DecodeError> {
        let bytes = self.read_slice(N)?;
        let mut out = [0u8; N];
        out.copy_from_slice(bytes);
        Ok(out)
    }

    pub fn read_bytes(&mut self, len: usize) -> Result<Vec<u8>, DecodeError> {
        Ok(self.read_slice(len)?.to_vec())
    }

    pub fn read_varint(&mut self) -> Result<u64, DecodeError> {
        let prefix = self.read_u8()? as u64;
        let value = if prefix < 0xfd {
            prefix
        } else if prefix == 0xfd {
            let value = self.read_u16_le()? as u64;
            if value < 0xfd {
                return Err(DecodeError::NonCanonicalVarInt);
            }
            value
        } else if prefix == 0xfe {
            let value = self.read_u32_le()? as u64;
            if value < 0x1_0000 {
                return Err(DecodeError::NonCanonicalVarInt);
            }
            value
        } else {
            let value = self.read_u64_le()?;
            if value < 0x1_0000_0000 {
                return Err(DecodeError::NonCanonicalVarInt);
            }
            value
        };

        if value > MAX_COMPACT_SIZE {
            return Err(DecodeError::SizeTooLarge);
        }
        Ok(value)
    }

    pub fn read_var_bytes(&mut self) -> Result<Vec<u8>, DecodeError> {
        let len = self.read_varint()?;
        let len = usize::try_from(len).map_err(|_| DecodeError::SizeTooLarge)?;
        self.read_bytes(len)
    }

    pub fn read_var_str(&mut self) -> Result<String, DecodeError> {
        let bytes = self.read_var_bytes()?;
        String::from_utf8(bytes).map_err(|_| DecodeError::InvalidData("invalid utf8 string"))
    }

    pub fn read_bool(&mut self) -> Result<bool, DecodeError> {
        Ok(self.read_u8()? != 0)
    }

    pub fn read_hash_le(&mut self) -> Result<Hash256, DecodeError> {
        let bytes = self.read_slice(32)?;
        Ok(bytes.try_into().expect("read_slice length"))
    }
}

pub trait Encodable {
    fn consensus_encode(&self, encoder: &mut Encoder);
}

pub trait Decodable: Sized {
    fn consensus_decode(decoder: &mut Decoder) -> Result<Self, DecodeError>;
}

pub fn encode<T: Encodable>(value: &T) -> Vec<u8> {
    let mut encoder = Encoder::new();
    value.consensus_encode(&mut encoder);
    encoder.into_inner()
}

pub fn decode<T: Decodable>(bytes: &[u8]) -> Result<T, DecodeError> {
    let mut decoder = Decoder::new(bytes);
    let value = T::consensus_decode(&mut decoder)?;
    if !decoder.is_empty() {
        return Err(DecodeError::TrailingBytes);
    }
    Ok(value)
}
