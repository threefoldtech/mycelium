use std::{io, net::{IpAddr, Ipv4Addr}};
use bytes::{BufMut, BytesMut, Buf};
use tokio_util::codec::{Encoder, Decoder};
use crate::packet::{Packet, ControlPacket, DataPacket, PacketType, BabelTLVType, BabelPacketBody, BabelTLV, BabelPacketHeader};

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
            if src.len() < 1 {
                return Ok(None);
            }

            let packet_type_byte = src.get_u8(); // ! This will advance the buffer 1 byte !
            let packet_type = match packet_type_byte {
                0 => PacketType::DataPacket,
                1 => PacketType::ControlPacket,
                _ => return Err(std::io::Error::new(std::io::ErrorKind::InvalidData, "Invalid packet type")),
            };

            packet_type
        };

        // Decode packet based on determined packet_type
        match packet_type {
            PacketType::DataPacket => {
                match self.data_packet_codec.decode(src) {
                    Ok(Some(p)) => {
                        self.packet_type = None; // Reset state
                        Ok(Some(Packet::DataPacket(p)))
                    },
                    Ok(None) => Ok(None),
                    Err(e) => Err(e),
                }
            }
            PacketType::ControlPacket => {
                match self.control_packet_codec.decode(src) {
                    Ok(Some(p)) => {
                        self.packet_type = None; // Reset state
                        Ok(Some(Packet::ControlPacket(p)))
                    },
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
    dest_ip: Option<std::net::Ipv4Addr>,
}

impl DataPacketCodec {
    pub fn new() -> Self {
        DataPacketCodec {
            len: None,
            dest_ip: None,
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
            if src.len() < 4 {
                return Ok(None);
            }

            // Decode octets
            let mut ip_bytes = [0u8; 4];
            ip_bytes.copy_from_slice(&src[..4]);
            let dest_ip = Ipv4Addr::from(ip_bytes);
            src.advance(4);

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
        self.len = None;
        self.dest_ip = None;

        Ok(Some(DataPacket {
            raw_data: data,
            dest_ip,
        }))
    }
}

impl Encoder<DataPacket> for DataPacketCodec {
    type Error = std::io::Error;

    fn encode(&mut self, item: DataPacket, dst: &mut BytesMut) -> Result<(), Self::Error> {
        dst.reserve(item.raw_data.len() + 6);
        // Write the length of the data
        dst.put_u16(item.raw_data.len() as u16);
        // Write the destination IP
        dst.put_slice(&item.dest_ip.octets());
        // Write the data
        dst.extend_from_slice(&item.raw_data);

        Ok(())
    }
}

/* ****************************CONTROL PACKET******************************** */
pub struct ControlPacketCodec {}

impl ControlPacketCodec {
    pub fn new() -> Self {
        ControlPacketCodec {}
    }
}

// TODO FUTURE-WISE --> HANDLE BUFFER READS THAT MIGHT NOT HAVE ARRIVED YET
impl Decoder for ControlPacketCodec {
    type Item = ControlPacket;
    type Error = std::io::Error;

    fn decode(&mut self, buf: &mut BytesMut) -> Result<Option<Self::Item>, Self::Error> {
        if buf.remaining() < 4 {
            return Ok(None);
        }

        let magic = buf.get_u8();
        let version = buf.get_u8();
        let body_length = buf.get_u16();

        if buf.remaining() < body_length as usize {
            return Ok(None);
        }

        let header = BabelPacketHeader {
            magic,
            version,
            body_length,
        };

        let tlv_type = match BabelTLVType::from_u8(buf.get_u8()) {
            Some(t) => t,
            None => return Err(io::Error::new(io::ErrorKind::InvalidData, "Invalid TLV type")),
        };

        if tlv_type == BabelTLVType::Pad1 {
            return Ok(Some(ControlPacket {
                header,
                body: Some(BabelPacketBody {
                    tlv_type,
                    length: 0,
                    body: BabelTLV::Pad1,
                }),
            }));
        }

        let length = buf.get_u8();

        let body = match tlv_type {
            BabelTLVType::PadN => {
                buf.advance(usize::from(length));
                Some(BabelPacketBody {
                    tlv_type,
                    length,
                    body: BabelTLV::PadN(length),
                })
            }
            BabelTLVType::Hello => {
                let flags = buf.get_u16();
                let seqno = buf.get_u16();
                let interval = buf.get_u16();
                Some(BabelPacketBody {
                    tlv_type,
                    length,
                    body: BabelTLV::Hello { flags, seqno, interval },
                })
            }
            BabelTLVType::IHU => {
                let _address_encoding = buf.get_u8();
                let _reserved = buf.get_u8();
                let rxcost = buf.get_u16();
                let interval = buf.get_u16();
                let address = IpAddr::V4(Ipv4Addr::new(buf.get_u8(), buf.get_u8(), buf.get_u8(), buf.get_u8()));
                Some(BabelPacketBody {
                    tlv_type,
                    length,
                    body: BabelTLV::IHU { rxcost, interval, address },
                })
            }
            // Add decoding logic for other TLV types.
            _ => None,
        };

        Ok(Some(ControlPacket { header, body }))
    }
}

impl Encoder<ControlPacket> for ControlPacketCodec {
    type Error = io::Error;

    fn encode(&mut self, message: ControlPacket, buf: &mut BytesMut) -> Result<(), Self::Error> {
        // Write BabelPacketHeader
        buf.put_u8(message.header.magic);
        buf.put_u8(message.header.version);
        buf.put_u16(message.header.body_length);

        if let Some(body) = message.body {
            // Write BabelPacketBody
            buf.put_u8(body.tlv_type as u8);
            buf.put_u8(body.length);

            match body.body {
                BabelTLV::Pad1 => {}
                BabelTLV::PadN(padding_length) => {
                    buf.put_slice(&vec![0; usize::from(padding_length)]);
                }
                BabelTLV::Hello { flags, seqno, interval } => {
                    buf.put_u16(flags);
                    buf.put_u16(seqno);
                    buf.put_u16(interval);
                }
                BabelTLV::IHU { rxcost, interval, address } => {
                    buf.put_u8(0); // Temporary static address encoding
                    buf.put_u8(0); // Reserved field should be set to 0 and MUST be ignored on reception
                    buf.put_u16(rxcost);
                    buf.put_u16(interval);
                    match address {
                        IpAddr::V4(ipv4) => {
                            buf.put_u8(ipv4.octets()[0]);
                            buf.put_u8(ipv4.octets()[1]);
                            buf.put_u8(ipv4.octets()[2]);
                            buf.put_u8(ipv4.octets()[3]);
                        }
                        IpAddr::V6(_ipv6) => {
                            println!("IPv6 not supported yet");
                        }
                    }
                }
                // Add encoding logic for other TLV types.
                _ => {}
            }
        }

        Ok(())
    }
}