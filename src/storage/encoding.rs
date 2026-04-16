use bytes::BufMut;
use std::io::Read;
use std::mem;

pub type KeyLen = u16;
pub type ValueLen = u32;

pub const KEY_LEN_MAX: usize = KeyLen::MAX as usize;
pub const VALUE_LEN_MAX: usize = ValueLen::MAX as usize;

#[derive(thiserror::Error, Debug)]
pub enum CodecError {
    #[error("key length exceeds maximum allowed size, (actual {0}, max {KEY_LEN_MAX})")]
    KeyLengthExceeded(usize),
    #[error("value length exceeds maximum allowed size, (actual {0}, max {VALUE_LEN_MAX})")]
    ValueLengthExceeded(usize),
    #[error(transparent)]
    IOError(#[from] std::io::Error),
}

/// Encodes a key into the provided buffer, prefixed by its length.
pub fn encode_key(key: &[u8], buf: &mut impl BufMut) -> Result<(), CodecError> {
    if key.len() > KEY_LEN_MAX {
        return Err(CodecError::KeyLengthExceeded(key.len()));
    }

    buf.put_u16_le(key.len() as KeyLen);
    buf.put_slice(key);
    Ok(())
}

/// Encodes a value into the provided buffer, prefixed by its length.
pub fn encode_value(value: &[u8], buf: &mut impl BufMut) -> Result<(), CodecError> {
    if value.len() > VALUE_LEN_MAX {
        return Err(CodecError::ValueLengthExceeded(value.len()));
    }

    buf.put_u32_le(value.len() as ValueLen);
    buf.put_slice(value);
    Ok(())
}

/// Decodes a key from the provided reader.
pub fn decode_key(reader: &mut impl Read) -> Result<Vec<u8>, CodecError> {
    let mut key_lenb = [0u8; mem::size_of::<KeyLen>()];
    reader.read_exact(&mut key_lenb)?;
    let key_len = KeyLen::from_le_bytes(key_lenb);
    let mut key = vec![0u8; key_len as usize];
    reader.read_exact(&mut key)?;
    Ok(key)
}

/// Decodes a value from the provided reader.
pub fn decode_value(reader: &mut impl Read) -> Result<Vec<u8>, CodecError> {
    let mut value_lenb = [0u8; mem::size_of::<ValueLen>()];
    reader.read_exact(&mut value_lenb)?;
    let value_len = ValueLen::from_le_bytes(value_lenb);
    let mut value = vec![0u8; value_len as usize];
    reader.read_exact(&mut value)?;
    Ok(value)
}
