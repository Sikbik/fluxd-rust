use std::fs::{File, OpenOptions};
use std::io::{Read, Seek, SeekFrom, Write};
use std::path::{Path, PathBuf};
use std::sync::Mutex;

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct FileLocation {
    pub file_id: u32,
    pub offset: u64,
    pub len: u32,
}

impl FileLocation {
    pub fn encode(&self) -> [u8; 16] {
        let mut out = [0u8; 16];
        out[0..4].copy_from_slice(&self.file_id.to_le_bytes());
        out[4..12].copy_from_slice(&self.offset.to_le_bytes());
        out[12..16].copy_from_slice(&self.len.to_le_bytes());
        out
    }

    pub fn decode(bytes: &[u8]) -> Option<Self> {
        if bytes.len() != 16 {
            return None;
        }
        let file_id = u32::from_le_bytes(bytes[0..4].try_into().ok()?);
        let offset = u64::from_le_bytes(bytes[4..12].try_into().ok()?);
        let len = u32::from_le_bytes(bytes[12..16].try_into().ok()?);
        Some(Self {
            file_id,
            offset,
            len,
        })
    }
}

#[derive(Debug)]
pub enum FlatFileError {
    Io(std::io::Error),
    InvalidLocation,
    LengthMismatch,
}

impl std::fmt::Display for FlatFileError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            FlatFileError::Io(err) => write!(f, "{err}"),
            FlatFileError::InvalidLocation => write!(f, "invalid flatfile location"),
            FlatFileError::LengthMismatch => write!(f, "flatfile length mismatch"),
        }
    }
}

impl std::error::Error for FlatFileError {}

impl From<std::io::Error> for FlatFileError {
    fn from(err: std::io::Error) -> Self {
        FlatFileError::Io(err)
    }
}

pub struct FlatFileStore {
    dir: PathBuf,
    prefix: String,
    max_file_size: u64,
    state: Mutex<FlatFileState>,
}

#[derive(Debug)]
struct FlatFileState {
    current_file: u32,
    current_len: u64,
}

impl FlatFileStore {
    pub fn new(dir: impl Into<PathBuf>, max_file_size: u64) -> Result<Self, FlatFileError> {
        Self::new_with_prefix(dir, "data", max_file_size)
    }

    pub fn new_with_prefix(
        dir: impl Into<PathBuf>,
        prefix: impl Into<String>,
        max_file_size: u64,
    ) -> Result<Self, FlatFileError> {
        let dir = dir.into();
        let prefix = prefix.into();
        std::fs::create_dir_all(&dir)?;
        let (current_file, current_len) = Self::locate_active_file(&dir, &prefix, max_file_size)?;
        Ok(Self {
            dir,
            prefix,
            max_file_size,
            state: Mutex::new(FlatFileState {
                current_file,
                current_len,
            }),
        })
    }

    pub fn append(&self, bytes: &[u8]) -> Result<FileLocation, FlatFileError> {
        let mut state = self.state.lock().expect("flatfile lock");
        let needed = 4u64 + bytes.len() as u64;
        if state.current_len + needed > self.max_file_size {
            state.current_file += 1;
            state.current_len = 0;
        }
        let offset = state.current_len;
        let path = self.file_path(state.current_file);
        let mut file = OpenOptions::new().create(true).append(true).open(&path)?;
        let len = bytes.len() as u32;
        file.write_all(&len.to_le_bytes())?;
        file.write_all(bytes)?;
        file.flush()?;
        state.current_len += needed;
        Ok(FileLocation {
            file_id: state.current_file,
            offset,
            len,
        })
    }

    pub fn read(&self, location: FileLocation) -> Result<Vec<u8>, FlatFileError> {
        if location.len == 0 {
            return Err(FlatFileError::InvalidLocation);
        }
        let path = self.file_path(location.file_id);
        let mut file = File::open(&path)?;
        file.seek(SeekFrom::Start(location.offset))?;
        let mut len_bytes = [0u8; 4];
        file.read_exact(&mut len_bytes)?;
        let stored_len = u32::from_le_bytes(len_bytes);
        if stored_len != location.len {
            return Err(FlatFileError::LengthMismatch);
        }
        let mut buffer = vec![0u8; stored_len as usize];
        file.read_exact(&mut buffer)?;
        Ok(buffer)
    }

    fn file_path(&self, file_id: u32) -> PathBuf {
        self.dir.join(format!("{}{file_id:05}.dat", self.prefix))
    }

    fn locate_active_file(
        dir: &Path,
        prefix: &str,
        max_file_size: u64,
    ) -> Result<(u32, u64), FlatFileError> {
        let mut file_id = 0u32;
        let mut last_existing: Option<(u32, u64)> = None;
        loop {
            let path = dir.join(format!("{prefix}{file_id:05}.dat"));
            if !path.exists() {
                break;
            }
            let metadata = std::fs::metadata(&path)?;
            let len = metadata.len();
            last_existing = Some((file_id, len));
            file_id += 1;
        }

        match last_existing {
            Some((last_id, len)) => {
                if len >= max_file_size {
                    Ok((last_id + 1, 0))
                } else {
                    Ok((last_id, len))
                }
            }
            None => Ok((0, 0)),
        }
    }
}
