use int_enum::IntEnum;

use super::*;

/// The type field describes the type of packet.
#[repr(u8)]
#[derive(Debug, Default, Clone, Copy, PartialEq, Eq, IntEnum)]
pub(crate) enum PacketType {
    #[default]
    Data = 0,
    Fin = 1,
    State = 2,
    Reset = 3,
    Syn = 4,
}

/// UTP packet structure
#[derive(Debug)]
pub(crate) struct UtpPacket {
    pub header: Header,
    pub payload: Bytes,
}
