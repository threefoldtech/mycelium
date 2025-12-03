//! Abstraction over diffie hellman, symmetric encryption, and hashing.

use core::fmt;
use std::{
    error::Error,
    fmt::Display,
    net::Ipv6Addr,
    ops::{Deref, DerefMut},
};

use aes_gcm::{aead::OsRng, AeadCore, AeadInPlace, Aes256Gcm, Key, KeyInit};
use serde::{de::Visitor, Deserialize, Serialize};

/// Default MTU for a packet. Ideally this would not be needed and the [`PacketBuffer`] takes a
/// const generic argument which is then expanded with the needed extra space for the buffer,
/// however as it stands const generics can only be used standalone and not in a constant
/// expression. This _is_ possible on nightly rust, with a feature gate (generic_const_exprs).
const PACKET_SIZE: usize = 1_400;

/// Size of an AES_GCM tag in bytes.
const AES_TAG_SIZE: usize = 16;

/// Size of an AES_GCM nonce in bytes.
const AES_NONCE_SIZE: usize = 12;

/// Size of user defined data header. This header will be part of the encrypted data.
const DATA_HEADER_SIZE: usize = 4;

/// Size of a `PacketBuffer`.
const PACKET_BUFFER_SIZE: usize = PACKET_SIZE + AES_TAG_SIZE + AES_NONCE_SIZE + DATA_HEADER_SIZE;

/// A public key used as part of Diffie Hellman key exchange. It is derived from a [`SecretKey`].
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub struct PublicKey(x25519_dalek::PublicKey);

/// A secret used as part of Diffie Hellman key exchange.
///
/// This type intentionally does not implement or derive [`Debug`] to avoid accidentally leaking
/// secrets in logs.
#[derive(Clone)]
pub struct SecretKey(x25519_dalek::StaticSecret);

/// A statically computed secret from a [`SecretKey`] and a [`PublicKey`].
///
/// This type intentionally does not implement or derive [`Debug`] to avoid accidentally leaking
/// secrets in logs.
#[derive(Clone)]
pub struct SharedSecret([u8; 32]);

/// A buffer for packets. This holds enough space to  encrypt a packet in place without
/// reallocating.
///
/// Internally, the buffer is created with an additional header. Because this header is part of the
/// encrypted content, it is not included in the global version set by the main packet header. As
/// such, an internal version is included.
pub struct PacketBuffer {
    buf: Vec<u8>,
    /// Amount of bytes written in the buffer
    size: usize,
}

/// A reference to the header in a [`PacketBuffer`].
pub struct PacketBufferHeader<'a> {
    data: &'a [u8; DATA_HEADER_SIZE],
}

/// A mutable reference to the header in a [`PacketBuffer`].
pub struct PacketBufferHeaderMut<'a> {
    data: &'a mut [u8; DATA_HEADER_SIZE],
}

/// Opaque type indicating decryption failed.
#[derive(Debug, Clone, Copy)]
pub struct DecryptionError;

impl Display for DecryptionError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str("Decryption failed, invalid or insufficient encrypted content for this key")
    }
}

impl Error for DecryptionError {}

impl SecretKey {
    /// Generate a new `StaticSecret` using [`OsRng`] as an entropy source.
    pub fn new() -> Self {
        SecretKey(x25519_dalek::StaticSecret::random_from_rng(OsRng))
    }

    /// View this `SecretKey` as a byte array.
    #[inline]
    pub fn as_bytes(&self) -> &[u8; 32] {
        self.0.as_bytes()
    }

    /// Computes the [`SharedSecret`] from this `SecretKey` and a [`PublicKey`].
    pub fn shared_secret(&self, other: &PublicKey) -> SharedSecret {
        SharedSecret(self.0.diffie_hellman(&other.0).to_bytes())
    }
}

impl Default for SecretKey {
    fn default() -> Self {
        Self::new()
    }
}

