pub(crate) mod codec;
mod handler;
pub mod methods;
pub mod protocol;
use self::handler::{RPCHandler, SubstreamId};
use self::methods::{RPCCodedResponse, RPCResponse, StatusMessage};
use self::protocol::{OutboundRequest, RPCProtocol};
use self::{handler::HandlerEvent, protocol::InboundRequest};
use libp2p::core::connection::ConnectionId;
use libp2p::swarm::{
    ConnectionHandler, NetworkBehaviour, NetworkBehaviourAction, NotifyHandler, PollParameters,
    SubstreamProtocol,
};
use libp2p::PeerId;
use std::task::{Context, Poll};

pub trait ReqId: Send + 'static + std::fmt::Debug + Copy + Clone {}
impl<T> ReqId for T where T: Send + 'static + std::fmt::Debug + Copy + Clone {}

#[allow(dead_code)]
#[derive(Debug, Clone)]
pub enum RPCSend<Id> {
    Request(Id, OutboundRequest),
    Response(SubstreamId, RPCCodedResponse),
}

#[derive(Debug, Clone)]
pub enum RPCReceived<Id> {
    Request(SubstreamId, InboundRequest),
    Response(Id, RPCResponse),
}

#[derive(Debug)]
pub struct RPCMessage<Id> {
    pub peer_id: PeerId,
    pub conn_id: ConnectionId,
    pub event: HandlerEvent<Id>,
}

pub struct RPC<Id: ReqId> {
    events: Vec<NetworkBehaviourAction<RPCMessage<Id>, RPCHandler<Id>>>,
}

impl<Id: ReqId> RPC<Id> {
    pub fn new() -> Self {
        RPC { events: Vec::new() }
    }

    pub fn send_response(
        &mut self,
        peer_id: PeerId,
        id: (ConnectionId, SubstreamId),
        event: RPCCodedResponse,
    ) {
        self.events.push(NetworkBehaviourAction::NotifyHandler {
            peer_id,
            handler: NotifyHandler::One(id.0),
            event: RPCSend::Response(id.1, event),
        });
    }

    pub fn _send_request(&mut self, peer_id: PeerId, request_id: Id, event: OutboundRequest) {
        self.events.push(NetworkBehaviourAction::NotifyHandler {
            peer_id,
            handler: NotifyHandler::Any,
            event: RPCSend::Request(request_id, event),
        });
    }
}

impl<Id> NetworkBehaviour for RPC<Id>
where
    Id: ReqId,
{
    type ConnectionHandler = RPCHandler<Id>;
    type OutEvent = RPCMessage<Id>;

    fn new_handler(&mut self) -> Self::ConnectionHandler {
        RPCHandler::new(SubstreamProtocol::new(RPCProtocol {}, ()))
    }

    fn inject_event(
        &mut self,
        peer_id: PeerId,
        conn_id: ConnectionId,
        event: <Self::ConnectionHandler as ConnectionHandler>::OutEvent,
    ) {
        self.events
            .push(NetworkBehaviourAction::GenerateEvent(RPCMessage {
                peer_id,
                conn_id,
                event,
            }))
    }

    fn poll(
        &mut self,
        _cx: &mut Context,
        _: &mut impl PollParameters,
    ) -> Poll<NetworkBehaviourAction<Self::OutEvent, Self::ConnectionHandler>> {
        if !self.events.is_empty() {
            return Poll::Ready(self.events.remove(0));
        }
        Poll::Pending
    }
}

// API

#[allow(dead_code)]
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum RequestId<AppReqId> {
    Application(AppReqId),
    Internal,
}

#[allow(dead_code)]
#[derive(PartialEq)]
pub enum Request {
    Status(StatusMessage),
}

impl std::convert::From<Request> for OutboundRequest {
    fn from(req: Request) -> OutboundRequest {
        match req {
            Request::Status(s) => OutboundRequest::Status(s),
        }
    }
}

#[allow(dead_code)]
#[derive(PartialEq)]
pub enum Response {
    Status(StatusMessage),
}

impl std::convert::From<Response> for RPCCodedResponse {
    fn from(resp: Response) -> RPCCodedResponse {
        match resp {
            Response::Status(s) => RPCCodedResponse::Success(RPCResponse::Status(s)),
        }
    }
}
