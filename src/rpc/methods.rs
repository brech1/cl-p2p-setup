use serde::Serialize;
use ssz_derive::{Decode, Encode};
use ssz_types::{
    typenum::{U256, U4, U64},
    BitVector, VariableList,
};
use tree_hash::Hash256;

pub type MaxErrorLen = U256;

#[derive(Debug, Clone)]
pub struct ErrorType(pub VariableList<u8, MaxErrorLen>);

pub type SubnetBitfieldLength = U64;
pub type SyncCommitteeSubnetCount = U4;

pub type EnrAttestationBitfield = BitVector<SubnetBitfieldLength>;
pub type EnrSyncCommitteeBitfield = BitVector<SyncCommitteeSubnetCount>;

// https://github.com/ethereum/consensus-specs/blob/dev/specs/phase0/p2p-interface.md#messages

#[derive(Encode, Decode, Clone, Debug, PartialEq)]
pub struct StatusMessage {
    pub fork_digest: [u8; 4],
    pub finalized_root: Hash256,
    pub finalized_epoch: u64,
    pub head_root: Hash256,
    pub head_slot: u64,
}

#[derive(Encode, Decode, Clone, Debug, PartialEq)]
pub struct Ping {
    pub data: u64,
}

#[derive(Clone, Debug, PartialEq, Serialize, Encode, Decode)]
pub struct MetaData {
    pub seq_number: u64,
    pub attnets: EnrAttestationBitfield,
    pub syncnets: EnrSyncCommitteeBitfield,
}

// Response

#[derive(Debug, Clone, PartialEq)]
pub enum RPCCodedResponse {
    Success(RPCResponse),
    Error,
}

impl RPCCodedResponse {
    pub fn as_u8(&self) -> Option<u8> {
        match self {
            RPCCodedResponse::Success(_) => Some(0),
            RPCCodedResponse::Error => Some(4),
        }
    }

    pub fn is_response(response_code: u8) -> bool {
        matches!(response_code, 0)
    }

    pub fn close_after(&self) -> bool {
        !matches!(self, RPCCodedResponse::Success(_))
    }
}

#[derive(Debug, Clone, PartialEq)]
pub enum RPCResponse {
    Status(StatusMessage),
    Pong(Ping),
    MetaData(MetaData),
}
