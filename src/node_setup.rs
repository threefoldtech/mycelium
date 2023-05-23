use futures::TryStreamExt;
use rtnetlink::Handle;
use x25519_dalek::PublicKey;
use std::{
    net::{IpAddr, Ipv6Addr},
    sync::Arc,
};
use tokio_tun::{Tun, TunBuilder};

pub const TUN_NAME: &str = "tun0";
pub const TUN_ROUTE_DEST: Ipv6Addr = Ipv6Addr::new(0x200, 0, 0, 0, 0, 0, 0, 0);
pub const TUN_ROUTE_PREFIX: u8 = 7;

// Create a TUN interface
pub fn create_tun_interface() -> Result<Arc<Tun>, Box<dyn std::error::Error>> {
    let tun = TunBuilder::new()
        .name("tun0")
        .tap(false)
        .mtu(1420)
        .packet_info(false)
        .up()
        .try_build()?;

    Ok(Arc::new(tun))
}


pub async fn retrieve_tun_link_index(handle: Handle) -> Result<u32, Box<dyn std::error::Error>> {
    let mut link_req = handle.link().get().match_name(TUN_NAME.to_string()).execute();
    let link_index = if let Some(link) = link_req.try_next().await? {
        link.header.index
    } else {
        panic!("link not found");
    };

    Ok(link_index)
}

// Add address to TUN interface
// this automatically creates a routing entry (for the /64 prefix)
pub async fn add_address(handle: Handle, addr: Ipv6Addr, link_index: u32) -> Result<u32, Box<dyn std::error::Error>> {
    // add address to tun interface
    handle
        .address()
        .add(
            link_index,
            IpAddr::V6(addr),
            64,
        )
        .execute()
        .await?;

    Ok(link_index)
}


// Adding route to TUN interface
pub async fn add_route(handle: Handle, link_index: u32) -> Result<(), Box<dyn std::error::Error>> {
    // add route to tun interface
    let route = handle.route();
    route
        .add()
        .v6()
        .destination_prefix(TUN_ROUTE_DEST, TUN_ROUTE_PREFIX)
        .output_interface(link_index)
        .execute()
        .await?;

    Ok(())
}


pub async fn setup_node(addr: Ipv6Addr) -> Result<Arc<Tun>, Box<dyn std::error::Error>> {

    let tun = match create_tun_interface() {
        Ok(tun) => {
            println!("TUN interface created");
            tun
        }
        Err(e) => {
            panic!("Error creating TUN interface: {}", e);
        }
    };


    let (conn, handle, _) = rtnetlink::new_connection()?;
    tokio::spawn(conn);

    let tun_link_index = match retrieve_tun_link_index(handle.clone()).await {
        Ok(link_index) => {
            println!("TUN interface link index retrieved");
            link_index
        }
        Err(e) => {
            panic!("Error retrieving TUN interface link index: {}", e);
        }
    };

    match add_address(handle.clone(), addr, tun_link_index).await {
        Ok(_) => {
            println!("Address added to TUN interface");
        }
        Err(e) => {
            panic!("Error adding address to TUN interface: {}", e);
        }
    };

    // add route for /7 (global scope for the overlay)
    match add_route(handle.clone(), tun_link_index).await {
        Ok(_) => {
            println!("Route added to TUN interface");
        }
        Err(e) => {
            panic!("Error adding route to TUN interface: {}", e);
        }
    };

    Ok(tun)
}
