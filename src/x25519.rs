use blake2::digest::{Update, VariableOutput};
use blake2::Blake2bVar;
use rand_core::OsRng;
use std::fs::{File, OpenOptions};
use std::io::{Read, Write};
use std::path::Path;
use std::{
    net::Ipv6Addr,
};
use x25519_dalek::{PublicKey, StaticSecret, SharedSecret};

// Read the secret key from a file if it exists, otherwise generate a new one and write it to a file
// Returns the secret key and the corresponding public key
pub fn get_keypair() -> Result<(StaticSecret, PublicKey), Box<dyn std::error::Error>> {
    let path = Path::new("keys.txt");

    let (secret_key, public_key) = if path.exists() {
        let mut file = File::open(&path).expect("Failed to open file");
        let mut secret_bytes = [0u8; 32];
        file.read(&mut secret_bytes).expect("Failed to read file");

        let secret_key = StaticSecret::from(secret_bytes);
        let public_key = PublicKey::from(&secret_key);

        (secret_key, public_key)
    } else {
        let secret_key = StaticSecret::new(OsRng);
        let public_key = PublicKey::from(&secret_key);

        let mut file = OpenOptions::new()
            .write(true)
            .create_new(true)
            .open(&path)
            .expect("Failed to open file");

        file.write_all(secret_key.to_bytes().as_ref())
            .expect("Failed to write to file");

        (secret_key, public_key)
    };

    Ok((secret_key, public_key))
}

pub fn shared_secret_from_keypair(secret: StaticSecret, pubkey: PublicKey) -> SharedSecret {
    secret.diffie_hellman(&pubkey)
}

pub fn generate_addr_from_pubkey(pubkey: &PublicKey) -> Ipv6Addr {
    let mut hasher = Blake2bVar::new(16).unwrap(); // output ipv6 is 16 bytes
    hasher.update(pubkey.as_bytes());
    let mut buf = [0u8; 16];
    hasher.finalize_variable(&mut buf).unwrap();

    let ipv6_bytes: [u8; 16] = [
        0x02, 0x00, // This prefix ensures the address falls into the 200::/7 range
        buf[0], buf[1], buf[2], buf[3], buf[4], buf[5], buf[6], buf[7], buf[8], buf[9], buf[10],
        buf[11], buf[12], buf[13],
    ];

    let addr = Ipv6Addr::from(ipv6_bytes);
    println!("output buf : {:?}", addr);

    addr
}
