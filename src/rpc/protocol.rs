use super::{
    codec::{
        base::{BaseInboundCodec, BaseOutboundCodec},
        ssz_snappy::{SSZSnappyInboundCodec, SSZSnappyOutboundCodec},
        InboundCodec, OutboundCodec,
    },
    methods::{MetaData, Ping, StatusMessage},
};
use futures::{
    future::BoxFuture,
    prelude::{AsyncRead, AsyncWrite},
    FutureExt, SinkExt, StreamExt,
};
use libp2p::{
    core::UpgradeInfo, request_response::ProtocolName, swarm::NegotiatedSubstream, InboundUpgrade,
    OutboundUpgrade,
};
use serde::ser::StdError;
use ssz::Encode;
use std::{fmt::Display, io, time::Duration};
use tokio_io_timeout::TimeoutStream;
use tokio_util::{
    codec::Framed,
    compat::{Compat, FuturesAsyncReadCompatExt},
};

pub const MAX_RPC_SIZE_POST_MERGE: usize = 10 * 1_048_576; // 10 * 2**20

pub const PROTOCOL_PREFIX: &str = "/eth2/beacon_chain/req";

pub const ENCODING: &str = "ssz_snappy";

const TTFB_TIMEOUT: u64 = 5;

const REQUEST_TIMEOUT: u64 = 15;

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
    LightClientBootstrap,
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
            Protocol::LightClientBootstrap => "light_client_bootstrap",
        };
        f.write_str(repr)
    }
}

// Protocol version
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Version {
    V1,
    V2,
}

impl std::fmt::Display for Version {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        let repr = match self {
            Version::V1 => "1",
            Version::V2 => "2",
        };
        f.write_str(repr)
    }
}

