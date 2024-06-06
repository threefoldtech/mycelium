use std::net::IpAddr;

use mycelium::crypto::PublicKey;
use serde::Serialize;

#[derive(Debug, Serialize)]
struct InspectOutput {
    #[serde(rename = "publicKey")]
    public_key: PublicKey,
    address: IpAddr,
}

/// Inspect the given pubkey, or the local key if no pubkey is given
pub fn inspect(pubkey: PublicKey, json: bool) -> Result<(), Box<dyn std::error::Error>> {
    let address = pubkey.address().into();
    if json {
        let out = InspectOutput {
            public_key: pubkey,
            address,
        };

        let out_string = serde_json::to_string_pretty(&out)?;
        println!("{out_string}");
    } else {
        println!("Public key: {pubkey}");
        println!("Address: {address}");
    }

    Ok(())
}
