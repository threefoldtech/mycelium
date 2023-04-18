use std::net::Ipv4Addr;

use etherparse::{PacketHeaders, IpHeader};
use tokio_util::codec::{Decoder, Encoder};
use bytes::{BytesMut, Buf, BufMut};

#[derive(Clone)]
pub enum Packet {
    DataPacket(DataPacket), // packet coming from kernel
    //ControlPacket(ControlPacket), // babel related packets
}

// create function to extract destip from Packet type
impl Packet {
    pub fn get_dest_ip(&self) -> std::net::Ipv4Addr {
        match self {
            Packet::DataPacket(packet) => packet.dest_ip,
            //Packet::ControlPacket(packet) => packet.dest_ip,
        }
    }
}

#[derive(Clone)]
pub struct DataPacket {
    pub raw_data: Vec<u8>,
    pub dest_ip: std::net::Ipv4Addr,
}

#[derive(Debug, Clone, Copy)]
#[repr(u8)]
pub enum PacketType {
    DataPacket = 0,
    _ControlPacket = 1,
}

pub struct PacketCodec {
    packet_type: Option<PacketType>,
    data_packet_codec: DataPacketCodec,
    //control_packet_codec: ControlPacketCodec,
}

impl PacketCodec {
    pub fn new() -> Self {
        PacketCodec {packet_type: None, data_packet_codec: DataPacketCodec::new()}
    }
}

pub struct DataPacketCodec {
    len: Option<u16>,
    dest_ip: Option<std::net::Ipv4Addr>, 
}

impl DataPacketCodec{
    pub fn new() -> Self {
        DataPacketCodec { len: None , dest_ip: None }
    }
}


impl Decoder for DataPacketCodec {
    type Item = DataPacket;
    type Error = std::io::Error;

    fn decode(&mut self, src: &mut BytesMut) -> Result<Option<Self::Item>, Self::Error> {
        let data_len = if let Some(data_len) = self.len {
            data_len
        } else {

            // check we have enough data to decode
            if src.len() < 2 {
                return Ok(None);
            }    

            let data_len = src.get_u16();
            self.len = Some(data_len);

            data_len
        } as usize;

        let dest_ip = if let Some(dest_ip) = self.dest_ip {
            dest_ip
        } else {
            if src.len() < 4 {
                return Ok(None);
            }

            // decode octets
            let mut ip_bytes = [0u8; 4];
            ip_bytes.copy_from_slice(&src[..4]);
            let dest_ip = Ipv4Addr::from(ip_bytes);
            src.advance(4);

            self.dest_ip = Some(dest_ip);
            dest_ip
        };

        if src.len() < data_len {

            src.reserve(data_len - src.len());

            return Ok(None);
        } 

        // we have enough data
        let data = src[..data_len].to_vec();
        src.advance(data_len);

        // Reset state 
        self.len = None;
        self.dest_ip = None;
      
        Ok(Some(DataPacket { raw_data: data, dest_ip }))
    }
}

impl Encoder<DataPacket> for DataPacketCodec {
    type Error = std::io::Error;

    fn encode(&mut self, item: DataPacket, dst: &mut BytesMut) -> Result<(), Self::Error> {
        // implies that length is never more than u16

        dst.reserve(item.raw_data.len() + 6);
        dst.put_u16(item.raw_data.len() as u16);
        // dest ip wegschrijven
        dst.put_slice(&item.dest_ip.octets());

        dst.extend_from_slice(&item.raw_data);


        Ok(())
    }
}

impl Decoder for PacketCodec {
    type Item = Packet;
    type Error = std::io::Error; 

    fn decode(&mut self, src: &mut BytesMut) -> Result<Option<Self::Item>, Self::Error> {
        let packet_type = if let Some(packet_type) = self.packet_type {
            packet_type
        } else {

            // Check if we have enough bytes to read one byte (which shows to packet type)
            if src.len() < 1 {
                return Ok(None);
            }

            let raw_packet_type = src.get_u8(); // Beware: this advances src by 1 u8
            let packet_type = match raw_packet_type {
                0 => { PacketType::DataPacket }
                // 1 => { PacketType::ControlPacket }
                _ => return Err(std::io::Error::new(std::io::ErrorKind::InvalidData, "Unrecognized packet type"))
            };
            
            packet_type
        };

        match packet_type {
            PacketType::DataPacket => {
                match self.data_packet_codec.decode(src) {
                    Ok(Some(p)) => {
                        self.packet_type = None; // necessary otherwise we would have the situation where assume the packet_type already exists and just read further
                        Ok(Some(Packet::DataPacket(p)))
                    },
                    Ok(None) => {
                        Ok(None)
                    },
                    Err(e) => {
                        Err(e)
                    }
                }
            }
            PacketType::_ControlPacket => {
                unimplemented!()
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
            // PacketType::ControlPacket(controlpacket) => {
            //     dst.put_u8(1);
            //     self.control_packet.codec.encode(controlpacket);
            // }
        }
    }
}


