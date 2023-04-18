use crate::{packet_control::Packet, peer_manager};

pub fn route_packet(packet: Packet, peer_manager: &peer_manager::PeerManager) {
    // To route the packet to the correct destination we need to route the packet to:
    // 1. It's own TUN interface is the packet destination's IP is the same is this node's IP
    // OR
    // 2. Towards a correct to_peer based on the destination IP (lookup in PeerManager)
    // OR
    // 3. Drop the packet if we can't find a peer that matches the destination IP

    // 1.

    // 2.

    println!("We reached here!");



    // TEMPORARY: we just send it to the first peer
    if let Some(first_peer) = &peer_manager.known_peers.lock().unwrap().get(0) {
        println!("Routing the message to the first peer");
        first_peer.to_peer.send(packet);
    }

    // 3.


}