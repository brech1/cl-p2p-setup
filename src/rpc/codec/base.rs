use crate::rpc::{
    methods::{ErrorType, RPCCodedResponse, RPCResponse},
    protocol::{InboundRequest, OutboundRequest},
};
use libp2p::bytes::{BufMut, BytesMut};
use tokio_util::codec::{Decoder, Encoder};

pub trait OutboundCodec<TItem>: Encoder<TItem> + Decoder {
    type CodecErrorType;

    fn decode_error(
        &mut self,
        src: &mut BytesMut,
    ) -> Result<Option<Self::CodecErrorType>, <Self as Decoder>::Error>;
}

// Inbound codec

pub struct BaseInboundCodec<TCodec>
where
    TCodec: Encoder<RPCCodedResponse> + Decoder,
{
    inner: TCodec,
}

impl<TCodec> BaseInboundCodec<TCodec>
where
    TCodec: Encoder<RPCCodedResponse> + Decoder,
{
    pub fn new(codec: TCodec) -> Self {
        BaseInboundCodec { inner: codec }
    }
}

impl<TCodec> Encoder<RPCCodedResponse> for BaseInboundCodec<TCodec>
where
    TCodec: Decoder + Encoder<RPCCodedResponse>,
{
    type Error = <TCodec as Encoder<RPCCodedResponse>>::Error;

    fn encode(&mut self, item: RPCCodedResponse, dst: &mut BytesMut) -> Result<(), Self::Error> {
        dst.clear();
        dst.reserve(1);
        dst.put_u8(
            item.as_u8()
                .expect("Should never encode a stream termination"),
        );
        self.inner.encode(item, dst)
    }
}

impl<TCodec> Decoder for BaseInboundCodec<TCodec>
where
    TCodec: Encoder<RPCCodedResponse> + Decoder<Item = InboundRequest>,
{
    type Item = InboundRequest;
    type Error = <TCodec as Decoder>::Error;

    fn decode(&mut self, src: &mut BytesMut) -> Result<Option<Self::Item>, Self::Error> {
        self.inner.decode(src)
    }
}

// Outbound Codec

pub struct BaseOutboundCodec<TOutboundCodec>
where
    TOutboundCodec: OutboundCodec<OutboundRequest>,
{
    inner: TOutboundCodec,
    current_response_code: Option<u8>,
}

impl<TOutboundCodec> BaseOutboundCodec<TOutboundCodec>
where
    TOutboundCodec: OutboundCodec<OutboundRequest>,
{
    pub fn new(codec: TOutboundCodec) -> Self {
        BaseOutboundCodec {
            inner: codec,
            current_response_code: None,
        }
    }
}

impl<TCodec> Encoder<OutboundRequest> for BaseOutboundCodec<TCodec>
where
    TCodec: OutboundCodec<OutboundRequest> + Encoder<OutboundRequest>,
{
    type Error = <TCodec as Encoder<OutboundRequest>>::Error;

    fn encode(&mut self, item: OutboundRequest, dst: &mut BytesMut) -> Result<(), Self::Error> {
        self.inner.encode(item, dst)
    }
}

impl<TCodec> Decoder for BaseOutboundCodec<TCodec>
where
    TCodec:
        OutboundCodec<OutboundRequest, CodecErrorType = ErrorType> + Decoder<Item = RPCResponse>,
{
    type Item = RPCCodedResponse;
    type Error = <TCodec as Decoder>::Error;

    fn decode(&mut self, src: &mut BytesMut) -> Result<Option<Self::Item>, Self::Error> {
        if src.len() <= 1 {
            return Ok(None);
        }

        let response_code = self.current_response_code.unwrap_or_else(|| {
            let resp_code = src.split_to(1)[0];
            self.current_response_code = Some(resp_code);
            resp_code
        });

        let inner_result = {
            if RPCCodedResponse::is_response(response_code) {
                self.inner
                    .decode(src)
                    .map(|r| r.map(RPCCodedResponse::Success))
            } else {
                self.inner
                    .decode_error(src)
                    .map(|r| r.map(|resp| RPCCodedResponse::from_error(response_code, resp)))
            }
        };

        if let Ok(Some(_)) = inner_result {
            self.current_response_code = None;
        }

        inner_result
    }
}
