use std::net::{IpAddr, Ipv4Addr};
use tokio::sync::mpsc;

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
    pub raw_data: Vec<u8>,
    pub dest_ip: Ipv4Addr,
}

impl DataPacket {}

/* ****************************CONTROL PACKET******************************** */

#[derive(Debug, Clone)]
pub struct ControlStruct {
    pub control_packet: ControlPacket,
    pub control_reply_tx: mpsc::UnboundedSender<ControlPacket>,
    pub src_overlay_ip: IpAddr,
}

#[derive(Debug, PartialEq, Clone)]
pub struct ControlPacket {
    pub header: BabelPacketHeader,
    pub body: Option<BabelPacketBody>,
    // pub trailer: Option<BabelPacketTrailer>,
}

#[derive(Debug, PartialEq, Clone)]
pub struct BabelPacketHeader {
    pub magic: u8,
    pub version: u8,
    pub body_length: u16,
}

// A BabelPacketBody describes exactly one TLV
// TODO: According to the protocol the body of a Babel packet can contain multiple subsequent TLV's
#[derive(Debug, PartialEq, Clone)]
pub struct BabelPacketBody {
    pub tlv_type: BabelTLVType,
    pub length: u8,
    pub body: BabelTLV, 
}

impl ControlStruct {
    pub fn reply(self, control_packet: ControlPacket) {
        if let Err(e) = self.control_reply_tx.send(control_packet) {
            eprintln!("Error reply: {:?}", e);
        }
    }
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
    pub fn new_ihu(rxcost: u16, interval: u16, dest_address: IpAddr) -> Self {
        Self {
            header: BabelPacketHeader::new(10), 
            body: Some(BabelPacketBody { 
                tlv_type: BabelTLVType::IHU, 
                length: 8, 
                body: BabelTLV::IHU { 
                    rxcost, 
                    interval, 
                    address: dest_address, 
                } 
            }),
        }
    }
}

#[derive(Debug, PartialEq, Clone)]
pub enum BabelTLVType {
    Pad1 = 0,
    PadN = 1,
    AckReq = 2,
    Ack = 3,
    Hello = 4,
    IHU = 5,
    RouterID = 6,
    NextHop = 7,
    Update = 8,
    RouteReq = 9,
    SeqnoReq = 10,
}

impl BabelTLVType {
    pub fn from_u8(value: u8) -> Option<Self> {
        match value {
            // 0 => Some(Self::Pad1),
            // 1 => Some(Self::PadN),
            2 => Some(Self::AckReq),
            3 => Some(Self::Ack),
            4 => Some(Self::Hello),
            5 => Some(Self::IHU),
            // 6 => Some(Self::RouterID),
            7 => Some(Self::NextHop),
            8 => Some(Self::Update),
            9 => Some(Self::RouteReq),
            10 => Some(Self::SeqnoReq),
            _ => None,
        }
    }
}

#[derive(Debug, Clone, PartialEq)]
pub enum BabelTLV {
    // These TLVs are not implemented as they are used for padding when sending multiple TLVs in one packet.
    // Pad1,
    // PadN(u8),
    AckReq { 
        nonce: u16, 
        interval: u16
    },
    Ack { nonce: u16 },
    Hello { 
        flags: u16,
        seqno: u16, 
        interval: u16,
    },
    IHU { 
        rxcost: u16, 
        interval: u16,
        address: IpAddr, 
    },
    // RouterID TLVs are sent just before Update TLVs and server the purpose of identifying the router that sent the Update TLV.
    // This is used when multiple Update TLVs are sent where a prefix is included in the first Update TLV and omitted in the subsequent Update TLVs.
    // As we do not support ommitted prefixes, we do not need to implement this TLV. We pass the router_id as a parameter to the Update TLV instead.
    // RouterID { router_id: u16 },
    NextHop { address: IpAddr },
    Update {
        address_encoding: u8,
        prefix_length: u8,
        interval: u16,
        seqno: u16,
        metric: u16,
        prefix: IpAddr,
        router_id: u64,
    },
    RouteReq { prefix: IpAddr, plen: u8 },
    SeqnoReq {
        prefix: IpAddr,
        plen: u8,
        seqno: u16,
        router_id: u16,
    },
}