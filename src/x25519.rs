use std::fs::{File, OpenOptions};
use std::io::{Read, Write};
use std::path::Path;
use rand_core::OsRng;
use x25519_dalek::{PublicKey, StaticSecret};


// Read the secret key from a file if it exists, otherwise generate a new one and write it to a file
// Returns the secret key and the corresponding public key
pub fn get_keypair() -> Result<(StaticSecret, PublicKey), Box<dyn std::error::Error>>{
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

        file.write_all(secret_key.to_bytes().as_ref()).expect("Failed to write to file");

        (secret_key, public_key)
    };

    Ok((secret_key, public_key))
}