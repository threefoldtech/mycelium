use tokio_tun::{Tun, TunBuilder};
use std::{
    sync::Arc,
    net::Ipv4Addr,
    error::Error,
};
use rtnetlink::Handle;
use futures::stream::TryStreamExt;

pub const TUN_NAME: &str = "tun0";
pub const TUN_ROUTE_DEST: Ipv4Addr = Ipv4Addr::new(10, 0, 0, 0);
pub const TUN_ROUTE_PREFIX: u8 = 16;

// Create a TUN interface
pub fn create_tun_interface(int_addr: Ipv4Addr) -> Result<Arc<Tun>, Box<dyn Error>> {
    let tun = TunBuilder::new()
        .name(TUN_NAME)
        .tap(false)
        .mtu(1420)
        .packet_info(false)
        .address(int_addr)
        .broadcast(Ipv4Addr::new(255, 255, 255, 0))
        .up()
        .try_build()?;

    Ok(Arc::new(tun))
}

// Add a route to the TUN interface
pub async fn add_route(handle: Handle) -> Result<(), Box<dyn Error>> {
    let mut link_request = handle
        .link()
        .get()
        .match_name(String::from(TUN_NAME))
        .execute();

    let link_idx = if let Some(link) = link_request.try_next().await? {
        link.header.index
    } else {
        eprintln!("link not found");
        panic!("link not found");
    };

    let route = handle.route();
    route
        .add()
        .v4()
        .destination_prefix(TUN_ROUTE_DEST, TUN_ROUTE_PREFIX)
        .output_interface(link_idx)
        .execute()
        .await?;

    Ok(())
}

pub async fn setup_node(tun_addr: Ipv4Addr) -> Result<Arc<Tun>, Box<dyn Error>> {
    let tun = create_tun_interface(tun_addr)?;
    println!("Interface '{}' ({}) created", TUN_NAME, tun_addr);

    let (conn, handle, _) = rtnetlink::new_connection()?;
    tokio::spawn(conn);

    add_route(handle.clone()).await?;

    println!("Static route created");
    
    Ok(tun)
}

