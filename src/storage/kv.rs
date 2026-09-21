use byteorder::{LittleEndian, ReadBytesExt, WriteBytesExt};
use snafu::prelude::*;
use std::io;

use super::encoding::Codable;

pub type KeyLen = u16;
pub type ValueLen = u32;

pub const KEY_LEN_MAX: u16 = KeyLen::MAX;
pub const VALUE_LEN_MAX: u32 = ValueLen::MAX;

#[derive(Snafu, Debug)]
pub enum KeyError {
    #[snafu(display(
        "key length exceeds maximum allowed size, (actual {actual_size}, max {KEY_LEN_MAX})"
    ))]
    KeyLenExceeded { actual_size: usize },
}

#[derive(Default, Debug, PartialEq, Eq, PartialOrd, Ord, Clone)]
pub struct Key(Vec<u8>);

impl Key {
    pub fn len(&self) -> KeyLen {
        self.0.len() as KeyLen
    }
}

impl TryFrom<&[u8]> for Key {
    type Error = KeyError;

    fn try_from(key: &[u8]) -> Result<Self, Self::Error> {
        if key.len() > KEY_LEN_MAX as usize {
            return KeyLenExceededSnafu {
                actual_size: key.len(),
            }
            .fail();
        }
        Ok(Self(Vec::from(key)))
    }
}

impl TryFrom<Vec<u8>> for Key {
    type Error = KeyError;

    fn try_from(key: Vec<u8>) -> Result<Self, Self::Error> {
        if key.len() > KEY_LEN_MAX as usize {
            return KeyLenExceededSnafu {
                actual_size: key.len(),
            }
            .fail();
        }
        Ok(Self(key))
    }
}

impl AsRef<[u8]> for Key {
    fn as_ref(&self) -> &[u8] {
        &self.0
    }
}

impl Codable for Key {
    type EncodeErr = io::Error;
    type DecodeErr = io::Error;

    fn encode(&self, writer: &mut impl io::Write) -> Result<usize, Self::EncodeErr> {
        writer.write_u16::<LittleEndian>(self.len())?;
        writer.write_all(&self.0)?;
        Ok(size_of::<KeyLen>() + self.0.len())
    }

    fn decode(reader: &mut impl io::Read) -> Result<Self, Self::DecodeErr> {
        let key_len: KeyLen = reader.read_u16::<LittleEndian>()?;
        let mut key = Self(vec![0u8; key_len as usize]);
        reader.read_exact(&mut key.0)?;
        Ok(key)
    }
}

#[derive(Snafu, Debug)]
pub enum ValueError {
    #[snafu(display(
        "value length exceeds maximum allowed size, (actual {actual_size}, max {VALUE_LEN_MAX})"
    ))]
    ValueLenExceeded { actual_size: usize },
}

#[derive(Debug, PartialEq, Clone)]
pub struct Value(Vec<u8>);

impl Value {
    pub fn len(&self) -> ValueLen {
        self.0.len() as ValueLen
    }

    /// Decodes the length of a value from the reader, assuming the reader is
    /// positioned at the start of a serialized value.
    pub fn decode_len(reader: &mut impl io::Read) -> Result<ValueLen, io::Error> {
        reader.read_u32::<LittleEndian>()
    }
}

impl TryFrom<&[u8]> for Value {
    type Error = ValueError;

    fn try_from(value: &[u8]) -> Result<Self, Self::Error> {
        if value.len() > KEY_LEN_MAX as usize {
            return ValueLenExceededSnafu {
                actual_size: value.len(),
            }
            .fail();
        }
        Ok(Self(Vec::from(value)))
    }
}

impl TryFrom<Vec<u8>> for Value {
    type Error = ValueError;

    fn try_from(key: Vec<u8>) -> Result<Self, Self::Error> {
        if key.len() > VALUE_LEN_MAX as usize {
            return ValueLenExceededSnafu {
                actual_size: key.len(),
            }
            .fail();
        }
        Ok(Self(key))
    }
}

impl AsRef<[u8]> for Value {
    fn as_ref(&self) -> &[u8] {
        &self.0
    }
}

impl Codable for Value {
    type EncodeErr = io::Error;
    type DecodeErr = io::Error;

    fn encode(&self, writer: &mut impl io::Write) -> Result<usize, Self::EncodeErr> {
        writer.write_u32::<LittleEndian>(self.len())?;
        writer.write_all(&self.0)?;
        Ok(size_of::<ValueLen>() + self.0.len())
    }

    fn decode(reader: &mut impl io::Read) -> Result<Self, Self::DecodeErr> {
        let val_len: ValueLen = reader.read_u32::<LittleEndian>()?;
        let mut val = Self(vec![0u8; val_len as usize]);
        reader.read_exact(&mut val.0)?;
        Ok(val)
    }
}
