//! Framed zstd payload store for the large fields of session records.
//!
//! `payloads.bin` is a flat sequence of length-prefixed records:
//!
//! ```text
//! record := varint(id_len) | id_bytes | varint(frame_len) | zstd_frame
//! ```
//!
//! Only the two large fields (`ToolResult.content`, `Image.source.data`) move here;
//! everything else stays inline in `tree.jsonl`. The file is the sole compressed
//! region of a session. On session open a single pass builds an in-memory index
//! mapping `payloadId -> (frame_offset, frame_len)` by reading only the length
//! prefixes — no frame is decompressed to build the index, so the scan is
//! O(records) seeks, not O(bytes) decode.

use std::collections::HashMap;
use std::fs::{File, OpenOptions};
use std::io::{self, BufReader, Read, Seek, SeekFrom, Write};
use std::path::{Path, PathBuf};

use serde::{Serialize, Deserialize};
use serde_json::Value;

use crate::sessions::SessionError;
use crate::StorageError;

const ZSTD_LEVEL: i32 = 3;
const PAYLOADS_FILE: &str = "payloads.bin";

/// A content-addressed payload identifier with a branded prefix (`pld_`).
/// Random tail (UUIDv7 + random) lets ids serve as keys without a collision space.
#[derive(Debug, Clone, PartialEq, Eq, Hash, Serialize, Deserialize)]
pub struct PayloadId(String);

impl PayloadId {
    pub fn new() -> Self {
        let id = uuid::Uuid::now_v7().to_string();
        let encoded = bs58::encode(id).into_string();
        Self(format!("pld_{encoded}"))
    }

    pub fn as_str(&self) -> &str {
        &self.0
    }
}

impl Default for PayloadId {
    fn default() -> Self {
        Self::new()
    }
}

/// Trait bridging the storage layer and the concrete message/output types it
/// cannot name. `maki-storage` is generic over `<M, U, T>` and must not depend on
/// `maki-providers`, so the externalization of large fields into the payload
/// store is delegated to each concrete type's implementation.
///
/// `encode` serializes self into an inline JSON value, spilling large blobs into
/// the writer and holding a `payload_id` in their place. `decode` rehydrates by
/// paging blobs back from the reader.
pub trait StoredInTree: Sized {
    fn encode(&self, writer: &mut PayloadWriter) -> Result<Value, SessionError>;
    fn decode(value: Value, reader: &PayloadReader) -> Result<Self, SessionError>;
}

/// Blanket impl: `serde_json::Value` is stored verbatim (no externalization).
/// Keeps storage tests type-agnostic without a real provider dependency.
impl StoredInTree for Value {
    fn encode(&self, _writer: &mut PayloadWriter) -> Result<Value, SessionError> {
        Ok(self.clone())
    }

    fn decode(value: Value, _reader: &PayloadReader) -> Result<Self, SessionError> {
        Ok(value)
    }
}

#[derive(Debug, Clone, Copy)]
struct FrameLoc {
    offset: u64,
    len: u32,
}

/// Append-only writer for the payload store. Frames are independent zstd-3 blobs;
/// a torn trailing frame loses only that payload (see durability §14).
pub struct PayloadWriter {
    file: File,
    dir: PathBuf,
    index: HashMap<String, FrameLoc>,
    next_offset: u64,
}

impl PayloadWriter {
    pub fn open(dir: &Path) -> Result<Self, SessionError> {
        std::fs::create_dir_all(dir).map_err(StorageError::from)?;
        let path = dir.join(PAYLOADS_FILE);
        let file = OpenOptions::new()
            .create(true)
            .append(true)
            .open(&path)
            .map_err(StorageError::from)?;
        let next_offset = file.metadata().map_err(StorageError::from)?.len();
        Ok(Self {
            file,
            dir: dir.to_path_buf(),
            index: HashMap::new(),
            next_offset,
        })
    }

    /// Encode a blob into a self-contained zstd frame and append it, returning a
    /// fresh payload id that references the frame.
    pub fn write(&mut self, blob: &[u8]) -> Result<PayloadId, SessionError> {
        let id = PayloadId::new();
        let frame = zrip::compress(blob, ZSTD_LEVEL)
            .map_err(|e| SessionError::Zstd(e.to_string()))?;
        let mut buf = Vec::with_capacity(16 + id.as_str().len() + frame.len());
        write_varint(&mut buf, id.as_str().len() as u64);
        buf.extend_from_slice(id.as_str().as_bytes());
        write_varint(&mut buf, frame.len() as u64);
        let frame_offset = self.next_offset + buf.len() as u64;
        buf.extend_from_slice(&frame);
        self.file.write_all(&buf).map_err(StorageError::from)?;
        self.file.sync_data().map_err(StorageError::from)?;
        self.index.insert(
            id.as_str().to_owned(),
            FrameLoc {
                offset: frame_offset,
                len: frame.len() as u32,
            },
        );
        self.next_offset += buf.len() as u64;
        Ok(id)
    }

