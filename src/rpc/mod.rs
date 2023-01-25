pub(crate) mod codec;
mod handler;
pub mod methods;
mod outbound;
mod protocol;
use self::methods::{RPCCodedResponse, RPCResponse};
use self::{handler::HandlerEvent, outbound::OutboundRequest, protocol::InboundRequest};
use libp2p::core::connection::ConnectionId;
use libp2p::PeerId;

#[derive(Debug, Clone)]
pub enum RPCSend<Id> {
    Request(Id, OutboundRequest),
    Response(Id, RPCCodedResponse),
}

#[derive(Debug, Clone)]
pub enum RPCReceived<Id> {
    Request(Id, InboundRequest),
    Response(Id, RPCResponse),
}

#[derive(Debug)]
pub struct RPCMessage<Id> {
    pub peer_id: PeerId,
    pub conn_id: ConnectionId,
    pub event: HandlerEvent<Id>,
}
