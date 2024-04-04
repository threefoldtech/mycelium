use bytes::{Buf, BufMut, BytesMut};
pub use control::ControlPacket;
pub use data::DataPacket;
use tokio_util::codec::{Decoder, Encoder};

mod control;
mod data;

/// Current version of the protocol being used.
const PROTOCOL_VERSION: u8 = 1;

/// The size of a `Packet` header on the wire, in bytes.
const PACKET_HEADER_SIZE: usize = 4;

#[derive(Debug, Clone)]
pub enum Packet {
    DataPacket(DataPacket),
    ControlPacket(ControlPacket),
}

#[derive(Debug, Clone, Copy)]
#[repr(u8)]
pub enum PacketType {
    DataPacket = 0,
    ControlPacket = 1,
}

pub struct Codec {
    packet_type: Option<PacketType>,
    data_packet_codec: data::Codec,
    control_packet_codec: control::Codec,
}

impl Codec {
    pub fn new() -> Self {
        Codec {
            packet_type: None,
            data_packet_codec: data::Codec::new(),
            control_packet_codec: control::Codec::new(),
        }
    }
}

impl Decoder for Codec {
    type Item = Packet;
    type Error = std::io::Error;

    fn decode(&mut self, src: &mut BytesMut) -> Result<Option<Self::Item>, Self::Error> {
        // Determine the packet_type
        let packet_type = if let Some(packet_type) = self.packet_type {
            packet_type
        } else {
            // Check we can read the header
            if src.remaining() <= PACKET_HEADER_SIZE {
                return Ok(None);
            }

            let mut header = [0; PACKET_HEADER_SIZE];
            header.copy_from_slice(&src[..PACKET_HEADER_SIZE]);
            src.advance(PACKET_HEADER_SIZE);

            // For now it's a hard error to not follow the 1 defined protocol version
            if header[0] != PROTOCOL_VERSION {
                return Err(std::io::Error::new(
                    std::io::ErrorKind::InvalidData,
                    "Unknown protocol version",
                ));
            };

            let packet_type_byte = header[1];
            let packet_type = match packet_type_byte {
                0 => PacketType::DataPacket,
                1 => PacketType::ControlPacket,
                _ => {
                    return Err(std::io::Error::new(
                        std::io::ErrorKind::InvalidData,
                        "Invalid packet type",
                    ));
                }
            };

            self.packet_type = Some(packet_type);

            packet_type
        };

        // Decode packet based on determined packet_type
        match packet_type {
            PacketType::DataPacket => {
                match self.data_packet_codec.decode(src) {
                    Ok(Some(p)) => {
                        self.packet_type = None; // Reset state
                        Ok(Some(Packet::DataPacket(p)))
                    }
                    Ok(None) => Ok(None),
                    Err(e) => Err(e),
                }
            }
            PacketType::ControlPacket => {
                match self.control_packet_codec.decode(src) {
                    Ok(Some(p)) => {
                        self.packet_type = None; // Reset state
                        Ok(Some(Packet::ControlPacket(p)))
                    }
                    Ok(None) => Ok(None),
                    Err(e) => Err(e),
                }
            }
        }
    }
}

impl Encoder<Packet> for Codec {
    type Error = std::io::Error;

    fn encode(&mut self, item: Packet, dst: &mut BytesMut) -> Result<(), Self::Error> {
        match item {
            Packet::DataPacket(datapacket) => {
                dst.put_slice(&[PROTOCOL_VERSION, 0, 0, 0]);
                self.data_packet_codec.encode(datapacket, dst)
            }
            Packet::ControlPacket(controlpacket) => {
                dst.put_slice(&[PROTOCOL_VERSION, 1, 0, 0]);
                self.control_packet_codec.encode(controlpacket, dst)
            }
        }
    }
}

impl Default for Codec {
    fn default() -> Self {
        Self::new()
    }
}
