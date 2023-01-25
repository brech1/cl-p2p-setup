use crate::rpc::{
    methods::{ErrorType, MetaData, RPCCodedResponse, RPCResponse},
    outbound::OutboundRequest,
    protocol::{InboundRequest, Protocol, ProtocolId, RPCError, MAX_RPC_SIZE_POST_MERGE},
};
use libp2p::bytes::BytesMut;
use snap::{read::FrameDecoder, write::FrameEncoder};
use ssz::{Decode, Encode};
use ssz_types::VariableList;
use std::io::{Cursor, Read, Write};
use tokio_util::codec::{Decoder, Encoder};
use unsigned_varint::codec::Uvi;

use super::base::OutboundCodec;

const CONTEXT_BYTES_LEN: usize = 4;

// Outbound Codec

pub struct SSZSnappyInboundCodec {
    protocol: ProtocolId,
    inner: Uvi<usize>,
    len: Option<usize>,
}

impl SSZSnappyInboundCodec {
    fn new(protocol: ProtocolId) -> Self {
        Self {
            protocol,
            inner: Uvi::default(),
            len: None,
        }
    }
}

impl Encoder<RPCCodedResponse> for SSZSnappyInboundCodec {
    type Error = RPCError;

    fn encode(
        &mut self,
        item: RPCCodedResponse,
        _dst: &mut libp2p::bytes::BytesMut,
    ) -> Result<(), Self::Error> {
        let _bytes = match &item {
            RPCCodedResponse::Success(m) => println!("{m:?}"),
            RPCCodedResponse::Error => (),
            RPCCodedResponse::StreamTermination => (),
        };

        Ok(())
    }
}

impl Decoder for SSZSnappyInboundCodec {
    type Item = InboundRequest;
    type Error = RPCError;

    fn decode(
        &mut self,
        src: &mut libp2p::bytes::BytesMut,
    ) -> Result<Option<Self::Item>, Self::Error> {
        if self.protocol.message_name == Protocol::MetaData {
            return Ok(Some(InboundRequest::MetaData));
        }

        let length = match handle_length(&mut self.inner, &mut self.len, src)? {
            Some(len) => len,
            None => return Ok(None),
        };

        let ssz_limits = self.protocol.rpc_response_limits();

        if ssz_limits.is_out_of_bounds(length, MAX_RPC_SIZE_POST_MERGE) {
            return Err(RPCError::Error);
        }

        let max_compressed_len = snap::raw::max_compress_len(length) as u64;

        let limit_reader = Cursor::new(src.as_ref()).take(max_compressed_len);
        let mut reader = FrameDecoder::new(limit_reader);

        let mut decoded_buffer = vec![0; length];

        match reader.read_exact(&mut decoded_buffer) {
            Ok(()) => {
                let n = reader.get_ref().get_ref().position();
                self.len = None;
                let _read_bytes = src.split_to(n as usize);

                if self.protocol.message_name == Protocol::MetaData {
                    if !decoded_buffer.is_empty() {
                        Err(RPCError::Error)
                    } else {
                        Ok(Some(InboundRequest::MetaData))
                    }
                } else {
                    return Err(RPCError::Error);
                }
            }
            Err(_) => Err(RPCError::Error),
        }
    }
}

pub fn handle_length(
    uvi_codec: &mut Uvi<usize>,
    len: &mut Option<usize>,
    bytes: &mut BytesMut,
) -> Result<Option<usize>, RPCError> {
    if let Some(length) = len {
        Ok(Some(*length))
    } else {
        match uvi_codec.decode(bytes).map_err(RPCError::from)? {
            Some(length) => {
                *len = Some(length as usize);
                Ok(Some(length))
            }
            None => Ok(None),
        }
    }
}

// Outbound Codec

pub struct SSZSnappyOutboundCodec {
    protocol: ProtocolId,
    inner: Uvi<usize>,
    len: Option<usize>,
}

impl SSZSnappyOutboundCodec {
    fn new(protocol: ProtocolId) -> Self {
        Self {
            protocol,
            inner: Uvi::default(),
            len: None,
        }
    }
}

impl Encoder<OutboundRequest> for SSZSnappyOutboundCodec {
    type Error = RPCError;

