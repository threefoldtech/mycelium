use std::net::Ipv6Addr;

use bytes::{Buf, BufMut, BytesMut};
use tokio_util::codec::{Decoder, Encoder};

/// Size of the header start for a data packet (before the IP addresses).
const DATA_PACKET_HEADER_SIZE: usize = 4;

#[derive(Debug, Clone)]
pub struct DataPacket {
    pub raw_data: Vec<u8>, // eccrypte data isself then append the nonce
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

            let data_len = src.get_u16();
            // Reserved part of header
            let _ = src.get_u8();
            // Hop limit
            let hop_limit = src.get_u8();
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
        // Write the length of the data
        dst.put_u16(item.raw_data.len() as u16);
        // Write reserved part of header
        dst.put_u8(0);
        // Write hop limit.
        dst.put_u8(item.hop_limit);
        // Write the source IP
        dst.put_slice(&item.src_ip.octets());
        // Write the destination IP
        dst.put_slice(&item.dst_ip.octets());
        // Write the data
        dst.extend_from_slice(&item.raw_data);

        Ok(())
    }
}