    /// Adopt an existing frame (byte-for-byte copy from another store) without
    /// recompression. Used by fork/migration where re-encoding would lose info.
    pub fn write_raw_frame(&mut self, id: &PayloadId, frame: &[u8]) -> Result<(), SessionError> {
        let mut buf = Vec::with_capacity(16 + id.as_str().len() + frame.len());
        write_varint(&mut buf, id.as_str().len() as u64);
        buf.extend_from_slice(id.as_str().as_bytes());
        write_varint(&mut buf, frame.len() as u64);
        let frame_offset = self.next_offset + buf.len() as u64;
        buf.extend_from_slice(frame);
        self.file.write_all(&buf).map_err(StorageError::from)?;
        self.file.sync_data().map_err(StorageError::from)?;
        self.index.insert(
            id.as_str().to_owned(),
            FrameLoc {
                offset: frame_offset,
                len: frame.len() as u32,
            },
        );
        self.next_offset += buf.len() as u64;
        Ok(())
    }

    pub fn dir(&self) -> &Path {
        &self.dir
    }
}

/// Read-side view over the payload store: O(1) paging by id.
/// Built once on open by scanning only the length prefixes.
pub struct PayloadReader {
    file: File,
    index: HashMap<String, FrameLoc>,
}

impl PayloadReader {
    pub fn open(dir: &Path) -> Result<Self, SessionError> {
        let path = dir.join(PAYLOADS_FILE);
        let file = File::open(&path).map_err(StorageError::from)?;
        let index = build_index(&file)?;
        Ok(Self { file, index })
    }

    pub fn exists(dir: &Path) -> bool {
        dir.join(PAYLOADS_FILE).exists()
    }

    fn loc(&self, id: &str) -> Result<FrameLoc, SessionError> {
        self.index
            .get(id)
            .copied()
            .ok_or_else(|| SessionError::MissingPayload(id.to_string()))
    }

    /// Page and decompress exactly one payload by id.
    pub fn read(&self, id: &str) -> Result<Vec<u8>, SessionError> {
        let loc = self.loc(id)?;
        let mut reader = BufReader::new(&self.file);
        reader
            .seek(SeekFrom::Start(loc.offset))
            .map_err(StorageError::from)?;
        let mut buf = vec![0u8; loc.len as usize];
        reader.read_exact(&mut buf).map_err(StorageError::from)?;
        zrip::decompress(&buf).map_err(|e| SessionError::Zstd(e.to_string()))
    }
}

/// Build the payload index by scanning only the length prefixes of each record.
/// On a torn trailing record (frame_len runs past EOF) the scan stops at the
/// last complete record (durability §14 rule 2).
fn build_index(file: &File) -> Result<HashMap<String, FrameLoc>, SessionError> {
    let mut index = HashMap::new();
    let mut reader = BufReader::new(file);
    let mut offset: u64 = 0;
    loop {
        let id_len = match read_varint(&mut reader) {
            Ok(n) => n,
            Err(e) if e.kind() == io::ErrorKind::UnexpectedEof => break,
            Err(e) => return Err(StorageError::from(e).into()),
        };
        let mut id_buf = vec![0u8; id_len as usize];
        match reader.read_exact(&mut id_buf) {
            Ok(()) => {}
            Err(e) if e.kind() == io::ErrorKind::UnexpectedEof => break,
            Err(e) => return Err(StorageError::from(e).into()),
        }
        offset += varint_len(id_len) + id_len;
        let frame_len = match read_varint(&mut reader) {
            Ok(n) => n,
            Err(e) if e.kind() == io::ErrorKind::UnexpectedEof => break,
            Err(e) => return Err(StorageError::from(e).into()),
        };
        let frame_offset = offset + varint_len(frame_len);
        if let Some(c) = id_buf.iter().position(|&b| b == 0) {
            id_buf.truncate(c);
        }
        let id = String::from_utf8(id_buf)
            .map_err(|e| SessionError::CorruptPayload(e.into_bytes()))?;
        index.insert(
            id,
            FrameLoc {
                offset: frame_offset,
                len: frame_len as u32,
            },
        );
        match reader.seek(SeekFrom::Current(frame_len as i64)) {
            Ok(n) => offset = n,
            Err(e) if e.kind() == io::ErrorKind::UnexpectedEof => break,
            Err(e) => return Err(StorageError::from(e).into()),
        }
    }
    Ok(index)
}

fn write_varint(buf: &mut Vec<u8>, mut value: u64) {
    while value >= 0x80 {
        buf.push((value as u8) | 0x80);
        value >>= 7;
    }
    buf.push(value as u8);
}

fn varint_len(mut value: u64) -> u64 {
    let mut n = 1u64;
    while value >= 0x80 {
        n += 1;
        value >>= 7;
    }
    n
}

