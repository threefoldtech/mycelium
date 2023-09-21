pub use control::ControlPacket;
pub use data::DataPacket;

mod control;
mod data;

#[derive(Debug, Clone)]
pub enum Packet {
    DataPacket(DataPacket),
    ControlPacket(ControlPacket),
}

#[derive(Debug, Clone, Copy)]
#[repr(u8)]
pub enum PacketType {
    DataPacket = 0,
    ControlPacket = 1,
}