impl PublicKey {
    /// Generates an [`Ipv6Addr`] from a `PublicKey`.
    ///
    /// The generated address is guaranteed to be part of the `400::/7` range.
    pub fn address(&self) -> Ipv6Addr {
        let mut hasher = blake3::Hasher::new();
        hasher.update(self.as_bytes());
        let mut buf = [0; 16];
        hasher.finalize_xof().fill(&mut buf);
        // Mangle the first byte to be of the expected form. Because of the network range
        // requirement, we MUST set the third bit, and MAY set the last bit. Instead of discarding
        // the first 7 bits of the hash, use the first byte to determine if the last bit is set.
        // If there is an odd number of bits set in the first byte, set the last bit of the result.
        let lsb = buf[0].count_ones() as u8 % 2;
        buf[0] = 0x04 | lsb;
        Ipv6Addr::from(buf)
    }

    /// Convert this `PublicKey` to a byte array.
    pub fn to_bytes(self) -> [u8; 32] {
        self.0.to_bytes()
    }

    /// View this `PublicKey` as a byte array.
    pub fn as_bytes(&self) -> &[u8; 32] {
        self.0.as_bytes()
    }
}

impl SharedSecret {
    /// Encrypt a [`PacketBuffer`] using the `SharedSecret` as key.
    ///
    /// Internally, a new random nonce will be generated using the OS's crypto rng generator. This
    /// nonce is appended to the encrypted data.
    pub fn encrypt(&self, mut data: PacketBuffer) -> Vec<u8> {
        let key: Key<Aes256Gcm> = self.0.into();
        let nonce = Aes256Gcm::generate_nonce(OsRng);

        let cipher = Aes256Gcm::new(&key);
        let tag = cipher
            .encrypt_in_place_detached(&nonce, &[], &mut data.buf[..data.size])
            .expect("Encryption can't fail; qed.");

        data.buf[data.size..data.size + AES_TAG_SIZE].clone_from_slice(tag.as_slice());
        data.buf[data.size + AES_TAG_SIZE..data.size + AES_TAG_SIZE + AES_NONCE_SIZE]
            .clone_from_slice(&nonce);

        data.buf.truncate(data.size + AES_NONCE_SIZE + AES_TAG_SIZE);

        data.buf
    }

    /// Decrypt a message previously encrypted with an equivalent `SharedSecret`. In other words, a
    /// message that was previously created by the [`SharedSecret::encrypt`] method.
    ///
    /// Internally, this messages assumes that a 12 byte nonce is present at the end of the data.
    /// If the passed in data to decrypt does not contain a valid nonce, decryption fails and an
    /// opaque error is returned. As an extension to this, if the data is not of sufficient length
    /// to contain a valid nonce, an error is returned immediately.
    pub fn decrypt(&self, mut data: Vec<u8>) -> Result<PacketBuffer, DecryptionError> {
        // Make sure we have sufficient data (i.e. a nonce).
        if data.len() < AES_NONCE_SIZE + AES_TAG_SIZE + DATA_HEADER_SIZE {
            return Err(DecryptionError);
        }

        let data_len = data.len();

        let key: Key<Aes256Gcm> = self.0.into();
        {
            let (data, nonce) = data.split_at_mut(data_len - AES_NONCE_SIZE);
            let (data, tag) = data.split_at_mut(data.len() - AES_TAG_SIZE);

            let cipher = Aes256Gcm::new(&key);
            cipher
                .decrypt_in_place_detached((&*nonce).into(), &[], data, (&*tag).into())
                .map_err(|_| DecryptionError)?;
        }

        Ok(PacketBuffer {
            // We did not remove the scratch space used for TAG and NONCE.
            size: data.len() - AES_TAG_SIZE - AES_NONCE_SIZE,
            buf: data,
        })
    }
}

impl PacketBuffer {
    /// Create a new blank `PacketBuffer`.
    pub fn new() -> Self {
        Self {
            buf: vec![0; PACKET_BUFFER_SIZE],
            size: 0,
        }
    }