    fn encode(&mut self, item: OutboundRequest, dst: &mut BytesMut) -> Result<(), Self::Error> {
        let bytes = match item {
            OutboundRequest::Status(req) => req.as_ssz_bytes(),
            OutboundRequest::Goodbye(req) => req.as_ssz_bytes(),
            OutboundRequest::Ping(req) => req.as_ssz_bytes(),
            OutboundRequest::MetaData => return Ok(()),
        };

        if bytes.len() > MAX_RPC_SIZE_POST_MERGE {
            return Err(RPCError::Error);
        }

        self.inner
            .encode(bytes.len(), dst)
            .map_err(RPCError::from)?;

        let mut writer = FrameEncoder::new(Vec::new());
        writer.write_all(&bytes).map_err(RPCError::from)?;
        writer.flush().map_err(RPCError::from)?;

        // Write compressed bytes to `dst`
        dst.extend_from_slice(writer.get_ref());
        Ok(())
    }
}

impl Decoder for SSZSnappyOutboundCodec {
    type Item = RPCResponse;
    type Error = RPCError;

    fn decode(&mut self, src: &mut BytesMut) -> Result<Option<Self::Item>, Self::Error> {
        if self.protocol.has_context_bytes() {
            if src.len() >= CONTEXT_BYTES_LEN {
                let context_bytes = src.split_to(CONTEXT_BYTES_LEN);
                let mut result = [0; CONTEXT_BYTES_LEN];
                result.copy_from_slice(context_bytes.as_ref());
            } else {
                return Ok(None);
            }
        }
        let length = match handle_length(&mut self.inner, &mut self.len, src)? {
            Some(len) => len,
            None => return Ok(None),
        };

        let ssz_limits = self.protocol.rpc_response_limits();

        if ssz_limits.is_out_of_bounds(length, MAX_RPC_SIZE_POST_MERGE) {
            return Err(RPCError::Error);
        }

        // Calculate worst case compression length for given uncompressed length
        let max_compressed_len = snap::raw::max_compress_len(length) as u64;
        // Create a limit reader as a wrapper that reads only upto `max_compressed_len` from `src`.
        let limit_reader = Cursor::new(src.as_ref()).take(max_compressed_len);
        let mut reader = FrameDecoder::new(limit_reader);

        let mut decoded_buffer = vec![0; length];

        match reader.read_exact(&mut decoded_buffer) {
            Ok(()) => {
                let n = reader.get_ref().get_ref().position();
                self.len = None;
                let _read_bytes = src.split_to(n as usize);

                if self.protocol.message_name == Protocol::MetaData {
                    if !decoded_buffer.is_empty() {
                        Err(RPCError::Error)
                    } else {
                        // TODO: Fix unwrap
                        Ok(Some(RPCResponse::MetaData(
                            MetaData::from_ssz_bytes(&decoded_buffer).unwrap(),
                        )))
                    }
                } else {
                    return Err(RPCError::Error);
                }
            }
            Err(_) => Err(RPCError::Error),
        }
    }
}

impl OutboundCodec<OutboundRequest> for SSZSnappyOutboundCodec {
    type CodecErrorType = ErrorType;

    fn decode_error(
        &mut self,
        src: &mut BytesMut,
    ) -> Result<Option<Self::CodecErrorType>, RPCError> {
        let length = match handle_length(&mut self.inner, &mut self.len, src)? {
            Some(len) => len,
            None => return Ok(None),
        };

        // Should not attempt to decode rpc chunks with `length > max_packet_size` or not within bounds of
        // packet size for ssz container corresponding to `ErrorType`.
        if length > MAX_RPC_SIZE_POST_MERGE {
            return Err(RPCError::Error);
        }

        // Calculate worst case compression length for given uncompressed length
        let max_compressed_len = snap::raw::max_compress_len(length) as u64;
        // Create a limit reader as a wrapper that reads only upto `max_compressed_len` from `src`.
        let limit_reader = Cursor::new(src.as_ref()).take(max_compressed_len);
        let mut reader = FrameDecoder::new(limit_reader);
        let mut decoded_buffer = vec![0; length];
        match reader.read_exact(&mut decoded_buffer) {
            Ok(()) => {
                // `n` is how many bytes the reader read in the compressed stream
                let n = reader.get_ref().get_ref().position();
                self.len = None;
                let _read_bytes = src.split_to(n as usize);
                Ok(Some(ErrorType(
                    VariableList::from_ssz_bytes(&decoded_buffer).or(Err(RPCError::Error))?,
                )))
            }
            Err(_) => Err(RPCError::Error),
        }
    }
}