fn read_varint(reader: &mut impl Read) -> io::Result<u64> {
    let mut result = 0u64;
    let mut shift = 0u32;
    loop {
        let mut byte = [0u8; 1];
        reader.read_exact(&mut byte)?;
        let b = byte[0];
        result |= ((b & 0x7f) as u64) << shift;
        if b & 0x80 == 0 {
            return Ok(result);
        }
        shift += 7;
        if shift >= 64 {
            return Err(io::Error::new(io::ErrorKind::InvalidData, "varint overflow"));
        }
    }
}

/// Truncate the payload file to the last complete record boundary. Used on open
/// to recover from a torn trailing frame (durability §14 rule 2).
pub fn truncate_to_last_complete(dir: &Path) -> Result<u64, SessionError> {
    let path = dir.join(PAYLOADS_FILE);
    let file = File::open(&path).map_err(StorageError::from)?;
    let mut reader = BufReader::new(&file);
    let total = file.metadata().map_err(StorageError::from)?.len();
    let mut offset: u64 = 0;
    let mut last_complete: u64 = 0;
    loop {
        let id_len = read_varint(&mut reader);
        let id_len = match id_len {
            Ok(0) => break,
            Ok(n) => n,
            Err(e) if e.kind() == io::ErrorKind::UnexpectedEof => break,
            Err(e) => return Err(StorageError::from(e).into()),
        };
        let mut id_buf = vec![0u8; id_len as usize];
        match reader.read_exact(&mut id_buf) {
            Ok(()) => {}
            Err(e) if e.kind() == io::ErrorKind::UnexpectedEof => break,
            Err(e) => return Err(StorageError::from(e).into()),
        }
        offset += varint_len(id_len) + id_len;
        let frame_len = match read_varint(&mut reader) {
            Ok(n) => n,
            Err(e) if e.kind() == io::ErrorKind::UnexpectedEof => break,
            Err(e) => return Err(StorageError::from(e).into()),
        };
        let header_len = varint_len(frame_len);
        let record_end = offset + header_len + frame_len;
        if record_end > total {
            break;
        }
        match reader.seek(SeekFrom::Current(frame_len as i64)) {
            Ok(n) => offset = n,
            Err(e) if e.kind() == io::ErrorKind::UnexpectedEof => break,
            Err(e) => return Err(StorageError::from(e).into()),
        }
        last_complete = record_end;
    }
    if last_complete < total {
        OpenOptions::new()
            .write(true)
            .open(&path)
            .map_err(StorageError::from)?
            .set_len(last_complete)
            .map_err(StorageError::from)?;
    }
    Ok(last_complete)
}

#[cfg(test)]
mod tests {
    use super::*;
    use tempfile::TempDir;

    #[test]
    fn roundtrip_single_payload() {
        let tmp = TempDir::new().unwrap();
        let mut writer = PayloadWriter::open(tmp.path()).unwrap();
        let blob = b"large tool result content".repeat(64);
        let id = writer.write(&blob).unwrap();

        let reader = PayloadReader::open(tmp.path()).unwrap();
        let out = reader.read(id.as_str()).unwrap();
        assert_eq!(out, blob);
    }

    #[test]
    fn multiple_payloads_index_and_page() {
        let tmp = TempDir::new().unwrap();
        let ids = {
            let mut writer = PayloadWriter::open(tmp.path()).unwrap();
            let a = writer.write(&b"first".repeat(32)).unwrap();
            let b = writer.write(&b"second".repeat(32)).unwrap();
            let c = writer.write(&b"third".repeat(32)).unwrap();
            drop(writer);
            vec![a, b, c]
        };
        let reader = PayloadReader::open(tmp.path()).unwrap();
        assert_eq!(reader.read(ids[0].as_str()).unwrap(), b"first".repeat(32));
        assert_eq!(reader.read(ids[2].as_str()).unwrap(), b"third".repeat(32));
        assert_eq!(reader.read(ids[1].as_str()).unwrap(), b"second".repeat(32));
    }

    #[test]
    fn torn_trailing_frame_recovered() {
        let tmp = TempDir::new().unwrap();
        let id = {
            let mut writer = PayloadWriter::open(tmp.path()).unwrap();
            writer.write(&b"good payload".repeat(16)).unwrap()
        };
        let path = tmp.path().join(PAYLOADS_FILE);
        let len = path.metadata().unwrap().len();
        let f = OpenOptions::new().write(true).open(&path).unwrap();
        f.set_len(len + 37).unwrap();
        f.sync_all().unwrap();
        drop(f);

        let recovered = truncate_to_last_complete(tmp.path()).unwrap();
        assert_eq!(recovered, len);

        let reader = PayloadReader::open(tmp.path()).unwrap();
        assert_eq!(
            reader.read(id.as_str()).unwrap(),
            b"good payload".repeat(16)
        );
    }
}
