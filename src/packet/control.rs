use std::net::IpAddr;

use crate::{
    babel, metric::Metric, peer::Peer, router_id::RouterId, sequence_number::SeqNo, subnet::Subnet,
};

pub type ControlPacket = babel::Tlv;

impl ControlPacket {
    pub fn new_hello(dest_peer: &mut Peer, interval: u16) -> Self {
        let tlv: babel::Tlv = babel::Hello::new_unicast(dest_peer.hello_seqno(), interval).into();
        dest_peer.increment_hello_seqno();
        tlv
    }

    pub fn new_ihu(interval: u16, dest_address: IpAddr) -> Self {
        // TODO: Set rx metric
        babel::Ihu::new(Metric::from(0), interval, Some(dest_address)).into()
    }

    pub fn new_update(
        interval: u16,
        seqno: SeqNo,
        metric: Metric,
        subnet: Subnet,
        router_id: RouterId,
    ) -> Self {
        babel::Update::new(interval, seqno, metric, subnet, router_id).into()
    }
}
