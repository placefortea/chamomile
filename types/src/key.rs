use rand_core::{CryptoRng, RngCore};
use secp256k1::{
    constants::ONE,
    ecdsa::{RecoverableSignature, RecoveryId},
    Message as SecpMessage, PublicKey as SecpPublicKey, Secp256k1, SecretKey as SecpSecretKey,
};
use sha3::{Digest, Keccak256};

pub use secp256k1;

use crate::types::{new_io_error, PeerId, PEER_ID_LENGTH};

pub const SECRET_KEY_LENGTH: usize = 32;
pub const PUBLIC_KEY_LENGTH: usize = 33;
pub const SIGNATURE_LENGTH: usize = 65;

/// Public Key
#[derive(Clone)]
pub struct PublicKey(SecpPublicKey);

/// Secret Key
pub struct SecretKey(SecpSecretKey);

pub struct Signature(RecoverableSignature);

/// The keypair, include pk, sk, address
pub struct Key {
    pub pub_key: PublicKey,
    pub sec_key: SecretKey,
}

impl Key {
    pub fn from_sec_key(sec_key: SecretKey) -> Self {
        let secp = Secp256k1::new();
        let pub_key = PublicKey(sec_key.0.public_key(&secp));

        Self { pub_key, sec_key }
    }

    pub fn default() -> Self {
        let sec_key = SecretKey(SecpSecretKey::from_slice(&ONE).unwrap());
        Self::from_sec_key(sec_key)
    }

    pub fn generate<R: CryptoRng + RngCore>(rng: &mut R) -> Key {
        let sec_key = SecretKey(SecpSecretKey::new(rng));
        Self::from_sec_key(sec_key)
    }

    pub fn peer_id(&self) -> PeerId {
        self.pub_key.peer_id()
    }

    pub fn public(&self) -> PublicKey {
        self.pub_key.clone()
    }

    pub fn sign(&self, msg: &[u8]) -> Signature {
        let mut hasher = Keccak256::new();
        hasher.update(msg);
        let result = hasher.finalize();
        let msg = SecpMessage::from_digest(result.into());
        let secp = Secp256k1::new();
        let sign = secp.sign_ecdsa_recoverable(&msg, &self.sec_key.0);
        Signature(sign)
    }

    pub fn sign_eth(&self, message: &[u8]) -> Signature {
        const PREFIX: &str = "\x19Ethereum Signed Message:\n";

        let len = message.len();
        let len_string = len.to_string();

        let mut eth_message = Vec::with_capacity(PREFIX.len() + len_string.len() + len);
        eth_message.extend_from_slice(PREFIX.as_bytes());
        eth_message.extend_from_slice(len_string.as_bytes());
        eth_message.extend_from_slice(message);

        self.sign(&eth_message)
    }

    pub fn to_db_bytes(&self) -> Vec<u8> {
        let mut bytes = vec![];
        bytes.extend(&self.sec_key.0.secret_bytes());
        bytes
    }

    pub fn from_db_bytes(bytes: &[u8]) -> std::io::Result<Self> {
        if bytes.len() < SECRET_KEY_LENGTH {
            return Err(new_io_error("keypair from db bytes failure."));
        }
        let sec_key = SecretKey(
            SecpSecretKey::from_slice(&bytes[..SECRET_KEY_LENGTH])
                .map_err(|_| new_io_error("secret key from db bytes failure."))?,
        );
        Ok(Self::from_sec_key(sec_key))
    }
}

impl PublicKey {
    pub fn new(pk: SecpPublicKey) -> Self {
        Self(pk)
    }

    pub fn raw(&self) -> &SecpPublicKey {
        &self.0
    }

    pub fn peer_id(&self) -> PeerId {
        let public_key = self.0.serialize_uncompressed();
        let mut hasher = Keccak256::new();
        hasher.update(&public_key[1..]);
        let result = hasher.finalize();
        let mut bytes = [0u8; PEER_ID_LENGTH];
        bytes.copy_from_slice(&result[12..]);
        PeerId(bytes)
    }
}

impl SecretKey {
    pub fn new(sk: SecpSecretKey) -> Self {
        Self(sk)
    }

    pub fn raw(&self) -> &SecpSecretKey {
        &self.0
    }
}

impl Signature {
    pub fn to_bytes(&self) -> Vec<u8> {
        let (recv, fixed) = self.0.serialize_compact();
        let id = match recv {
            RecoveryId::Zero => 0u8,
            RecoveryId::One => 1u8,
            RecoveryId::Two => 2u8,
            RecoveryId::Three => 3u8,
        };
        let mut bytes = fixed.to_vec();
        bytes.push(id + 27); // Compatible with eth
        bytes
    }

    pub fn from_bytes(bytes: &[u8]) -> std::io::Result<Signature> {
        let bytes_len = bytes.len();
        if bytes_len != SIGNATURE_LENGTH {
            return Err(new_io_error("Invalid signature length"));
        }

        let id = match bytes[64] {
            // Case 0: raw/bare
            v @ 0..=26 => v % 4,
            // Case 2: non-eip155 v value
            v @ 27..=34 => (v - 27) % 4,
            // Case 3: eip155 V value
            v @ 35.. => (v - 1) % 2,
        };

        let recv = RecoveryId::try_from(id as i32).map_err(|_| new_io_error("Invalid signature value"))?;
        RecoverableSignature::from_compact(&bytes[..64], recv)
            .map(Signature)
            .map_err(|_| new_io_error("Invalid signature value"))
    }

