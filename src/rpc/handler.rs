use super::protocol::{
    InboundRequest, InboundSubstream, OutboundFramed, OutboundRequest, OutboundRequestContainer,
};
use super::{
    methods::RPCCodedResponse,
    protocol::{Protocol, RPCError, RPCProtocol},
    RPCReceived, RPCSend, ReqId,
};
use fnv::FnvHashMap;
use futures::{Future, FutureExt, Sink, SinkExt, StreamExt};
use libp2p::core::UpgradeError;
use libp2p::swarm::{
    ConnectionHandler, ConnectionHandlerEvent, ConnectionHandlerUpgrErr, KeepAlive,
    NegotiatedSubstream, SubstreamProtocol,
};
use libp2p::{InboundUpgrade, OutboundUpgrade};
use smallvec::SmallVec;
use std::collections::hash_map::Entry;
use std::task::Poll;
use std::{
    collections::VecDeque,
    pin::Pin,
    time::{Duration, Instant},
};
use tokio::time::{sleep_until, Instant as TInstant, Sleep};
use tokio_util::time::{delay_queue, DelayQueue};

const SHUTDOWN_TIMEOUT_SECS: u8 = 15;
const MAX_INBOUND_SUBSTREAMS: usize = 32;
const RESPONSE_TIMEOUT: u64 = 10;
const IO_ERROR_RETRIES: u8 = 3;

pub type HandlerEvent<Id> = Result<RPCReceived<Id>, &'static str>;

#[derive(Debug, Clone, Copy, Hash, Eq, PartialEq)]
pub struct SubstreamId(usize);

// Inbound

#[allow(dead_code)]
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

enum HandlerState {
    Active,
    ShuttingDown(Pin<Box<Sleep>>),
    Deactivated,
}

// RPC Handler
pub struct RPCHandler<Id> {
    listen_protocol: SubstreamProtocol<RPCProtocol, ()>,
    events_out: SmallVec<[HandlerEvent<Id>; 4]>,
    // Dialing
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
    // State
    state: HandlerState,
    outbound_io_error_retries: u8,
    waker: Option<std::task::Waker>,
}

impl<Id> RPCHandler<Id> {
    pub fn new(listen_protocol: SubstreamProtocol<RPCProtocol, ()>) -> Self {
        Self {
            listen_protocol,
            events_out: SmallVec::new(),
            dial_queue: SmallVec::new(),
            dial_negotiated: 0,
            inbound_substreams: FnvHashMap::default(),
            inbound_substreams_delay: DelayQueue::new(),
            outbound_substreams: FnvHashMap::default(),
            outbound_substreams_delay: DelayQueue::new(),
            current_inbound_substream_id: SubstreamId(0),
            current_outbound_substream_id: SubstreamId(0),
            max_dial_negotiated: 8,
            state: HandlerState::Active,
            outbound_io_error_retries: 0,
            waker: None,
        }
    }

    fn shutdown(&mut self, goodbye_reason: Option<Id>) {
        if matches!(self.state, HandlerState::Active) {
            while let Some((_, _)) = self.dial_queue.pop() {
                self.events_out.push(Err("handler error"));
            }

            if let Some(id) = goodbye_reason {
                self.dial_queue.push((id, OutboundRequest::Goodbye(1)));
            }

            self.state = HandlerState::ShuttingDown(Box::pin(sleep_until(
                TInstant::now() + Duration::from_secs(SHUTDOWN_TIMEOUT_SECS as u64),
            )));
        }
    }

    fn send_request(&mut self, id: Id, req: OutboundRequest) {
        match self.state {
            HandlerState::Active => {
                self.dial_queue.push((id, req));
            }
            _ => self.events_out.push(Err("handler error")),
        }
    }

    fn send_response(&mut self, inbound_id: SubstreamId, response: RPCCodedResponse) {
        let inbound_info = if let Some(info) = self.inbound_substreams.get_mut(&inbound_id) {
            info
        } else {
            return;
        };

        if let RPCCodedResponse::Error = response {
            self.events_out.push(Err("handler error"));
        }

        if matches!(self.state, HandlerState::Deactivated) {
            return;
        }

        inbound_info.pending_items.push_back(response);
    }
}