    /// Get a reference to the packet header.
    pub fn header(&self) -> PacketBufferHeader<'_> {
        PacketBufferHeader {
            data: self.buf[..DATA_HEADER_SIZE]
                .try_into()
                .expect("Header size constant is correct; qed"),
        }
    }

    /// Get a mutable reference to the packet header.
    pub fn header_mut(&mut self) -> PacketBufferHeaderMut<'_> {
        PacketBufferHeaderMut {
            data: <&mut [u8] as TryInto<&mut [u8; DATA_HEADER_SIZE]>>::try_into(
                &mut self.buf[..DATA_HEADER_SIZE],
            )
            .expect("Header size constant is correct; qed"),
        }
    }

    /// Get a reference to the entire useable inner buffer.
    pub fn buffer(&self) -> &[u8] {
        let buf_end = self.buf.len() - AES_NONCE_SIZE - AES_TAG_SIZE;
        &self.buf[DATA_HEADER_SIZE..buf_end]
    }

    /// Get a mutable reference to the entire useable internal buffer.
    pub fn buffer_mut(&mut self) -> &mut [u8] {
        let buf_end = self.buf.len() - AES_NONCE_SIZE - AES_TAG_SIZE;
        &mut self.buf[DATA_HEADER_SIZE..buf_end]
    }

    /// Sets the amount of bytes in use by the buffer.
    pub fn set_size(&mut self, size: usize) {
        self.size = size + DATA_HEADER_SIZE;
    }
}

impl Default for PacketBuffer {
    fn default() -> Self {
        Self::new()
    }
}

impl From<[u8; 32]> for SecretKey {
    /// Load a secret key from a byte array.
    fn from(bytes: [u8; 32]) -> SecretKey {
        SecretKey(x25519_dalek::StaticSecret::from(bytes))
    }
}

impl fmt::Display for PublicKey {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(&faster_hex::hex_string(self.as_bytes()))
    }
}

impl Serialize for PublicKey {
    fn serialize<S>(&self, serializer: S) -> Result<S::Ok, S::Error>
    where
        S: serde::Serializer,
    {
        serializer.serialize_str(&faster_hex::hex_string(self.as_bytes()))
    }
}

struct PublicKeyVisitor;
impl Visitor<'_> for PublicKeyVisitor {
    type Value = PublicKey;

    fn expecting(&self, formatter: &mut std::fmt::Formatter) -> std::fmt::Result {
        formatter.write_str("A hex encoded public key (64 characters)")
    }

    fn visit_str<E>(self, v: &str) -> Result<Self::Value, E>
    where
        E: serde::de::Error,
    {
        if v.len() != 64 {
            Err(E::custom("Public key is 64 characters long"))
        } else {
            let mut backing = [0; 32];
            faster_hex::hex_decode(v.as_bytes(), &mut backing)
                .map_err(|_| E::custom("PublicKey is not valid hex"))?;
            Ok(PublicKey(backing.into()))
        }
    }
}

impl<'de> Deserialize<'de> for PublicKey {
    fn deserialize<D>(deserializer: D) -> Result<Self, D::Error>
    where
        D: serde::Deserializer<'de>,
    {
        deserializer.deserialize_str(PublicKeyVisitor)
    }
}

impl From<[u8; 32]> for PublicKey {
    /// Given a byte array, construct a `PublicKey`.
    fn from(bytes: [u8; 32]) -> PublicKey {
        PublicKey(x25519_dalek::PublicKey::from(bytes))
    }
}

impl TryFrom<&str> for PublicKey {
    type Error = faster_hex::Error;

    fn try_from(value: &str) -> Result<Self, Self::Error> {
        let mut output = [0u8; 32];
        faster_hex::hex_decode(value.as_bytes(), &mut output)?;
        Ok(PublicKey::from(output))
    }
}

impl From<&SecretKey> for PublicKey {
    fn from(value: &SecretKey) -> Self {
        PublicKey(x25519_dalek::PublicKey::from(&value.0))
    }
}

impl Deref for SharedSecret {
    type Target = [u8; 32];

