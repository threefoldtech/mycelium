use std::fmt;

use super::MessagePacket;

/// A message representing a "chunk" message.
///
/// The body of a chunk message has the following structure:
///   - 8 bytes: chunk index
///   - 8 bytes: chunk offset
///   - 8 bytes: chunk size
///   - remainder: chunk data of length based on field 3
pub struct MessageChunk {
    buffer: MessagePacket,
}

impl MessageChunk {
    /// Create a new `MessageChunk` in the provided [`MessagePacket`].
    pub fn new(mut buffer: MessagePacket) -> Self {
        buffer.set_used_buffer_size(24);
        buffer.header_mut().flags_mut().set_chunk();
        Self { buffer }
    }

    /// Return the index of the chunk in the message, as written in the body.
    pub fn chunk_idx(&self) -> u64 {
        u64::from_be_bytes(
            self.buffer.buffer()[..8]
                .try_into()
                .expect("Buffer contains a size field of valid length; qed"),
        )
    }

    /// Set the index of the chunk in the message body.
    pub fn set_chunk_idx(&mut self, chunk_idx: u64) {
        self.buffer.buffer_mut()[..8].copy_from_slice(&chunk_idx.to_be_bytes())
    }

    /// Return the chunk offset in the message, as written in the body.
    pub fn chunk_offset(&self) -> u64 {
        u64::from_be_bytes(
            self.buffer.buffer()[8..16]
                .try_into()
                .expect("Buffer contains a size field of valid length; qed"),
        )
    }

    /// Set the offset of the chunk in the message body.
    pub fn set_chunk_offset(&mut self, chunk_offset: u64) {
        self.buffer.buffer_mut()[8..16].copy_from_slice(&chunk_offset.to_be_bytes())
    }

    /// Return the size of the chunk in the message, as written in the body.
    pub fn chunk_size(&self) -> u64 {
        // Shield against a corrupt value.
        u64::min(
            u64::from_be_bytes(
                self.buffer.buffer()[16..24]
                    .try_into()
                    .expect("Buffer contains a size field of valid length; qed"),
            ),
            self.buffer.buffer().len() as u64 - 24,
        )
    }

    /// Set the size of the chunk in the message body.
    pub fn set_chunk_size(&mut self, chunk_size: u64) {
        self.buffer.buffer_mut()[16..24].copy_from_slice(&chunk_size.to_be_bytes())
    }

    /// Return a reference to the chunk data in the message.
    pub fn data(&self) -> &[u8] {
        &self.buffer.buffer()[24..24 + self.chunk_size() as usize]
    }

    /// Set the chunk data in this message. This will also set the size field to the proper value.
    pub fn set_chunk_data(&mut self, data: &[u8]) -> Result<(), InsufficientChunkSpace> {
        let buf = self.buffer.buffer_mut();
        let available_space = buf.len() - 24;

        if data.len() > available_space {
            return Err(InsufficientChunkSpace {
                available: available_space,
                needed: data.len(),
            });
        }

        // Slicing based on data.len() is fine here as we just checked to make sure we can handle
        // this capacity.
        buf[24..24 + data.len()].copy_from_slice(data);

        self.set_chunk_size(data.len() as u64);
        // Also set the extra space used by the buffer on the underlying packet.
        self.buffer.set_used_buffer_size(24 + data.len());

        Ok(())
    }

    /// Convert the `MessageChunk` into a reply. This does nothing if it is already a reply.
    pub fn into_reply(mut self) -> Self {
        self.buffer.header_mut().flags_mut().set_ack();
        // We want to leave the length field in tact but don't want to copy the data in the reply.
        // This needs additional work on the underlying buffer.
        // TODO
        self
    }

    /// Consumes this `MessageChunk`, returning the underlying [`MessagePacket`].
    pub fn into_inner(self) -> MessagePacket {
        self.buffer
    }
}

/// An error indicating not enough space is availbe in a message to set the chunk data.
#[derive(Debug)]
pub struct InsufficientChunkSpace {
    /// Amount of space available in the chunk.
    pub available: usize,
    /// Amount of space needed to set the chunk data
    pub needed: usize,
}

impl fmt::Display for InsufficientChunkSpace {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(
            f,
            "Insufficient capacity available, needed {} bytes, have {} bytes",
            self.needed, self.available
        )
    }
}

impl std::error::Error for InsufficientChunkSpace {}

#[cfg(test)]
mod tests {
    use std::array;

    use crate::{crypto::PacketBuffer, message::MessagePacket};

    use super::MessageChunk;