impl<Id> ConnectionHandler for RPCHandler<Id>
where
    Id: ReqId,
{
    type InEvent = RPCSend<Id>;
    type OutEvent = HandlerEvent<Id>;
    type Error = RPCError;
    type InboundProtocol = RPCProtocol;
    type OutboundProtocol = OutboundRequestContainer;
    type InboundOpenInfo = ();
    type OutboundOpenInfo = (Id, OutboundRequest);

    fn listen_protocol(&self) -> SubstreamProtocol<Self::InboundProtocol, Self::InboundOpenInfo> {
        self.listen_protocol.clone()
    }

    fn connection_keep_alive(&self) -> KeepAlive {
        let should_shutdown = match self.state {
            HandlerState::ShuttingDown(_) => {
                self.dial_queue.is_empty()
                    && self.outbound_substreams.is_empty()
                    && self.inbound_substreams.is_empty()
                    && self.events_out.is_empty()
                    && self.dial_negotiated == 0
            }
            HandlerState::Deactivated => true,
            _ => false,
        };

        if should_shutdown {
            KeepAlive::No
        } else {
            KeepAlive::Yes
        }
    }

    fn inject_fully_negotiated_inbound(
        &mut self,
        substream: <Self::InboundProtocol as InboundUpgrade<NegotiatedSubstream>>::Output,
        _info: Self::InboundOpenInfo,
    ) {
        if !matches!(self.state, HandlerState::Active) {
            return;
        }

        let (req, substream) = substream;
        let expected_responses = req.expected_responses();

        if expected_responses > 0 {
            if self.inbound_substreams.len() < MAX_INBOUND_SUBSTREAMS {
                let delay_key = self.inbound_substreams_delay.insert(
                    self.current_inbound_substream_id,
                    Duration::from_secs(RESPONSE_TIMEOUT),
                );
                let awaiting_stream = InboundState::Idle(substream);
                self.inbound_substreams.insert(
                    self.current_inbound_substream_id,
                    InboundInfo {
                        state: awaiting_stream,
                        pending_items: VecDeque::with_capacity(std::cmp::min(
                            expected_responses,
                            128,
                        ) as usize),
                        delay_key: Some(delay_key),
                        protocol: req.protocol(),
                        request_start_time: Instant::now(),
                        remaining_chunks: expected_responses,
                    },
                );
            } else {
                self.events_out.push(Err("HandlerRejected"));
                return self.shutdown(None);
            }
        }

        if let InboundRequest::Goodbye(_) = req {
            self.shutdown(None);
        }

        self.events_out.push(Ok(RPCReceived::Request(
            self.current_inbound_substream_id,
            req,
        )));
        self.current_inbound_substream_id.0 += 1;
    }

    fn inject_fully_negotiated_outbound(
        &mut self,
        out: <Self::OutboundProtocol as OutboundUpgrade<NegotiatedSubstream>>::Output,
        request_info: Self::OutboundOpenInfo,
    ) {
        self.dial_negotiated -= 1;
        let (id, request) = request_info;
        let proto = request.protocol();

        if matches!(self.state, HandlerState::Deactivated) {
            self.events_out.push(Err("HandlerState::Deactivated"));
        }

        let expected_responses = request.expected_responses();
        if expected_responses > 0 {
            let delay_key = self.outbound_substreams_delay.insert(
                self.current_outbound_substream_id,
                Duration::from_secs(RESPONSE_TIMEOUT),
            );
            let awaiting_stream = OutboundSubstreamState::RequestPendingResponse {
                substream: Box::new(out),
                request,
            };
            let expected_responses = if expected_responses > 1 {
                Some(expected_responses)
            } else {
                None
            };

            self.outbound_substreams.insert(
                self.current_outbound_substream_id,
                OutboundInfo {
                    state: awaiting_stream,
                    delay_key,
                    proto,
                    remaining_chunks: expected_responses,
                    req_id: id,
                },
            );

            self.current_outbound_substream_id.0 += 1;
        }
    }

    fn inject_event(&mut self, rpc_event: Self::InEvent) {
        match rpc_event {
            RPCSend::Request(id, req) => self.send_request(id, req),
            RPCSend::Response(inbound_id, response) => self.send_response(inbound_id, response),
        }

        if let Some(waker) = &self.waker {
            waker.wake_by_ref();
        }
    }

    fn inject_dial_upgrade_error(
        &mut self,
        request_info: Self::OutboundOpenInfo,
        error: ConnectionHandlerUpgrErr<
            <Self::OutboundProtocol as OutboundUpgrade<NegotiatedSubstream>>::Error,
        >,
    ) {
        let (id, req) = request_info;
        if let ConnectionHandlerUpgrErr::Upgrade(UpgradeError::Apply(RPCError::Error)) = error {
            self.outbound_io_error_retries += 1;
            if self.outbound_io_error_retries < IO_ERROR_RETRIES {
                self.send_request(id, req);
                return;
            }
        }

        self.dial_negotiated -= 1;

        self.outbound_io_error_retries = 0;

        println!("Dial upgrade error: {:?}", error);

        self.events_out.push(Err("Dial upgrade error"));
    }

    fn poll(
        &mut self,
        cx: &mut std::task::Context<'_>,
    ) -> std::task::Poll<
        libp2p::swarm::ConnectionHandlerEvent<
            Self::OutboundProtocol,
            Self::OutboundOpenInfo,
            Self::OutEvent,
            Self::Error,
        >,
    > {
        if let Some(waker) = &self.waker {
            if waker.will_wake(cx.waker()) {
                self.waker = Some(cx.waker().clone());
            }
        } else {
            self.waker = Some(cx.waker().clone());
        }

        if !self.events_out.is_empty() {
            return Poll::Ready(ConnectionHandlerEvent::Custom(self.events_out.remove(0)));
        } else {
            self.events_out.shrink_to_fit();
        }

        if let HandlerState::ShuttingDown(delay) = &mut self.state {
            match delay.as_mut().poll(cx) {
                Poll::Ready(_) => {
                    self.state = HandlerState::Deactivated;
                    return Poll::Ready(ConnectionHandlerEvent::Close(RPCError::Error));
                }
                Poll::Pending => {}
            };
        }

        loop {
            match self.inbound_substreams_delay.poll_expired(cx) {
                Poll::Ready(Some(Ok(inbound_id))) => {
                    if let Some(info) = self.inbound_substreams.get_mut(inbound_id.get_ref()) {
                        info.delay_key = None;
                        self.events_out.push(Err("Stream Timeout"));

                        if info.pending_items.back().map(|l| l.close_after()) == Some(false) {
                            println!("Request timed out");
                            info.pending_items.push_back(RPCCodedResponse::Error);
                        }
                    }
                }
                Poll::Ready(Some(Err(_))) => {
                    return Poll::Ready(ConnectionHandlerEvent::Close(RPCError::Error));
                }
                Poll::Pending | Poll::Ready(None) => break,
            }
        }

        loop {
            match self.outbound_substreams_delay.poll_expired(cx) {
                Poll::Ready(Some(Ok(outbound_id))) => {
                    if let Some(OutboundInfo {
                        proto: _,
                        req_id: _,
                        ..
                    }) = self.outbound_substreams.remove(outbound_id.get_ref())
                    {
                        let outbound_err = "Stream Timeout";

                        return Poll::Ready(ConnectionHandlerEvent::Custom(Err(outbound_err)));
                    } else {
                        println!("timed out substream not in the books");
                    }
                }
                Poll::Ready(Some(Err(_))) => {
                    println!("Outbound substream poll failed");
                    return Poll::Ready(ConnectionHandlerEvent::Close(RPCError::Error));
                }
                Poll::Pending | Poll::Ready(None) => break,
            }
        }

        let deactivated = matches!(self.state, HandlerState::Deactivated);

        let mut substreams_to_remove = Vec::new();
        for (id, info) in self.inbound_substreams.iter_mut() {
            loop {
                match std::mem::replace(&mut info.state, InboundState::Poisoned) {
                    InboundState::Idle(substream) if !deactivated => {
                        if let Some(message) = info.pending_items.pop_front() {
                            let last_chunk = info.remaining_chunks <= 1;
                            let fut =
                                send_message_to_inbound_substream(substream, message, last_chunk)
                                    .boxed();
                            info.state = InboundState::Busy(Box::pin(fut));
                        } else {
                            info.state = InboundState::Idle(substream);
                            break;
                        }
                    }
                    InboundState::Idle(mut substream) => {
                        match substream.close().poll_unpin(cx) {
                            Poll::Pending => info.state = InboundState::Idle(substream),
                            Poll::Ready(res) => {
                                substreams_to_remove.push(*id);
                                if let Some(ref delay_key) = info.delay_key {
                                    self.inbound_substreams_delay.remove(delay_key);
                                }
                                if let Err(_) = res {
                                    self.events_out
                                        .push(Err("Error shutting down the substream"));
                                }
                                if info.pending_items.back().map(|l| l.close_after()) == Some(false)
                                {
                                    self.events_out.push(Err("Cancel Request"));
                                }
                            }
                        }
                        break;
                    }
                    InboundState::Busy(mut fut) => {
                        match fut.poll_unpin(cx) {
                            Poll::Ready(Ok((substream, substream_was_closed)))
                                if !substream_was_closed =>
                            {
                                info.remaining_chunks = info.remaining_chunks.saturating_sub(1);

                                if let Some(ref delay_key) = info.delay_key {
                                    self.inbound_substreams_delay
                                        .reset(delay_key, Duration::from_secs(RESPONSE_TIMEOUT));
                                }

                                if !deactivated && !info.pending_items.is_empty() {
                                    if let Some(message) = info.pending_items.pop_front() {
                                        let last_chunk = info.remaining_chunks <= 1;
                                        let fut = send_message_to_inbound_substream(
                                            substream, message, last_chunk,
                                        )
                                        .boxed();
                                        info.state = InboundState::Busy(Box::pin(fut));
                                    }
                                } else {
                                    info.state = InboundState::Idle(substream);
                                    break;
                                }
                            }
                            Poll::Ready(Ok((_substream, _substream_was_closed))) => {
                                substreams_to_remove.push(*id);
                                if let Some(ref delay_key) = info.delay_key {
                                    self.inbound_substreams_delay.remove(delay_key);
                                }
                                break;
                            }
                            Poll::Ready(Err(error)) => {
                                println!("Error when trying to send a response: {}", error);

                                substreams_to_remove.push(*id);
                                if let Some(ref delay_key) = info.delay_key {
                                    self.inbound_substreams_delay.remove(delay_key);
                                }

                                self.events_out.push(Err("Response error"));

                                break;
                            }
                            Poll::Pending => {
                                info.state = InboundState::Busy(fut);
                                break;
                            }
                        };
                    }
                    InboundState::Poisoned => unreachable!("Poisoned inbound substream"),
                }
            }
        }

        for inbound_id in substreams_to_remove {
            self.inbound_substreams.remove(&inbound_id);
        }

        for outbound_id in self.outbound_substreams.keys().copied().collect::<Vec<_>>() {
            let (mut entry, state) = match self.outbound_substreams.entry(outbound_id) {
                Entry::Occupied(mut entry) => {
                    let state = std::mem::replace(
                        &mut entry.get_mut().state,
                        OutboundSubstreamState::Poisoned,
                    );
                    (entry, state)
                }
                Entry::Vacant(_) => unreachable!(),
            };

            match state {
                OutboundSubstreamState::RequestPendingResponse {
                    substream,
                    request: _,
                } if deactivated => {
                    entry.get_mut().state = OutboundSubstreamState::Closing(substream);
                    self.events_out.push(Err("Handler deactivated"))
                }
                OutboundSubstreamState::RequestPendingResponse {
                    mut substream,
                    request,
                } => match substream.poll_next_unpin(cx) {
                    Poll::Ready(Some(Ok(response))) => {
                        if request.expected_responses() > 1 && !response.close_after() {
                            let substream_entry = entry.get_mut();
                            let delay_key = &substream_entry.delay_key;
                            let remaining_chunks = substream_entry
                                .remaining_chunks
                                .map(|count| count.saturating_sub(1))
                                .unwrap_or_else(|| 0);
                            if remaining_chunks == 0 {
                                substream_entry.state = OutboundSubstreamState::Closing(substream);
                            } else {
                                substream_entry.state =
                                    OutboundSubstreamState::RequestPendingResponse {
                                        substream,
                                        request,
                                    };
                                substream_entry.remaining_chunks = Some(remaining_chunks);
                                self.outbound_substreams_delay
                                    .reset(delay_key, Duration::from_secs(RESPONSE_TIMEOUT));
                            }
                        } else {
                            entry.get_mut().state = OutboundSubstreamState::Closing(substream);
                        }

                        let id = entry.get().req_id;
                        let _proto = entry.get().proto;

                        let received = match response {
                            RPCCodedResponse::Success(resp) => Ok(RPCReceived::Response(id, resp)),
                            RPCCodedResponse::Error => Err("RPC Error"),
                        };

                        return Poll::Ready(ConnectionHandlerEvent::Custom(received));
                    }
                    Poll::Ready(None) => {
                        let delay_key = &entry.get().delay_key;
                        let _request_id = entry.get().req_id;
                        self.outbound_substreams_delay.remove(delay_key);
                        entry.remove_entry();

                        let outbound_err = "Incomplete response";
                        return Poll::Ready(ConnectionHandlerEvent::Custom(Err(outbound_err)));
                    }
                    Poll::Pending => {
                        entry.get_mut().state =
                            OutboundSubstreamState::RequestPendingResponse { substream, request }
                    }
                    Poll::Ready(Some(Err(_))) => {
                        let delay_key = &entry.get().delay_key;
                        self.outbound_substreams_delay.remove(delay_key);
                        let outbound_err = "Stream dropped";
                        entry.remove_entry();
                        return Poll::Ready(ConnectionHandlerEvent::Custom(Err(outbound_err)));
                    }
                },
                OutboundSubstreamState::Closing(mut substream) => {
                    match Sink::poll_close(Pin::new(&mut substream), cx) {
                        Poll::Ready(_) => {
                            let delay_key = &entry.get().delay_key;
                            let _protocol = entry.get().proto;
                            let _request_id = entry.get().req_id;
                            self.outbound_substreams_delay.remove(delay_key);
                            entry.remove_entry();
                        }
                        Poll::Pending => {
                            entry.get_mut().state = OutboundSubstreamState::Closing(substream);
                        }
                    }
                }
                OutboundSubstreamState::Poisoned => {
                    unreachable!("Coding Error: Outbound substream is poisoned")
                }
            }
        }

        if !self.dial_queue.is_empty() && self.dial_negotiated < self.max_dial_negotiated {
            self.dial_negotiated += 1;
            let (id, req) = self.dial_queue.remove(0);
            self.dial_queue.shrink_to_fit();
            return Poll::Ready(ConnectionHandlerEvent::OutboundSubstreamRequest {
                protocol: SubstreamProtocol::new(OutboundRequestContainer { req: req.clone() }, ())
                    .map_info(|()| (id, req)),
            });
        }

        if let HandlerState::ShuttingDown(_) = self.state {
            if self.dial_queue.is_empty()
                && self.outbound_substreams.is_empty()
                && self.inbound_substreams.is_empty()
                && self.events_out.is_empty()
                && self.dial_negotiated == 0
            {
                return Poll::Ready(ConnectionHandlerEvent::Close(RPCError::Error));
            }
        }

        Poll::Pending
    }
}

async fn send_message_to_inbound_substream(
    mut substream: InboundSubstream,
    message: RPCCodedResponse,
    last_chunk: bool,
) -> Result<(InboundSubstream, bool), RPCError> {
    let is_error = matches!(message, RPCCodedResponse::Error);

    let send_result = substream.send(message).await;

    if last_chunk || is_error || send_result.is_err() {
        let close_result = substream.close().await.map(|_| (substream, true));
        if let Err(e) = send_result {
            return Err(e);
        } else {
            return close_result;
        }
    }

    send_result.map(|_| (substream, false))
}