    fn deref(&self) -> &Self::Target {
        &self.0
    }
}

impl Deref for PacketBuffer {
    type Target = [u8];

    fn deref(&self) -> &Self::Target {
        &self.buf[DATA_HEADER_SIZE..self.size]
    }
}

impl Deref for PacketBufferHeader<'_> {
    type Target = [u8; DATA_HEADER_SIZE];

    fn deref(&self) -> &Self::Target {
        self.data
    }
}

impl Deref for PacketBufferHeaderMut<'_> {
    type Target = [u8; DATA_HEADER_SIZE];

    fn deref(&self) -> &Self::Target {
        self.data
    }
}

impl DerefMut for PacketBufferHeaderMut<'_> {
    fn deref_mut(&mut self) -> &mut Self::Target {
        self.data
    }
}

impl fmt::Debug for PacketBuffer {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("PacketBuffer")
            .field("data", &"...")
            .field("len", &self.size)
            .finish()
    }
}

#[cfg(test)]
mod tests {
    use super::{PacketBuffer, SecretKey, AES_NONCE_SIZE, AES_TAG_SIZE, DATA_HEADER_SIZE};

    #[test]
    /// Test if encryption works in general. We just create some random value and encrypt it.
    /// Specifically, this will help to catch runtime panics in case AES_TAG_SIZE or AES_NONCE_SIZE
    /// don't have a proper value aligned with the underlying AES_GCM implementation.
    fn encryption_succeeds() {
        let k1 = SecretKey::new();
        let k2 = SecretKey::new();

        let ss = k1.shared_secret(&(&k2).into());

        let mut pb = PacketBuffer::new();
        let data = b"vnno30nv f654q364 vfsv 44"; // Random keyboard smash.

        pb.buffer_mut()[..data.len()].copy_from_slice(data);
        pb.set_size(data.len());

        // We only care that this does not panic.
        let res = ss.encrypt(pb);
        // At the same time, check expected size.
        assert_eq!(
            res.len(),
            data.len() + DATA_HEADER_SIZE + AES_TAG_SIZE + AES_NONCE_SIZE
        );
    }

    #[test]
    /// Encrypt a value and then decrypt it. This makes sure the decrypt flow and encrypt flow
    /// match, and both follow the expected format. Also, we don't reuse the shared secret for
    /// decryption, but instead generate the secret again the other way round, to simulate a remote
    /// node.
    fn encrypt_decrypt_roundtrip() {
        let k1 = SecretKey::new();
        let k2 = SecretKey::new();

        let ss1 = k1.shared_secret(&(&k2).into());
        let ss2 = k2.shared_secret(&(&k1).into());

        // This assertion is not strictly necessary as it will be checked below implicitly.
        assert_eq!(ss1.as_slice(), ss2.as_slice());

        let data = b"dsafjiqjo23  u2953u8 3oid fjo321j";
        let mut pb = PacketBuffer::new();

        pb.buffer_mut()[..data.len()].copy_from_slice(data);
        pb.set_size(data.len());

        let res = ss1.encrypt(pb);

        let original = ss2.decrypt(res).expect("Decryption works");

        assert_eq!(&*original, &data[..]);
    }

    #[test]
    /// Test if PacketBufferHeaderMut actually modifies the PacketBuffer storage.
    fn modify_header() {
        let mut pb = PacketBuffer::new();
        let mut header = pb.header_mut();

        header[0] = 1;
        header[1] = 2;
        header[2] = 3;
        header[3] = 4;

        assert_eq!(pb.buf[..DATA_HEADER_SIZE], [1, 2, 3, 4]);
    }

    #[test]
    /// Verify [`PacketBuffer::buffer`] and [`PacketBuffer::buffer_mut`] actually have the
    /// appropriate size.
    fn buffer_mapping() {
        let mut pb = PacketBuffer::new();

        assert_eq!(pb.buffer().len(), super::PACKET_SIZE);
        assert_eq!(pb.buffer_mut().len(), super::PACKET_SIZE);
    }
}
