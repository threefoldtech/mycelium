use crate::{
    packet::{
        BabelPacketBody, BabelPacketHeader, BabelTLV, BabelTLVType, ControlPacket, ControlStruct,
        DataPacket,
    },
    peer::Peer,
    routing_table::{RouteEntry, RouteKey, RoutingTable},
    source_table::{SourceKey, SourceTable, FeasibilityDistance}, timers::Timer,
};
use std::{
    collections::HashMap,
    net::{IpAddr, Ipv4Addr},
    sync::{Arc, Mutex},
};
use rand::Rng;
use tokio::sync::mpsc::{self, UnboundedReceiver, UnboundedSender};
use tokio_tun::Tun;

const HELLO_INTERVAL: u16 = 4;
const IHU_INTERVAL: u16 = HELLO_INTERVAL * 3;
const UPDATE_INTERVAL: u16 = HELLO_INTERVAL * 4;

#[derive(Clone)]
pub struct Router {
    pub router_id: u64,
    // The peer interfaces are the known neighbors of this node
    peer_interfaces: Arc<Mutex<Vec<Peer>>>,
    pub router_control_tx: UnboundedSender<ControlStruct>,
    pub router_data_tx: UnboundedSender<DataPacket>,
    pub node_tun: Arc<Tun>,
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
            router_id: rand::thread_rng().gen(),
            peer_interfaces: Arc::new(Mutex::new(Vec::new())),
            router_control_tx,
            router_data_tx,
            node_tun,
            routing_table: Arc::new(Mutex::new(RoutingTable::new())),
            source_table: Arc::new(Mutex::new(SourceTable::new())),
            router_seqno: 0,
        };

        tokio::spawn(Router::handle_incoming_control_packet(router.clone(), router_control_rx));
        tokio::spawn(Router::handle_incoming_data_packets(router.clone(), router_data_rx));
        tokio::spawn(Router::start_periodic_hello_sender(router.clone()));

        router
    }

    async fn handle_incoming_control_packet(self, mut router_control_rx: UnboundedReceiver<ControlStruct>) {
        loop {
            while let Some(control_struct) = router_control_rx.recv().await {
                match control_struct.control_packet.body.tlv_type {
                    BabelTLVType::AckReq => todo!(),
                    BabelTLVType::Ack => todo!(),
                    BabelTLVType::Hello => Self::handle_incoming_hello(control_struct),
                    BabelTLVType::IHU => Self::handle_incoming_ihu(&self, control_struct),
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

        let tun_addr = self.node_tun.address().unwrap();
        loop {
            while let Some(data_packet) = router_data_rx.recv().await {
                let dest_ip = data_packet.dest_ip;

                if dest_ip == tun_addr {
                    match self.node_tun.send(&data_packet.raw_data).await {
                        Ok(_) => {},
                        Err(e) => eprintln!("Error sending data packet to TUN interface: {:?}", e),
                    }
                } else {
                    match self.get_peer_by_ip(IpAddr::V4(dest_ip)) {
                        Some(peer) => {
                            match peer.to_peer_data.send(data_packet) {
                                Ok(_) => {},
                                Err(e) => eprintln!("Error sending data packet to peer: {:?}", e),
                            }
                        },
                        None => eprintln!("No matching peer found for data packet"),
                    }
                }
            }
        }
    }


    async fn start_periodic_hello_sender(self) {
        loop {
            tokio::time::sleep(tokio::time::Duration::from_secs(HELLO_INTERVAL as u64)).await;
            for peer in self.peer_interfaces.lock().unwrap().iter_mut() {
                // create a new hello packet for this peer
                let hello = ControlPacket::new_hello(peer, HELLO_INTERVAL);
                // set the last hello timestamp for this peer
                peer.time_last_received_hello = tokio::time::Instant::now();
                // send the hello packet to the peer
                println!("Sending hello to peer: {:?}", peer.overlay_ip);
                if let Err(error) = peer.to_peer_control.send(hello) {
                    eprintln!("Error sending hello to peer: {}", error);
                }
            } 
        }
    }

    fn handle_incoming_hello(control_struct: ControlStruct) {
        let destination_ip = control_struct.src_overlay_ip;
        control_struct.reply(ControlPacket::new_ihu(IHU_INTERVAL, destination_ip));
    }

    fn handle_incoming_ihu(&self, control_struct: ControlStruct) {
        let mut source_peer = self.get_source_peer_from_control_struct(control_struct);
        // reset the IHU timer associated with the peer
        source_peer.ihu_timer.reset(tokio::time::Duration::from_secs(IHU_INTERVAL as u64));
        // measure time between Hello and and IHU and set the link cost
        source_peer.link_cost = tokio::time::Instant::now().duration_since(source_peer.time_last_received_hello).as_millis() as u16;
        println!("Link cost for peer {:?} set to {}", source_peer.overlay_ip, source_peer.link_cost);

        println!("IHU timer for peer {:?} reset", source_peer.overlay_ip);
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

    pub fn get_peer_by_ip(&self, peer_ip: IpAddr) -> Option<Peer> {
        let peers = self.get_peer_interfaces();
        let matching_peer = peers.iter().find(|peer| peer.overlay_ip == peer_ip);

        match matching_peer {
            Some(peer) => Some(peer.clone()),
            None => None,
        }
    }

    fn get_source_peer_from_control_struct(&self, control_struct: ControlStruct) -> Peer {
        let source = control_struct.src_overlay_ip;

        self.get_peer_by_ip(source).unwrap()
    }

    pub fn print_routes(&self) {
        let routing_table = self.routing_table.lock().unwrap();
        for route in routing_table.table.iter() {
            println!("Route: {:?}/{:?} (with next-hop: {:?})", route.0.prefix, route.0.plen, route.1.next_hop);
            println!("As advertised by: {:?}", route.1.source.router_id);
        }
    }
}


