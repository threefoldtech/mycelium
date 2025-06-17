use std::net::SocketAddr;

use serde::{Deserialize, Serialize};

/// Hashes as used in the definitions.
pub type Hash = [u8; 32];
/// The type used to refer to a hash which is file metadata.
pub type FileMetaHash = Hash;

/// A blob of metadata, bincode encoded.
#[derive(Deserialize, Serialize)]
pub enum Metadata {
    /// The metadata represents a [`File`].
    File(File),
    /// The metadata represents a [`Directory`].
    Directory(Directory),
}

/// Metadata about a single file.
#[derive(Deserialize, Serialize)]
pub struct File {
    /// The hash of the unencrypted file content. This is also used as encryption key.
    pub content_hash: Hash,
    /// The hash of the content after encryption, before chunking.
    pub encrypted_content_hash: Hash,
    /// Name of the file.
    pub name: String,
    /// Mime type of the file content.
    pub mime: String,
    /// The blocks which make up the actual data of the file.
    pub blocks: Vec<Block>,
}

/// Metadata about a single directory.
#[derive(Deserialize, Serialize)]
pub struct Directory {
    /// A list of file hashes.
    pub files: Vec<FileMetaHash>,
    /// Name of the directory.
    pub name: String,
}

/// Info about distribution of a single block.
#[derive(Deserialize, Serialize)]
pub struct Block {
    pub shards: Vec<Location>,
    /// Offset in bytes this block is placed in the file.
    pub start_offset: u64,
    /// Offset in bytes the last byte in this block is placed in the file.
    pub end_offset: u64,
}

/// Location information for shards in 0-DB
#[derive(Deserialize, Serialize)]
pub struct Location {
    /// 0-DB host IP address and port.
    pub host: SocketAddr,
    /// 0-DB namespace.
    pub namespace: String,
    /// 0-DB namespace secret, if one is present.
    pub secret: Option<String>,
}
