use std::net::Ipv6Addr;

#[derive(Debug, Clone)]
pub struct DataPacket {
    pub raw_data: Vec<u8>, // eccrypte data isself then append the nonce
    pub dst_ip: Ipv6Addr,
    pub src_ip: Ipv6Addr,
}
