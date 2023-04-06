use super::base::OutboundCodec;
use crate::rpc::{
    methods::{ErrorType, MetaData, Ping, RPCCodedResponse, RPCResponse, StatusMessage},
    protocol::{
        InboundRequest, OutboundRequest, Protocol, ProtocolId, RPCError, MAX_RPC_SIZE_POST_MERGE,
    },
};
use libp2p::bytes::BytesMut;
use snap::{read::FrameDecoder, write::FrameEncoder};
use ssz::{Decode, Encode};
use ssz_types::VariableList;
use std::io::{Cursor, Read, Write};
use tokio_util::codec::{Decoder, Encoder};
use unsigned_varint::codec::Uvi;

// Outbound Codec

pub struct SSZSnappyInboundCodec {
    protocol: ProtocolId,
    inner: Uvi<usize>,
    len: Option<usize>,
}

impl SSZSnappyInboundCodec {
    pub fn new(protocol: ProtocolId) -> Self {
        Self {
            protocol,
            inner: Uvi::default(),
            len: None,
        }
    }
}

impl Encoder<RPCCodedResponse> for SSZSnappyInboundCodec {
    type Error = RPCError;

    fn encode(&mut self, item: RPCCodedResponse, dst: &mut BytesMut) -> Result<(), Self::Error> {
        let bytes = match &item {
            RPCCodedResponse::Success(resp) => match &resp {
                RPCResponse::Status(res) => res.as_ssz_bytes(),
                RPCResponse::Pong(res) => res.data.as_ssz_bytes(),
                RPCResponse::MetaData(res) => res.as_ssz_bytes(),
            },
            RPCCodedResponse::Error => "".into(),
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

        dst.extend_from_slice(writer.get_ref());
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
                    Err(RPCError::Error)
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
                *len = Some(length);
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
    pub fn new(protocol: ProtocolId) -> Self {
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

                handle_response(self.protocol.message_name, &decoded_buffer)
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

        if length > MAX_RPC_SIZE_POST_MERGE {
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
                Ok(Some(ErrorType(
                    VariableList::from_ssz_bytes(&decoded_buffer).or(Err(RPCError::Error))?,
                )))
            }
            Err(_) => Err(RPCError::Error),
        }
    }
}

fn handle_response(
    protocol: Protocol,
    decoded_buffer: &[u8],
) -> Result<Option<RPCResponse>, RPCError> {
    match protocol {
        Protocol::Status => Ok(Some(RPCResponse::Status(
            StatusMessage::from_ssz_bytes(decoded_buffer).unwrap(),
        ))),
        Protocol::Ping => Ok(Some(RPCResponse::Pong(Ping {
            data: u64::from_ssz_bytes(decoded_buffer).unwrap(),
        }))),
        Protocol::MetaData => Ok(Some(RPCResponse::MetaData(
            MetaData::from_ssz_bytes(decoded_buffer).unwrap(),
        ))),
        _ => Err(RPCError::Error),
    }
}
