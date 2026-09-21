use std::io;

pub trait Codable {
    type EncodeErr;
    type DecodeErr;

    fn encode(&self, writer: &mut impl io::Write) -> Result<usize, Self::EncodeErr>;

    fn decode(reader: &mut impl io::Read) -> Result<Self, Self::DecodeErr>
    where
        Self: Sized;
}
