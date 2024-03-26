use std::net::Ipv6Addr;

use bytes::{Buf, BufMut, BytesMut};
use tokio_util::codec::{Decoder, Encoder};

/// Size of the header start for a data packet (before the IP addresses).
const DATA_PACKET_HEADER_SIZE: usize = 4;

/// Mask to extract data length from
const DATA_PACKET_LEN_MASK: u32 = (1 << 16) - 1;

#[derive(Debug, Clone)]
pub struct DataPacket {
    pub raw_data: Vec<u8>, // encrypted data itself, then append the nonce
    /// Max amount of hops for the packet.
    pub hop_limit: u8,
    pub src_ip: Ipv6Addr,
    pub dst_ip: Ipv6Addr,
}

pub struct Codec {
    header_vals: Option<HeaderValues>,
    src_ip: Option<Ipv6Addr>,
    dest_ip: Option<Ipv6Addr>,
}

/// Data from the DataPacket header.
#[derive(Clone, Copy)]
struct HeaderValues {
    len: u16,
    hop_limit: u8,
}

impl Codec {
    pub fn new() -> Self {
        Codec {
            header_vals: None,
            src_ip: None,
            dest_ip: None,
        }
    }
}

impl Decoder for Codec {
    type Item = DataPacket;
    type Error = std::io::Error;

    fn decode(&mut self, src: &mut BytesMut) -> Result<Option<Self::Item>, Self::Error> {
        // Determine the length of the data
        let HeaderValues { len, hop_limit } = if let Some(header_vals) = self.header_vals {
            header_vals
        } else {
            // Check we have enough data to decode
            if src.len() < DATA_PACKET_HEADER_SIZE {
                return Ok(None);
            }

            let raw_header = src.get_u32();
            // Hop limit is the last 8 bits.
            let hop_limit = (raw_header & 0xFF) as u8;
            let data_len = ((raw_header >> 8) & DATA_PACKET_LEN_MASK) as u16;
            let header_vals = HeaderValues {
                len: data_len,
                hop_limit,
            };

            self.header_vals = Some(header_vals);

            header_vals
        };

        let data_len = len as usize;

        // Determine the source IP
        let src_ip = if let Some(src_ip) = self.src_ip {
            src_ip
        } else {
            if src.len() < 16 {
                return Ok(None);
            }

            // Decode octets
            let mut ip_bytes = [0u8; 16];
            ip_bytes.copy_from_slice(&src[..16]);
            let src_ip = Ipv6Addr::from(ip_bytes);
            src.advance(16);

            self.src_ip = Some(src_ip);
            src_ip
        };

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

        // Check we have enough data to decode
        if src.len() < data_len {
            return Ok(None);
        }

        // Decode octets
        let mut data = vec![0u8; data_len];
        data.copy_from_slice(&src[..data_len]);
        src.advance(data_len);

        // Reset state
        self.header_vals = None;
        self.dest_ip = None;
        self.src_ip = None;

        Ok(Some(DataPacket {
            raw_data: data,
            hop_limit,
            dst_ip: dest_ip,
            src_ip,
        }))
    }
}

impl Encoder<DataPacket> for Codec {
    type Error = std::io::Error;

    fn encode(&mut self, item: DataPacket, dst: &mut BytesMut) -> Result<(), Self::Error> {
        dst.reserve(item.raw_data.len() + DATA_PACKET_HEADER_SIZE + 16 + 16);
        let mut raw_header = 0;
        // Add length of the data
        raw_header |= (item.raw_data.len() as u32) << 8;
        // And hop limit
        raw_header |= item.hop_limit as u32;
        dst.put_u32(raw_header);
        // Write the source IP
        dst.put_slice(&item.src_ip.octets());
        // Write the destination IP
        dst.put_slice(&item.dst_ip.octets());
        // Write the data
        dst.extend_from_slice(&item.raw_data);

        Ok(())
    }
}
