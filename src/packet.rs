use std::net::{IpAddr, Ipv6Addr};
use x25519_dalek::PublicKey;

use crate::{metric::Metric, peer::Peer, sequence_number::SeqNo};

pub const BABEL_MAGIC: u8 = 42;
pub const BABEL_VERSION: u8 = 2;

/* ********************************PAKCET*********************************** */
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

/* ******************************DATA PACKET********************************* */
#[derive(Debug, Clone)]
pub struct DataPacket {
    pub raw_data: Vec<u8>, // eccrypte data isself then append the nonce
    pub dest_ip: Ipv6Addr,
    pub pubkey: PublicKey,
}

impl DataPacket {}

/* ****************************CONTROL PACKET******************************** */

#[derive(Debug, Clone)]
pub struct ControlStruct {
    pub control_packet: ControlPacket,
    pub src_overlay_ip: IpAddr,
}

#[derive(Debug, PartialEq, Clone)]
pub struct ControlPacket {
    pub header: BabelPacketHeader,
    pub body: BabelPacketBody,
}

#[derive(Debug, PartialEq, Clone)]
pub struct BabelPacketHeader {
    pub magic: u8,
    pub version: u8,
    pub body_length: u16, // length of the whole BabelPacketBody (tlv_type, length and body)
}

// A BabelPacketBody describes exactly one TLV
#[derive(Debug, PartialEq, Clone)]
pub struct BabelPacketBody {
    pub tlv_type: BabelTLVType,
    pub length: u8, // length of the tlv (only the tlv, not tlv_type and length itself)
    pub tlv: BabelTLV,
}

impl BabelPacketHeader {
    pub fn new(body_length: u16) -> Self {
        Self {
            magic: BABEL_MAGIC,
            version: BABEL_VERSION,
            body_length,
        }
    }
}

impl ControlPacket {
    pub fn new_hello(dest_peer: &mut Peer, interval: u16) -> Self {
        let header_length = (BabelTLVType::Hello.get_tlv_length(false) + 2) as u16;
        dest_peer.increment_hello_seqno();
        Self {
            header: BabelPacketHeader::new(header_length),
            body: BabelPacketBody {
                tlv_type: BabelTLVType::Hello,
                length: BabelTLVType::Hello.get_tlv_length(false),
                tlv: BabelTLV::Hello {
                    seqno: dest_peer.hello_seqno(),
                    interval,
                },
            },
        }
    }

    pub fn new_ihu(interval: u16, dest_address: IpAddr) -> Self {
        let uses_ipv6 = dest_address.is_ipv6();
        let header_length = (BabelTLVType::IHU.get_tlv_length(uses_ipv6) + 2) as u16;
        Self {
            header: BabelPacketHeader::new(header_length),
            body: BabelPacketBody {
                tlv_type: BabelTLVType::IHU,
                length: BabelTLVType::IHU.get_tlv_length(uses_ipv6),
                tlv: BabelTLV::IHU {
                    interval,
                    address: dest_address,
                },
            },
        }
    }

    pub fn new_update(
        plen: u8,
        interval: u16,
        seqno: SeqNo,
        metric: Metric,
        prefix: IpAddr,
        router_id: PublicKey,
    ) -> Self {
        let uses_ipv6 = prefix.is_ipv6();
        let header_length = (BabelTLVType::Update.get_tlv_length(uses_ipv6) + 2) as u16;
        Self {
            header: BabelPacketHeader::new(header_length),
            body: BabelPacketBody {
                tlv_type: BabelTLVType::Update,
                length: BabelTLVType::Update.get_tlv_length(uses_ipv6),
                tlv: BabelTLV::Update {
                    plen,
                    interval,
                    seqno,
                    metric,
                    prefix,
                    router_id,
                },
            },
        }
    }
}

#[derive(Debug, PartialEq, Clone)]
pub enum BabelTLVType {
    Hello = 4,
    IHU = 5,
    Update = 8,
}

impl BabelTLVType {
    pub fn from_u8(value: u8) -> Option<Self> {
        match value {
            4 => Some(Self::Hello),
            5 => Some(Self::IHU),
            8 => Some(Self::Update),
            _ => None,
        }
    }

    pub fn get_tlv_length(self, uses_ipv6: bool) -> u8 {
        let (ipv6, ipv4) = match self {
            Self::Hello => (4, 4),
            Self::IHU => (18, 6),
            Self::Update => (31 + 1, 19 + 1), // +1 for ae
        };
        if uses_ipv6 {
            ipv6
        } else {
            ipv4
        }
    }
}

#[derive(Debug, Clone, PartialEq)]
pub enum BabelTLV {
    Hello {
        seqno: SeqNo,
        interval: u16,
    },
    IHU {
        interval: u16,
        address: IpAddr,
    },
    Update {
        plen: u8,
        interval: u16,
        seqno: SeqNo,
        metric: Metric,
        prefix: IpAddr,
        router_id: PublicKey,
    },
}
