use fluxd_primitives::encoding::{Decoder, Encoder};

pub const META_BLOCK_FILES_LAST_FILE_KEY: &[u8] = b"flatfiles:blocks:last_file";
pub const META_BLOCK_FILES_LAST_LEN_KEY: &[u8] = b"flatfiles:blocks:last_len";
pub const META_UNDO_FILES_LAST_FILE_KEY: &[u8] = b"flatfiles:undo:last_file";
pub const META_UNDO_FILES_LAST_LEN_KEY: &[u8] = b"flatfiles:undo:last_len";
pub const META_BLOCK_FILES_PRUNED_FILE_KEY: &[u8] = b"flatfiles:blocks:pruned_file";
pub const META_UNDO_FILES_PRUNED_FILE_KEY: &[u8] = b"flatfiles:undo:pruned_file";

const META_BLOCK_FILE_INFO_PREFIX: &[u8] = b"flatfiles:blocks:file:";
const META_UNDO_FILE_INFO_PREFIX: &[u8] = b"flatfiles:undo:file:";

pub const FLATFILE_INFO_FLAG_PRUNED: u32 = 1 << 0;

#[derive(Clone, Copy, Debug, Default, Eq, PartialEq)]
pub struct FlatFileInfo {
    pub blocks: u32,
    pub size: u64,
    pub height_first: i32,
    pub height_last: i32,
    pub time_first: u32,
    pub time_last: u32,
    pub flags: u32,
}

impl FlatFileInfo {
    pub fn encode(&self) -> Vec<u8> {
        let mut encoder = Encoder::new();
        encoder.write_u32_le(self.blocks);
        encoder.write_u64_le(self.size);
        encoder.write_i32_le(self.height_first);
        encoder.write_i32_le(self.height_last);
        encoder.write_u32_le(self.time_first);
        encoder.write_u32_le(self.time_last);
        encoder.write_u32_le(self.flags);
        encoder.into_inner()
    }

    pub fn decode(bytes: &[u8]) -> Option<Self> {
        let mut decoder = Decoder::new(bytes);
        let blocks = decoder.read_u32_le().ok()?;
        let size = decoder.read_u64_le().ok()?;
        let height_first = decoder.read_i32_le().ok()?;
        let height_last = decoder.read_i32_le().ok()?;
        let time_first = decoder.read_u32_le().ok()?;
        let time_last = decoder.read_u32_le().ok()?;
        let flags = decoder.read_u32_le().ok()?;
        if !decoder.is_empty() {
            return None;
        }
        Some(Self {
            blocks,
            size,
            height_first,
            height_last,
            time_first,
            time_last,
            flags,
        })
    }
}

pub fn block_file_info_key(file_id: u32) -> [u8; META_BLOCK_FILE_INFO_PREFIX.len() + 4] {
    let mut key = [0u8; META_BLOCK_FILE_INFO_PREFIX.len() + 4];
    key[0..META_BLOCK_FILE_INFO_PREFIX.len()].copy_from_slice(META_BLOCK_FILE_INFO_PREFIX);
    key[META_BLOCK_FILE_INFO_PREFIX.len()..].copy_from_slice(&file_id.to_le_bytes());
    key
}

pub fn undo_file_info_key(file_id: u32) -> [u8; META_UNDO_FILE_INFO_PREFIX.len() + 4] {
    let mut key = [0u8; META_UNDO_FILE_INFO_PREFIX.len() + 4];
    key[0..META_UNDO_FILE_INFO_PREFIX.len()].copy_from_slice(META_UNDO_FILE_INFO_PREFIX);
    key[META_UNDO_FILE_INFO_PREFIX.len()..].copy_from_slice(&file_id.to_le_bytes());
    key
}

pub fn parse_block_file_info_key(key: &[u8]) -> Option<u32> {
    if key.len() != META_BLOCK_FILE_INFO_PREFIX.len() + 4 {
        return None;
    }
    if !key.starts_with(META_BLOCK_FILE_INFO_PREFIX) {
        return None;
    }
    let mut id_bytes = [0u8; 4];
    id_bytes.copy_from_slice(&key[META_BLOCK_FILE_INFO_PREFIX.len()..]);
    Some(u32::from_le_bytes(id_bytes))
}

pub fn parse_undo_file_info_key(key: &[u8]) -> Option<u32> {
    if key.len() != META_UNDO_FILE_INFO_PREFIX.len() + 4 {
        return None;
    }
    if !key.starts_with(META_UNDO_FILE_INFO_PREFIX) {
        return None;
    }
    let mut id_bytes = [0u8; 4];
    id_bytes.copy_from_slice(&key[META_UNDO_FILE_INFO_PREFIX.len()..]);
    Some(u32::from_le_bytes(id_bytes))
}
