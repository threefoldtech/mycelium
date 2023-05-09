use crate::{
    packet::{
        BabelPacketBody, BabelPacketHeader, BabelTLV, BabelTLVType, ControlPacket, ControlStruct,
        DataPacket,
    },
    peer::Peer,
    routing_table::{RouteEntry, RouteKey, RoutingTable},
    source_table::{SourceKey, SourceTable, FeasibilityDistance},
};
use std::{
    collections::HashMap,
    net::{IpAddr, Ipv4Addr},
    sync::{Arc, Mutex},
};
use tokio::sync::mpsc::{self, UnboundedReceiver, UnboundedSender};
use tokio_tun::Tun;

const HELLO_INTERVAL: u16 = 4;
const IHU_INTERVAL: u16 = HELLO_INTERVAL * 3;

#[derive(Clone)]
pub struct Router {
    // The peer interfaces are the known neighbors of this node
    peer_interfaces: Arc<Mutex<Vec<Peer>>>,
    pub router_control_tx: UnboundedSender<ControlStruct>,
    pub router_data_tx: UnboundedSender<DataPacket>,
    pub node_tun: Arc<Tun>,
    pub sent_hello_timestamps: Arc<Mutex<HashMap<IpAddr, tokio::time::Instant>>>,
    pub routing_table: Arc<Mutex<RoutingTable>>,
    pub source_table: Arc<Mutex<SourceTable>>,
    pub router_seqno: u16,
}

impl Router {
    pub fn new(node_tun: Arc<Tun>) -> Self {

        // Tx is passed onto each new peer instance. This enables peers to send control packets to the router.
        let (router_control_tx, router_control_rx) = mpsc::unbounded_channel::<ControlStruct>();
        // Tx is passed onto each new peer instance. This enables peers to send data packets to the router.
        let (router_data_tx, router_data_rx) = mpsc::unbounded_channel::<DataPacket>();

        let router = Router {
            peer_interfaces: Arc::new(Mutex::new(Vec::new())),
            router_control_tx,
            router_data_tx,
            node_tun,
            sent_hello_timestamps: Arc::new(Mutex::new(HashMap::new())),
            routing_table: Arc::new(Mutex::new(RoutingTable::new())),
            source_table: Arc::new(Mutex::new(SourceTable::new())),
            router_seqno: 0,
        };

        tokio::spawn(Router::start_periodic_hello_sender(router.clone()));
        tokio::spawn(Router::handle_incoming_control_packet(router_control_rx));
        tokio::spawn(Router::handle_incoming_data_packets(router.clone(), router_data_rx));

        router
    }

    async fn handle_incoming_control_packet(mut router_control_rx: UnboundedReceiver<ControlStruct>) {
        loop {
            while let Some(control_struct) = router_control_rx.recv().await {
                match control_struct.control_packet.body.tlv_type {
                    BabelTLVType::AckReq => todo!(),
                    BabelTLVType::Ack => todo!(),
                    BabelTLVType::Hello => Self::handle_incoming_hello(control_struct),
                    BabelTLVType::IHU => todo!(),
                    BabelTLVType::NextHop => todo!(),
                    BabelTLVType::Update => todo!(),
                    BabelTLVType::RouteReq => todo!(),
                    BabelTLVType::SeqnoReq => todo!(),
                }
            }
        } 
    }

    async fn handle_incoming_data_packets(self, mut router_data_rx: UnboundedReceiver<DataPacket>) {
        // If the destination IP of the data packet matches with the IP address of this node's TUN interface
        // we should forward the data packet towards the TUN interface.
        // If the destination IP doesn't match, we need to lookup if we have a matching peer instance
        // where the destination IP matches with the peer's overlay IP. If we do, we should forward the
        // data packet to the peer's to_peer_data channel.
        loop {
            while let Some(data_packet) = router_data_rx.recv().await {
                let dest_ip = data_packet.dest_ip;
                if dest_ip == self.node_tun.address().unwrap() {
                    if let Err(e) = self.node_tun.send(&data_packet.raw_data).await {
                        eprintln!("Error sending data packet to TUN interface: {:?}", e);
                    }
                } else {
                    let matching_peer = self.get_peer_by_ip(dest_ip);
                    if let Some(peer) = matching_peer {
                        if let Err(e) = peer.to_peer_data.send(data_packet) {
                            eprintln!("Error sending data packet to peer: {:?}", e);
                        }
                    } else {
                        eprintln!("No matching peer found for data packet");
                    }
                }
            }
        }
    }

    async fn start_periodic_hello_sender(self) {
        for peer in self.peer_interfaces.lock().unwrap().iter_mut() {
            let hello = ControlPacket::new_hello(peer.last_sent_hello_seqno, HELLO_INTERVAL);
            println!("Sending hello to peer: {:?}", peer.stream_ip);
            if let Err(error) = peer.to_peer_control.send(hello) {
                eprintln!("Error sending hello to peer: {}", error);
            }
        } 
    }

    fn handle_incoming_hello(control_struct: ControlStruct) {
        let destination_ip = control_struct.src_overlay_ip;
        control_struct.reply(ControlPacket::new_ihu(IHU_INTERVAL, destination_ip));
    }


    pub fn add_directly_connected_peer(&self, peer: Peer) {
        self.peer_interfaces.lock().unwrap().push(peer);
    }

    pub fn get_node_tun_address(&self) -> Ipv4Addr {
        self.node_tun.address().unwrap()
    }

    pub fn get_peer_interfaces(&self) -> Vec<Peer> {
        self.peer_interfaces.lock().unwrap().clone()
    }

    fn get_peer_by_ip(&self, peer_ip: Ipv4Addr) -> Option<Peer> {
        let peers = self.get_peer_interfaces();
        let matching_peer = peers.iter().find(|peer| peer.overlay_ip == peer_ip);

        match matching_peer {
            Some(peer) => Some(peer.clone()),
            None => None,
        }
    }
}


