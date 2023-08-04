use crate::{
    babel::{self},
    crypto::PublicKey,
    packet::{ControlPacket, DataPacket, Packet, PacketType},
};
use bytes::{Buf, BufMut, BytesMut};
use log::debug;
use std::{io, net::Ipv6Addr};
use tokio_util::codec::{Decoder, Encoder};

/* ********************************PAKCET*********************************** */
pub struct PacketCodec {
    packet_type: Option<PacketType>,
    data_packet_codec: DataPacketCodec,
    control_packet_codec: ControlPacketCodec,
}

impl PacketCodec {
    pub fn new() -> Self {
        PacketCodec {
            packet_type: None,
            data_packet_codec: DataPacketCodec::new(),
            control_packet_codec: ControlPacketCodec::new(),
        }
    }
}

impl Decoder for PacketCodec {
    type Item = Packet;
    type Error = std::io::Error;

    fn decode(&mut self, src: &mut BytesMut) -> Result<Option<Self::Item>, Self::Error> {
        // Determine the packet_type
        let packet_type = if let Some(packet_type) = self.packet_type {
            packet_type
        } else {
            // Check we can read the packet type (1 byte)
            if src.is_empty() {
                return Ok(None);
            }

            let packet_type_byte = src.get_u8(); // ! This will advance the buffer 1 byte !
            let packet_type = match packet_type_byte {
                0 => PacketType::DataPacket,
                1 => PacketType::ControlPacket,
                _ => {
                    debug!("buffer: {:?}", &src[..src.remaining()]);
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

impl Encoder<Packet> for PacketCodec {
    type Error = std::io::Error;

    fn encode(&mut self, item: Packet, dst: &mut BytesMut) -> Result<(), Self::Error> {
        match item {
            Packet::DataPacket(datapacket) => {
                dst.put_u8(0);
                self.data_packet_codec.encode(datapacket, dst)
            }
            Packet::ControlPacket(controlpacket) => {
                dst.put_u8(1);
                self.control_packet_codec.encode(controlpacket, dst)
            }
        }
    }
}

/* ******************************DATA PACKET********************************* */
pub struct DataPacketCodec {
    len: Option<u16>,
    dest_ip: Option<std::net::Ipv6Addr>,
    pubkey: Option<PublicKey>,
}

impl DataPacketCodec {
    pub fn new() -> Self {
        DataPacketCodec {
            len: None,
            dest_ip: None,
            pubkey: None,
        }
    }
}

impl Decoder for DataPacketCodec {
    type Item = DataPacket;
    type Error = std::io::Error;

    fn decode(&mut self, src: &mut BytesMut) -> Result<Option<Self::Item>, Self::Error> {
        // Determine the length of the data
        let data_len = if let Some(data_len) = self.len {
            data_len
        } else {
            // Check we have enough data to decode
            if src.len() < 2 {
                return Ok(None);
            }

            let data_len = src.get_u16();
            self.len = Some(data_len);

            data_len
        } as usize;

        // Determine the destination IP
        let dest_ip = if let Some(dest_ip) = self.dest_ip {
            dest_ip
        } else {
            if src.len() < 16 {
                return Ok(None);
            }

            // Decode octets
            let mut ip_bytes = [0u8; 16];
            ip_bytes.copy_from_slice(&src[..16]);
            let dest_ip = Ipv6Addr::from(ip_bytes);
            src.advance(16);

            self.dest_ip = Some(dest_ip);
            dest_ip
        };

        // Determine the pubkey
        let pubkey = if let Some(pubkey) = self.pubkey {
            pubkey
        } else {
            if src.len() < 32 {
                return Ok(None);
            }

            let mut pubkey_bytes = [0u8; 32];
            pubkey_bytes.copy_from_slice(&src[..32]);
            let pubkey = PublicKey::from(pubkey_bytes);
            src.advance(32);

            self.pubkey = Some(pubkey);
            pubkey
        };

        // Check we have enough data to decode
        if src.len() < data_len {
            return Ok(None);
        }

        // Decode octets
        let mut data = vec![0u8; data_len];
        data.copy_from_slice(&src[..data_len]);
        src.advance(data_len);

        // Reset state
        self.len = None;
        self.dest_ip = None;
        self.pubkey = None;

        Ok(Some(DataPacket {
            raw_data: data,
            dest_ip,
            pubkey,
        }))
    }
}

impl Encoder<DataPacket> for DataPacketCodec {
    type Error = std::io::Error;

    fn encode(&mut self, item: DataPacket, dst: &mut BytesMut) -> Result<(), Self::Error> {
        dst.reserve(item.raw_data.len() + 16 + 32);
        // Write the length of the data
        dst.put_u16(item.raw_data.len() as u16);
        // Write the destination IP
        dst.put_slice(&item.dest_ip.octets());
        // Write the public key
        dst.put_slice(&item.pubkey.to_bytes());
        // Write the data
        dst.extend_from_slice(&item.raw_data);

        Ok(())
    }
}

/* ****************************CONTROL PACKET******************************** */
pub struct ControlPacketCodec {
    // TODO: wrapper to make it easier to deserialize
    codec: babel::Codec,
}

impl ControlPacketCodec {
    pub fn new() -> Self {
        ControlPacketCodec {
            codec: babel::Codec::new(),
        }
    }
}

impl Decoder for ControlPacketCodec {
    type Item = ControlPacket;
    type Error = std::io::Error;

    fn decode(&mut self, buf: &mut BytesMut) -> Result<Option<Self::Item>, Self::Error> {
        self.codec.decode(buf)
    }
}

impl Encoder<ControlPacket> for ControlPacketCodec {
    type Error = io::Error;

    fn encode(&mut self, message: ControlPacket, buf: &mut BytesMut) -> Result<(), Self::Error> {
        self.codec.encode(message, buf)
    }
}