    #[test]
    fn chunk_flag_set() {
        let mc = MessageChunk::new(MessagePacket::new(PacketBuffer::new()));

        let mp = mc.into_inner();
        assert!(mp.header().flags().chunk());
    }

    #[test]
    fn read_chunk_idx() {
        let mut pb = PacketBuffer::new();
        pb.buffer_mut()[12..20].copy_from_slice(&[0, 0, 0, 0, 0, 0, 100, 73]);

        let ms = MessageChunk::new(MessagePacket::new(pb));

        assert_eq!(ms.chunk_idx(), 25_673);
    }

    #[test]
    fn write_chunk_idx() {
        let mut ms = MessageChunk::new(MessagePacket::new(PacketBuffer::new()));

        ms.set_chunk_idx(723);

        // Since we don't work with packet buffer we don't have to account for the message packet
        // header.
        assert_eq!(&ms.buffer.buffer()[..8], &[0, 0, 0, 0, 0, 0, 2, 211]);
        assert_eq!(ms.chunk_idx(), 723);
    }

    #[test]
    fn read_chunk_offset() {
        let mut pb = PacketBuffer::new();
        pb.buffer_mut()[20..28].copy_from_slice(&[0, 0, 0, 0, 0, 20, 40, 60]);

        let ms = MessageChunk::new(MessagePacket::new(pb));

        assert_eq!(ms.chunk_offset(), 1_321_020);
    }

    #[test]
    fn write_chunk_offset() {
        let mut ms = MessageChunk::new(MessagePacket::new(PacketBuffer::new()));

        ms.set_chunk_offset(1_000_000);

        // Since we don't work with packet buffer we don't have to account for the message packet
        // header.
        assert_eq!(&ms.buffer.buffer()[8..16], &[0, 0, 0, 0, 0, 15, 66, 64]);
        assert_eq!(ms.chunk_offset(), 1_000_000);
    }

    #[test]
    fn read_chunk_size() {
        let mut pb = PacketBuffer::new();
        pb.buffer_mut()[28..36].copy_from_slice(&[0, 0, 0, 0, 0, 0, 3, 232]);

        let ms = MessageChunk::new(MessagePacket::new(pb));

        assert_eq!(ms.chunk_size(), 1_000);
    }

    #[test]
    fn write_chunk_size() {
        let mut ms = MessageChunk::new(MessagePacket::new(PacketBuffer::new()));

        ms.set_chunk_size(1_300);

        // Since we don't work with packet buffer we don't have to account for the message packet
        // header.
        assert_eq!(&ms.buffer.buffer()[16..24], &[0, 0, 0, 0, 0, 0, 5, 20]);
        assert_eq!(ms.chunk_size(), 1_300);
    }

    #[test]
    fn read_chunk_data() {
        const CHUNK_DATA: &[u8] = &[1, 2, 3, 4, 5, 6, 7, 8, 9, 10, 11, 12, 13, 14, 15, 16];
        let mut pb = PacketBuffer::new();
        // Set data len
        pb.buffer_mut()[28..36].copy_from_slice(&CHUNK_DATA.len().to_be_bytes());
        pb.buffer_mut()[36..36 + CHUNK_DATA.len()].copy_from_slice(CHUNK_DATA);

        let ms = MessageChunk::new(MessagePacket::new(pb));

        assert_eq!(ms.chunk_size(), 16);
        assert_eq!(ms.data(), CHUNK_DATA);
    }

    #[test]
    fn write_chunk_data() {
        const CHUNK_DATA: &[u8] = &[1, 2, 3, 4, 5, 6, 7, 8, 9, 10, 11, 12, 13, 14, 15, 16];
        let mut ms = MessageChunk::new(MessagePacket::new(PacketBuffer::new()));

        let res = ms.set_chunk_data(CHUNK_DATA);
        assert!(res.is_ok());

        // Since we don't work with packet buffer we don't have to account for the message packet
        // header.
        // Check and make sure size is properly set.
        assert_eq!(&ms.buffer.buffer()[16..24], &[0, 0, 0, 0, 0, 0, 0, 16]);
        assert_eq!(ms.chunk_size(), 16);
        assert_eq!(ms.data(), CHUNK_DATA);
    }

    #[test]
    fn write_chunk_data_oversized() {
        let data: [u8; 1500] = array::from_fn(|_| 0xFF);
        let mut ms = MessageChunk::new(MessagePacket::new(PacketBuffer::new()));

        let res = ms.set_chunk_data(&data);
        assert!(res.is_err());
    }
}
