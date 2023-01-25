use super::methods::{MetaData, Ping, StatusMessage};
use libp2p::{core::UpgradeInfo, request_response::ProtocolName};
use ssz::Encode;
use std::io;

pub const MAX_RPC_SIZE_POST_MERGE: usize = 10 * 1_048_576; // 10 * 2**20

pub const PROTOCOL_PREFIX: &str = "/eth2/beacon_chain/req";

pub const PROTOCOL_SUFFIX: &str = "2/ssz_snappy";

// Req/Resp Domain protocols
#[repr(u8)]
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Protocol {
    Status,
    Goodbye,
    BlocksByRange,
    BlocksByRoot,
    Ping,
    MetaData,
}

impl std::fmt::Display for Protocol {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        let repr = match self {
            Protocol::Status => "status",
            Protocol::Goodbye => "goodbye",
            Protocol::BlocksByRange => "beacon_blocks_by_range",
            Protocol::BlocksByRoot => "beacon_blocks_by_root",
            Protocol::Ping => "ping",
            Protocol::MetaData => "metadata",
        };
        f.write_str(repr)
    }
}

/// Tracks the types in a protocol id.
#[derive(Clone, Debug)]
pub struct ProtocolId {
    pub message_name: Protocol,
    protocol_id: String,
}

impl ProtocolId {
    pub fn rpc_request_limits(&self) -> RpcLimits {
        match self.message_name {
            // BlocksByRange, BlocksByRoot, Goodbye not impl
            Protocol::Status => RpcLimits::new(
                <StatusMessage as Encode>::ssz_fixed_len(),
                <StatusMessage as Encode>::ssz_fixed_len(),
            ),
            Protocol::Ping => RpcLimits::new(
                <Ping as Encode>::ssz_fixed_len(),
                <Ping as Encode>::ssz_fixed_len(),
            ),
            _ => RpcLimits::new(0, 0), // Metadata requests are empty
        }
    }

    pub fn rpc_response_limits(&self) -> RpcLimits {
        match self.message_name {
            // BlocksByRange, BlocksByRoot not impl
            // Goodbye has no response
            Protocol::Status => RpcLimits::new(
                <StatusMessage as Encode>::ssz_fixed_len(),
                <StatusMessage as Encode>::ssz_fixed_len(),
            ),
            Protocol::Ping => RpcLimits::new(
                <Ping as Encode>::ssz_fixed_len(),
                <Ping as Encode>::ssz_fixed_len(),
            ),
            Protocol::MetaData => RpcLimits::new(0, <MetaData as Encode>::ssz_fixed_len()),
            _ => RpcLimits::new(0, 0),
        }
    }

    pub fn has_context_bytes(&self) -> bool {
        match self.message_name {
            Protocol::BlocksByRange | Protocol::BlocksByRoot => true,
            _ => false,
        }
    }
}

impl ProtocolId {
    pub fn new(message_name: Protocol) -> Self {
        let protocol_id = format!("{}/{}/{}", PROTOCOL_PREFIX, message_name, PROTOCOL_SUFFIX);

        ProtocolId {
            message_name,
            protocol_id,
        }
    }
}

impl ProtocolName for ProtocolId {
    fn protocol_name(&self) -> &[u8] {
        self.protocol_id.as_bytes()
    }
}

// Represents the ssz length bounds for RPC messages.
#[derive(Debug, PartialEq)]
pub struct RpcLimits {
    pub min: usize,
    pub max: usize,
}

impl RpcLimits {
    pub fn new(min: usize, max: usize) -> Self {
        Self { min, max }
    }

    pub fn is_out_of_bounds(&self, length: usize, max_rpc_size: usize) -> bool {
        length > std::cmp::min(self.max, max_rpc_size) || length < self.min
    }
}

pub enum RPCError {
    Error,
}

impl From<tokio::time::error::Elapsed> for RPCError {
    fn from(_: tokio::time::error::Elapsed) -> Self {
        RPCError::Error
    }
}

impl From<io::Error> for RPCError {
    fn from(_err: io::Error) -> Self {
        RPCError::Error
    }
}

// Protocol
pub struct RPCProtocol {}

impl UpgradeInfo for RPCProtocol {
    type Info = ProtocolId;
    type InfoIter = Vec<Self::Info>;

    /// The list of supported RPC protocols for Lighthouse.
    fn protocol_info(&self) -> Self::InfoIter {
        vec![
            ProtocolId::new(Protocol::Status),
            ProtocolId::new(Protocol::Goodbye),
            ProtocolId::new(Protocol::BlocksByRange),
            ProtocolId::new(Protocol::BlocksByRoot),
            ProtocolId::new(Protocol::Ping),
            ProtocolId::new(Protocol::MetaData),
        ]
    }
}

#[derive(Debug, Clone, PartialEq)]
pub enum InboundRequest {
    Status(StatusMessage),
    Goodbye(u64),
    Ping(Ping),
    MetaData,
}
