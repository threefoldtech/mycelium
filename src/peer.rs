use tokio::{
    io::{AsyncReadExt, AsyncWriteExt},
    net::TcpStream,
    sync::{mpsc, Mutex},
    select,
};
use std::{error::Error, sync::Arc};

#[derive(Debug)]
pub struct Peer {
    pub id: String,
    pub to_peer: mpsc::UnboundedSender<Vec<u8>>,
}

impl Peer {
    pub fn new(id: String, to_tun: mpsc::UnboundedSender<Vec<u8>>, mut stream: TcpStream) -> Result<Self, Box<dyn Error>> {

        // Create channel for each peer
        let (to_peer, mut from_tun) = mpsc::unbounded_channel::<Vec<u8>>();

        tokio::spawn(async move {
            loop {
                let link_mtu = 1500;
                let mut read_buf = vec![0u8; link_mtu];

                select! {
                    // Read from TCP stream, write to 'to_tun'
                    read_result = stream.read(&mut read_buf) => {
                        match read_result {
                            Ok(n) => {
                                // Truncate buffer, removing any extra bytes
                                read_buf.truncate(n);

                                // For testing purposes
                                println!("Received bytes from peer: {:?}", read_buf);

                                // Send to TUN interface
                                if let Err(error) = to_tun.send(read_buf.clone()) {
                                    eprintln!("Error sending to TUN: {}", error);
                                }
                            },
                            Err(error) => {
                                eprintln!("Error reading from TCP stream: {}", error);
                            }
                        }
                    }
                    // Read from 'from_tun' receiver, write to TCP stream
                    Some(packet) = from_tun.recv() => {
                        if let Err(error) = stream.write_all(&packet).await {
                            eprintln!("Error writing to TCP stream: {}", error);
                        }
                    }

                }
            }
        });

        Ok(Self {
            id,
            to_peer,
        })
    }
}