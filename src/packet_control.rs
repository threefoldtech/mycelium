use etherparse::ip_number;
use tokio_util::codec::{Decoder, Encoder};
use bytes::{BytesMut, Buf, BufMut};


pub enum Packet {
    DataPacket(DataPacket), // packet coming from kernel
    ControlPacket(ControlPacket), // babel related packets
}
// todo: high-level packetcodec 

pub struct DataPacket {
    // ... additional types of data packets
    pub raw_data: Vec<u8>,
}

pub enum PacketType {
    DataPacket,
    ControlPacket,
}

pub struct PacketCodec {
    packet_type: PacketType,
    data_packet_codec: DataPacketCodec,
    control_packet_codec: ControlPacketCodec,
}

pub struct DataPacketCodec {
    len: Option<u16>,
}

impl DataPacketCodec{
    pub fn new() -> Self {
        DataPacketCodec { len:None }
    }
}

impl Decoder for DataPacketCodec {
    type Item = DataPacket;
    type Error = std::io::Error;

    fn decode(&mut self, src: &mut BytesMut) -> Result<Option<Self::Item>, Self::Error> {
        let data_len = if let Some(data_len) = self.len {
            data_len
        } else {

            // do we have enough data to decode
            if src.len() < 2 {
                return Ok(None);
            }    

            let data_len = src.get_u16();
            self.len = Some(data_len);

            data_len
        } as usize;

        if src.len() < data_len {

            src.reserve(data_len - src.len());

            return Ok(None);
        } 

        // we have enough data
        let data = src[..data_len].to_vec();
        src.advance(data_len);

        // set len to None so next we read we start at header again
        self.len = None;

        Ok(Some(DataPacket{raw_data: data}))
   }
}

impl Encoder<DataPacket> for DataPacketCodec {
    type Error = std::io::Error;

    fn encode(&mut self, item: DataPacket, dst: &mut BytesMut) -> Result<(), Self::Error> {
        // implies that length is never more than u16

        dst.put_u16(item.raw_data.len() as u16);
        dst.reserve(item.raw_data.len());

        dst.extend_from_slice(&item.raw_data);


        Ok(())
    }
}

pub enum ControlPacket {
    Ping(u32),
    OtherControlType(u32),
    // ... additional types of control packets
}

struct ControlPacketHeader {
    header_type: u8,
    len: u8,
}

pub struct ControlPacketCodec {
   //header: ControlPacketHeader,
   //data: Vec<u8>,
}

// impl Decoder for ControlPacketCodec {
//     type Item = ControlPacket;
//     type Error = std::io::Error;

//     fn decode(&mut self, src: &mut BytesMut) -> Result<Option<Self::Item>, Self::Error> {
//         // Check if we have enough data to read the header
//         if src.len() < 2 {
//             return Ok(None);
//         }

//         // Read the header
//         self.header.header_type = src[0];
//         self.header.len = src[1];

//         // Check if we have enough data to read the rest of the packet
//         if src.len() < 2 + self.header.len as usize {
//             return Ok(None);
//         }

//         // Read the rest of the packet
//         self.data = src[2..2 + self.header.len as usize].to_vec();

//         // Remove the packet from the buffer
//         src.advance(2 + self.header.len as usize);

//         // Parse the packet
//         match self.header.header_type {
//             ip_number::ICMP => {
//                 // Ping packet
//                 let ping_id = u32::from_le_bytes(self.data.clone().try_into().unwrap());
//                 Ok(Some(ControlPacket::Ping(ping_id)))
//             },
//             1 => {
//                 // Other control packet
//                 let other_control_id = u32::from_le_bytes(self.data.clone().try_into().unwrap());
//                 Ok(Some(ControlPacket::OtherControlType(other_control_id)))
//             },
//             _ => {
//                 // Unknown packet type
//                 Err(std::io::Error::new(
//                     std::io::ErrorKind::InvalidData,
//                     "Unknown packet type",
//                 ))
//             }
//         }
//     }
// }

// implement the encoder
impl Encoder<ControlPacket> for ControlPacketCodec {
    type Error = std::io::Error;

    fn encode(&mut self, item: ControlPacket, dst: &mut BytesMut) -> Result<(), Self::Error> {
        match item {
            ControlPacket::Ping(ping_id) => {
                // Write the header
                dst.put_u8(0);
                dst.put_u8(4);

                // Write the data
                dst.put_u32_le(ping_id);
            },
            ControlPacket::OtherControlType(other_control_id) => {
                // Write the header
                dst.put_u8(1);
                dst.put_u8(4);

                // Write the data
                dst.put_u32_le(other_control_id);
            },
        }

        Ok(())
    }
}
