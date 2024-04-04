use std::{io, net::IpAddr, time::Duration};

use bytes::BytesMut;
use tokio_util::codec::{Decoder, Encoder};

use crate::{
    babel, metric::Metric, peer::Peer, router_id::RouterId, sequence_number::SeqNo, subnet::Subnet,
};

pub type ControlPacket = babel::Tlv;

pub struct Codec {
    // TODO: wrapper to make it easier to deserialize
    codec: babel::Codec,
}

impl ControlPacket {
    pub fn new_hello(dest_peer: &Peer, interval: Duration) -> Self {
        let tlv: babel::Tlv =
            babel::Hello::new_unicast(dest_peer.hello_seqno(), (interval.as_millis() / 10) as u16)
                .into();
        dest_peer.increment_hello_seqno();
        tlv
    }

    pub fn new_ihu(rx_cost: Metric, interval: Duration, dest_address: Option<IpAddr>) -> Self {
        babel::Ihu::new(rx_cost, (interval.as_millis() / 10) as u16, dest_address).into()
    }

    pub fn new_update(
        interval: Duration,
        seqno: SeqNo,
        metric: Metric,
        subnet: Subnet,
        router_id: RouterId,
    ) -> Self {
        babel::Update::new(interval, seqno, metric, subnet, router_id).into()
    }
}

impl Codec {
    pub fn new() -> Self {
        Codec {
            codec: babel::Codec::new(),
        }
    }
}

impl Decoder for Codec {
    type Item = ControlPacket;
    type Error = std::io::Error;

    fn decode(&mut self, buf: &mut BytesMut) -> Result<Option<Self::Item>, Self::Error> {
        self.codec.decode(buf)
    }
}

impl Encoder<ControlPacket> for Codec {
    type Error = io::Error;

    fn encode(&mut self, message: ControlPacket, buf: &mut BytesMut) -> Result<(), Self::Error> {
        self.codec.encode(message, buf)
    }
}
