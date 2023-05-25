use aes_gcm::{
    aead::{Aead, AeadCore, KeyInit, OsRng as CryptOsRng},
    Aes256Gcm,
    Key as AesKey, // Or `Aes128Gcm`
};
use blake2::digest::{Update, VariableOutput};
use blake2::Blake2bVar;
use rand_core::OsRng;
use std::fs::{File, OpenOptions};
use std::io::{Read, Write};
use std::net::Ipv6Addr;
use std::path::Path;
use x25519_dalek::{PublicKey, SharedSecret, StaticSecret};

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

pub fn shared_secret_from_keypair(secret: &StaticSecret, pubkey: &PublicKey) -> SharedSecret {
    secret.diffie_hellman(pubkey)
}

pub fn generate_addr_from_pubkey(pubkey: &PublicKey) -> Ipv6Addr {
    let mut hasher = Blake2bVar::new(16).unwrap(); // output ipv6 is 16 bytes
    hasher.update(pubkey.as_bytes());
    let mut buf = [0u8; 16];
    hasher.finalize_variable(&mut buf).unwrap();
    buf[0] = 0x02 | buf[0] & 0x01;
    let addr = Ipv6Addr::from(buf);
    println!("output buf : {:?}", addr);

    addr
}

// when a node sends a datapacket, it will encrypt it using the shared secret (generated from a public key) and a nonce
// the publickey is sent in the clear, the nonce is appended to the end of the raw_data
// note: the raw_data gets encrypted first, then the nonce is appended to the end of the encrypted data
pub fn encrypt_raw_data(raw_data_without_nonce: Vec<u8>, shared_secret: SharedSecret) -> Vec<u8> {
    let key: AesKey<Aes256Gcm> = (*shared_secret.as_bytes()).into();
    let nonce = Aes256Gcm::generate_nonce(&mut CryptOsRng);
    println!("encryption nonce : {:?}", nonce);

    let cipher = Aes256Gcm::new(&key);
    let mut encrypted_data = cipher
        .encrypt(&nonce, raw_data_without_nonce.as_ref())
        .unwrap();

    encrypted_data.extend_from_slice(nonce.as_ref());

    encrypted_data
}

// when a node receives a datapacket, it will decrypt it using the shared secret (generated from a public key) and a nonce
// the nonce is 96-bits in size and is located at the end of the packet
pub fn decrypt_raw_data(encrypted_raw_data: Vec<u8>, shared_secret: SharedSecret) -> Vec<u8> {
    let key: AesKey<Aes256Gcm> = (*shared_secret.as_bytes()).into();
    let nonce = &encrypted_raw_data[encrypted_raw_data.len() - 12..];
    println!("decryption nonce : {:?}", nonce);
    let data = encrypted_raw_data[..encrypted_raw_data.len() - 12].to_vec();

    let cipher = Aes256Gcm::new(&key);
    let decrypted_data = cipher.decrypt(nonce.into(), data.as_ref()).unwrap();

    decrypted_data.to_vec()
}