    pub fn peer_id(&self, msg: &[u8]) -> std::io::Result<PeerId> {
        let mut hasher = Keccak256::new();
        hasher.update(msg);
        let result = hasher.finalize();
        let msg = SecpMessage::from_digest(result.into());

        let secp = Secp256k1::new();
        let pk = secp
            .recover_ecdsa(&msg, &self.0)
            .map_err(|_| new_io_error("Invalid signature"))?;
        Ok(PublicKey(pk).peer_id())
    }

    pub fn peer_id_eth(self, message: &[u8]) -> std::io::Result<PeerId> {
        const PREFIX: &str = "\x19Ethereum Signed Message:\n";

        let len = message.len();
        let len_string = len.to_string();

        let mut eth_message = Vec::with_capacity(PREFIX.len() + len_string.len() + len);
        eth_message.extend_from_slice(PREFIX.as_bytes());
        eth_message.extend_from_slice(len_string.as_bytes());
        eth_message.extend_from_slice(message);

        self.peer_id(&eth_message)
    }
}

impl TryFrom<&str> for PublicKey {
    type Error = std::io::Error;

    fn try_from(s: &str) -> Result<Self, Self::Error> {
        let bytes = hex::decode(s.trim_start_matches("0x")).map_err(|_| new_io_error("Invalid public key hex"))?;
        if bytes.len() != PUBLIC_KEY_LENGTH {
            return Err(new_io_error("Invalid public key length"));
        }
        Ok(PublicKey(
            SecpPublicKey::from_slice(&bytes)
                .map_err(|_| new_io_error("Invalid public key value"))?,
        ))
    }
}

impl ToString for PublicKey {
    fn to_string(&self) -> String {
        format!("0x{}", hex::encode(self.0.serialize()))
    }
}

impl TryFrom<&str> for SecretKey {
    type Error = std::io::Error;

    fn try_from(s: &str) -> Result<Self, Self::Error> {
        let bytes = hex::decode(s.trim_start_matches("0x")).map_err(|_| new_io_error("Invalid secret key hex"))?;
        if bytes.len() != SECRET_KEY_LENGTH {
            return Err(new_io_error("Invalid secret key length"));
        }
        Ok(SecretKey(
            SecpSecretKey::from_slice(&bytes)
                .map_err(|_| new_io_error("Invalid secret key value"))?,
        ))
    }
}

impl ToString for SecretKey {
    fn to_string(&self) -> String {
        format!("0x{}", hex::encode(self.0.secret_bytes()))
    }
}

impl TryFrom<&str> for Signature {
    type Error = std::io::Error;

    fn try_from(s: &str) -> Result<Self, Self::Error> {
        let bytes = hex::decode(s.trim_start_matches("0x")).map_err(|_| new_io_error("Invalid secret key hex"))?;
        if bytes.len() != SIGNATURE_LENGTH {
            return Err(new_io_error("Invalid secret key length"));
        }
        Signature::from_bytes(&bytes)
    }
}

impl ToString for Signature {
    fn to_string(&self) -> String {
        format!("0x{}", hex::encode(self.to_bytes()))
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    const SK_HEX: &str = "0xac0974bec39a17e36ba4a6b4d238ff944bacb478cbed5efcae784d7bf4f2ff80";
    const PEER_ID_HEX: &str = "0xf39Fd6e51aad88F6F4ce6aB8827279cffFb92266";
    const MESSAGE: &str = "thisismessage";
    const SIGN_HEX: &str = "0xff68207a1c9ea9446d77e727d230ee8e591df27d7eac17807b5231a57a44ec46213a2cb25520c51ff0b6de68914d9828ab5ee762014e92866e9e1cc8fdfe25721b";

    #[test]
    fn test_key() {
        let peer_id1 = PeerId::from_hex(PEER_ID_HEX).unwrap().to_bytes();
        let sk = SecretKey::try_from(SK_HEX).unwrap();
        assert_eq!(sk.to_string().as_str(), SK_HEX);
        let key = Key::from_sec_key(sk);
        let peer_id = key.peer_id().to_bytes();
        assert_eq!(peer_id1, peer_id);
    }

    #[test]
    fn test_signature() {
        let peer_id = PeerId::from_hex(PEER_ID_HEX).unwrap();
        let key = Key::from_sec_key(SecretKey::try_from(SK_HEX).unwrap());
        let sign = key.sign_eth(MESSAGE.as_bytes());
        let sign_bytes = sign.to_bytes();
        let sign2 = Signature::from_bytes(&sign_bytes).unwrap();
        let peer_id2 = sign2.peer_id_eth(MESSAGE.as_bytes()).unwrap();
        assert_eq!(peer_id, peer_id2);

        let sign3 = Signature::try_from(SIGN_HEX).unwrap();
        assert_eq!(sign3.to_string().as_str(), SIGN_HEX);
        let peer_id3 = sign3.peer_id_eth(MESSAGE.as_bytes()).unwrap();
        assert_eq!(peer_id, peer_id3);
    }
}
