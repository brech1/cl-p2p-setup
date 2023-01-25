use super::methods::{Ping, StatusMessage};

#[derive(Debug, Clone, PartialEq)]
pub enum OutboundRequest {
    Status(StatusMessage),
    Goodbye(u64),
    Ping(Ping),
    MetaData,
}
