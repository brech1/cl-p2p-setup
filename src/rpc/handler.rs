use super::{
    codec::{InboundCodec, OutboundCodec},
    methods::RPCCodedResponse,
    outbound::OutboundRequest,
    protocol::{Protocol, RPCError, RPCProtocol},
    RPCReceived,
};
use fnv::FnvHashMap;
use futures::Future;
use libp2p::swarm::{NegotiatedSubstream, SubstreamProtocol};
use smallvec::SmallVec;
use std::{collections::VecDeque, pin::Pin, time::Instant};
use tokio_io_timeout::TimeoutStream;
use tokio_util::time::{delay_queue, DelayQueue};
use tokio_util::{codec::Framed, compat::Compat};

pub type HandlerEvent<Id> = Result<RPCReceived<Id>, &'static str>;

#[derive(Debug, Clone, Copy, Hash, Eq, PartialEq)]
pub struct SubstreamId(usize);

// Inbound substream info.
pub type InboundFramed<TSocket> =
    Framed<std::pin::Pin<Box<TimeoutStream<Compat<TSocket>>>>, InboundCodec>;

type InboundSubstream = InboundFramed<NegotiatedSubstream>;

// Outbound substream info.
pub type OutboundFramed<TSocket> = Framed<Compat<TSocket>, OutboundCodec>;

// RPC Handler
pub struct RPCHandler<Id> {
    listen_protocol: SubstreamProtocol<RPCProtocol, ()>,
    events_out: SmallVec<[HandlerEvent<Id>; 4]>,
    dial_queue: SmallVec<[(Id, OutboundRequest); 4]>,
    dial_negotiated: u32,
    // Inbound
    inbound_substreams: FnvHashMap<SubstreamId, InboundInfo>,
    inbound_substreams_delay: DelayQueue<SubstreamId>,
    // Outbound
    outbound_substreams: FnvHashMap<SubstreamId, OutboundInfo<Id>>,
    outbound_substreams_delay: DelayQueue<SubstreamId>,
    // Substream IDs
    current_inbound_substream_id: SubstreamId,
    current_outbound_substream_id: SubstreamId,
    // Config
    max_dial_negotiated: u32,
}

// Inbound

struct InboundInfo {
    state: InboundState,
    pending_items: VecDeque<RPCCodedResponse>,
    protocol: Protocol,
    remaining_chunks: u64,
    request_start_time: Instant,
    delay_key: Option<delay_queue::Key>,
}

enum InboundState {
    Idle(InboundSubstream),
    Busy(Pin<Box<dyn Future<Output = Result<(InboundSubstream, bool), RPCError>> + Send>>),
    Poisoned,
}

// Outbound

struct OutboundInfo<Id> {
    state: OutboundSubstreamState,
    delay_key: delay_queue::Key,
    proto: Protocol,
    remaining_chunks: Option<u64>,
    req_id: Id,
}

pub enum OutboundSubstreamState {
    RequestPendingResponse {
        substream: Box<OutboundFramed<NegotiatedSubstream>>,
        request: OutboundRequest,
    },
    Closing(Box<OutboundFramed<NegotiatedSubstream>>),
    Poisoned,
}
