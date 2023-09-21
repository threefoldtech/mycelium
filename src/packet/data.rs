use std::net::Ipv6Addr;

use bytes::{Buf, BufMut, BytesMut};
use tokio_util::codec::{Decoder, Encoder};

#[derive(Debug, Clone)]
pub struct DataPacket {
    pub raw_data: Vec<u8>, // eccrypte data isself then append the nonce
    pub dst_ip: Ipv6Addr,
    pub src_ip: Ipv6Addr,
}

pub struct Codec {
    len: Option<u16>,
    dest_ip: Option<Ipv6Addr>,
    src_ip: Option<Ipv6Addr>,
}

impl Codec {
    pub fn new() -> Self {
        Codec {
            len: None,
            dest_ip: None,
            src_ip: None,
        }
    }
}

impl Decoder for Codec {
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
        self.src_ip = None;

        Ok(Some(DataPacket {
            raw_data: data,
            dst_ip: dest_ip,
            src_ip,
        }))
    }
}

impl Encoder<DataPacket> for Codec {
    type Error = std::io::Error;

    fn encode(&mut self, item: DataPacket, dst: &mut BytesMut) -> Result<(), Self::Error> {
        dst.reserve(item.raw_data.len() + 2 + 16 + 32);
        // Write the length of the data
        dst.put_u16(item.raw_data.len() as u16);
        // Write the destination IP
        dst.put_slice(&item.dst_ip.octets());
        // Write the public key
        dst.put_slice(&item.dst_ip.octets());
        // Write the data
        dst.extend_from_slice(&item.raw_data);

        Ok(())
    }
}
