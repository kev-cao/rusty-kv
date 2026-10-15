use std::io;

pub trait Codable {
    fn encode(&self, writer: &mut impl io::Write) -> Result<usize, io::Error>;

    fn decode(reader: &mut impl io::Read) -> Result<Self, io::Error>
    where
        Self: Sized;
}
