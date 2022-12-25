use discv5::{
    enr::{CombinedKey, CombinedPublicKey},
    Enr,
};
use libp2p::{identity::Keypair, PeerId};

// Get CombinedKey from Secp256k1 libp2p Keypair
pub fn key_from_libp2p(key: &libp2p::core::identity::Keypair) -> Result<CombinedKey, &'static str> {
    match key {
        Keypair::Secp256k1(key) => {
            let secret = discv5::enr::k256::ecdsa::SigningKey::from_bytes(&key.secret().to_bytes())
                .expect("libp2p key must be valid");
            Ok(CombinedKey::Secp256k1(secret))
        }
        _ => Err("pair not supported"),
    }
}

pub trait EnrAsPeerId {
    /// Converts the enr into a peer id
    fn as_peer_id(&self) -> PeerId;
}

impl EnrAsPeerId for Enr {
    fn as_peer_id(&self) -> PeerId {
        let public_key = self.public_key();

        match public_key {
            CombinedPublicKey::Secp256k1(pk) => {
                let pk_bytes = pk.to_bytes();
                let libp2p_pk = libp2p::core::PublicKey::Secp256k1(
                    libp2p::core::identity::secp256k1::PublicKey::decode(&pk_bytes)
                        .expect("valid public key"),
                );
                PeerId::from_public_key(&libp2p_pk)
            }
            CombinedPublicKey::Ed25519(pk) => {
                let pk_bytes = pk.to_bytes();
                let libp2p_pk = libp2p::core::PublicKey::Ed25519(
                    libp2p::core::identity::ed25519::PublicKey::decode(&pk_bytes)
                        .expect("valid public key"),
                );
                PeerId::from_public_key(&libp2p_pk)
            }
        }
    }
}
