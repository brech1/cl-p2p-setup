use crate::config::{BELLATRIX_FORK_VERSION, GENESIS_VALIDATORS_ROOT};
use serde_derive::{Deserialize, Serialize};
use ssz_derive::{Decode, Encode};
use tree_hash::{Hash256, TreeHash};
use tree_hash_derive::TreeHash;

#[derive(Debug, Clone, PartialEq, Default, Serialize, Deserialize, Encode, Decode, TreeHash)]
pub struct ForkId {
    pub fork_digest: [u8; 4],
    pub next_fork_version: [u8; 4],
    pub next_fork_epoch: u64,
}

impl ForkId {
    pub fn new() -> Self {
        Self {
            fork_digest: compute_fork_digest(
                BELLATRIX_FORK_VERSION,
                GENESIS_VALIDATORS_ROOT.into(),
            ),
            next_fork_version: BELLATRIX_FORK_VERSION,
            next_fork_epoch: u64::max_value(),
        }
    }
}

#[derive(Debug, PartialEq, Clone, Serialize, Deserialize, Encode, Decode, TreeHash)]
pub struct SigningData {
    pub object_root: Hash256,
    pub domain: Hash256,
}

pub trait SignedRoot: TreeHash {
    fn signing_root(&self, domain: Hash256) -> Hash256 {
        SigningData {
            object_root: self.tree_hash_root(),
            domain,
        }
        .tree_hash_root()
        .into()
    }
}

#[derive(Debug, Clone, PartialEq, Default, Serialize, Deserialize, Encode, Decode, TreeHash)]
pub struct ForkData {
    pub current_version: [u8; 4],
    pub genesis_validators_root: Hash256,
}

impl SignedRoot for ForkData {}

// https://eth2book.info/bellatrix/part3/helper/misc/#compute_fork_digest
pub fn compute_fork_digest(current_version: [u8; 4], genesis_validators_root: Hash256) -> [u8; 4] {
    let mut result = [0; 4];
    let root = compute_fork_data_root(current_version, genesis_validators_root);
    result.copy_from_slice(
        root.as_bytes()
            .get(0..4)
            .expect("root hash is at least 4 bytes"),
    );
    result
}

// https://eth2book.info/bellatrix/part3/helper/misc/#compute_fork_data_root
pub fn compute_fork_data_root(
    current_version: [u8; 4],
    genesis_validators_root: Hash256,
) -> Hash256 {
    ForkData {
        current_version,
        genesis_validators_root,
    }
    .tree_hash_root()
}
