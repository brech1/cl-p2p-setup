use discv5::{
    enr::{self, CombinedKey, CombinedPublicKey},
    Enr,
};
use libp2p::PeerId;

// Implement consensus specs enr structure
// See: https://github.com/ethereum/consensus-specs/blob/master/specs/phase0/p2p-interface.md#enr-structure

// eth2 field key
pub const ETH2_ENR_KEY: &str = "eth2";

pub fn build_enr(combined_key: &CombinedKey) -> Enr {
    let mut enr_builder = enr::EnrBuilder::new("v4");

    enr_builder.ip("0.0.0.0".parse().unwrap());

    enr_builder.udp4(9000);

    enr_builder.tcp4(9000);

    // Build and add eth2 field

    // enr_builder.add_value(ETH2_ENR_KEY, eth2_field);

    enr_builder.build(combined_key).unwrap()
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