#[derive(Clone, Debug)]
pub struct ProtocolId {
    pub message_name: Protocol,
    pub version: Version,
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
            _ => RpcLimits::new(0, 0),
        }
    }

    pub fn rpc_response_limits(&self) -> RpcLimits {
        match self.message_name {
            Protocol::Status => RpcLimits::new(
                <StatusMessage as Encode>::ssz_fixed_len(),
                <StatusMessage as Encode>::ssz_fixed_len(),
            ),
            Protocol::Ping => RpcLimits::new(
                <Ping as Encode>::ssz_fixed_len(),
                <Ping as Encode>::ssz_fixed_len(),
            ),
            Protocol::MetaData => RpcLimits::new(
                <MetaData as Encode>::ssz_fixed_len(),
                <MetaData as Encode>::ssz_fixed_len(),
            ),
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
    pub fn new(message_name: Protocol, version: Version) -> Self {
        let protocol_id = format!(
            "{}/{}/{}/{}",
            PROTOCOL_PREFIX, message_name, version, ENCODING
        );

        ProtocolId {
            message_name,
            version,
            protocol_id,
        }
    }
}

impl ProtocolName for ProtocolId {
    fn protocol_name(&self) -> &[u8] {
        self.protocol_id.as_bytes()
    }
}

// SSZ length bounds for RPC messages.

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

// Protocol

#[derive(Debug, Clone, PartialEq)]
pub struct RPCProtocol {}

impl UpgradeInfo for RPCProtocol {
    type Info = ProtocolId;
    type InfoIter = Vec<Self::Info>;

    fn protocol_info(&self) -> Self::InfoIter {
        vec![
            ProtocolId::new(Protocol::Status, Version::V1),
            ProtocolId::new(Protocol::Goodbye, Version::V1),
            ProtocolId::new(Protocol::Ping, Version::V1),
            ProtocolId::new(Protocol::MetaData, Version::V2),
        ]
    }
}

// Inbound

#[derive(Debug, Clone, PartialEq)]
pub enum InboundRequest {
    Status(StatusMessage),
    Goodbye(u64),
    Ping(Ping),
    MetaData,
}

impl InboundRequest {
    pub fn expected_responses(&self) -> u64 {
        match self {
            InboundRequest::Status(_) => 1,
            InboundRequest::Goodbye(_) => 0,
            InboundRequest::Ping(_) => 1,
            InboundRequest::MetaData => 1,
        }
    }

    pub fn protocol(&self) -> Protocol {
        match self {
            InboundRequest::Status(_) => Protocol::Status,
            InboundRequest::Goodbye(_) => Protocol::Goodbye,
            InboundRequest::Ping(_) => Protocol::Ping,
            InboundRequest::MetaData => Protocol::MetaData,
        }
    }
}

pub type InboundFramed<TSocket> =
    Framed<std::pin::Pin<Box<TimeoutStream<Compat<TSocket>>>>, InboundCodec>;

pub type InboundOutput<TSocket> = (InboundRequest, InboundFramed<TSocket>);

pub type InboundSubstream = InboundFramed<NegotiatedSubstream>;

impl<TSocket> InboundUpgrade<TSocket> for RPCProtocol
where
    TSocket: AsyncRead + AsyncWrite + Unpin + Send + 'static,
{
    type Output = InboundOutput<TSocket>;
    type Error = RPCError;
    type Future = BoxFuture<'static, Result<Self::Output, Self::Error>>;

    fn upgrade_inbound(self, socket: TSocket, protocol: ProtocolId) -> Self::Future {
        async move {
            let protocol_name = protocol.message_name;

            let socket = socket.compat();

            let codec = {
                let ssz_snappy_codec = BaseInboundCodec::new(SSZSnappyInboundCodec::new(protocol));

                InboundCodec::SSZSnappy(ssz_snappy_codec)
            };

            let mut timed_socket = TimeoutStream::new(socket);
            timed_socket.set_read_timeout(Some(Duration::from_secs(TTFB_TIMEOUT)));

            let socket = Framed::new(Box::pin(timed_socket), codec);

            match protocol_name {
                Protocol::MetaData => Ok((InboundRequest::MetaData, socket)),
                _ => {
                    match tokio::time::timeout(
                        Duration::from_secs(REQUEST_TIMEOUT),
                        socket.into_future(),
                    )
                    .await
                    {
                        Err(e) => Err(RPCError::from(e)),
                        Ok((Some(Ok(request)), stream)) => Ok((request, stream)),
                        Ok((Some(Err(e)), _)) => Err(e),
                        Ok((None, _)) => Err(RPCError::Error),
                    }
                }
            }
        }
        .boxed()
    }
}

// Outbound

#[derive(Debug, Clone)]
pub struct OutboundRequestContainer {
    pub req: OutboundRequest,
}

impl UpgradeInfo for OutboundRequestContainer {
    type Info = ProtocolId;
    type InfoIter = Vec<Self::Info>;

    fn protocol_info(&self) -> Self::InfoIter {
        self.req.supported_protocols()
    }
}

#[derive(Debug, Clone, PartialEq)]
pub enum OutboundRequest {
    Status(StatusMessage),
    Goodbye(u64),
    Ping(Ping),
    MetaData,
}

impl OutboundRequest {
    pub fn supported_protocols(&self) -> Vec<ProtocolId> {
        match self {
            OutboundRequest::Status(_) => vec![ProtocolId::new(Protocol::Status, Version::V1)],
            OutboundRequest::Goodbye(_) => vec![ProtocolId::new(Protocol::Goodbye, Version::V1)],
            OutboundRequest::Ping(_) => vec![ProtocolId::new(Protocol::Ping, Version::V1)],
            OutboundRequest::MetaData => vec![ProtocolId::new(Protocol::MetaData, Version::V2)],
        }
    }

    pub fn expected_responses(&self) -> u64 {
        match self {
            OutboundRequest::Status(_) => 1,
            OutboundRequest::Goodbye(_) => 0,
            OutboundRequest::Ping(_) => 1,
            OutboundRequest::MetaData => 1,
        }
    }

    pub fn protocol(&self) -> Protocol {
        match self {
            OutboundRequest::Status(_) => Protocol::Status,
            OutboundRequest::Goodbye(_) => Protocol::Goodbye,
            OutboundRequest::Ping(_) => Protocol::Ping,
            OutboundRequest::MetaData => Protocol::MetaData,
        }
    }
}

pub type OutboundFramed<TSocket> = Framed<Compat<TSocket>, OutboundCodec>;

impl<TSocket> OutboundUpgrade<TSocket> for OutboundRequestContainer
where
    TSocket: AsyncRead + AsyncWrite + Unpin + Send + 'static,
{
    type Output = OutboundFramed<TSocket>;
    type Error = RPCError;
    type Future = BoxFuture<'static, Result<Self::Output, Self::Error>>;

    fn upgrade_outbound(self, socket: TSocket, protocol: Self::Info) -> Self::Future {
        let socket = socket.compat();

        let codec = {
            let ssz_snappy_codec = BaseOutboundCodec::new(SSZSnappyOutboundCodec::new(protocol));

            OutboundCodec::SSZSnappy(ssz_snappy_codec)
        };

        let mut socket = Framed::new(socket, codec);

        async {
            socket.send(self.req).await?;
            socket.close().await?;
            Ok(socket)
        }
        .boxed()
    }
}

// RPC Error

#[derive(Debug, Clone, PartialEq)]
pub enum RPCError {
    Error,
}

impl StdError for RPCError {
    fn source(&self) -> Option<&(dyn StdError + 'static)> {
        None
    }

    fn description(&self) -> &str {
        "description() is deprecated; use Display"
    }

    fn cause(&self) -> Option<&dyn StdError> {
        self.source()
    }
}

impl Display for RPCError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            RPCError::Error => write!(f, "RPC Error"),
        }
    }
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
