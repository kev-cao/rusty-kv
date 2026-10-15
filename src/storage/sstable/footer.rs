use byteorder::{LittleEndian, ReadBytesExt, WriteBytesExt};
use std::io;

use crate::storage::{
    encoding::Codable,
    sstable::{SST_VERSION, SsTableError, ValidationSnafu},
};

pub const FOOTER_TAIL_SIZE: usize = 18;

// NB: If a new field is added to the footer, the FOOTER_SIZE constant must be
// updated.
const FOOTER_SIZE: usize = 24;

const FOOTER_MAGIC: u64 = 0xdeadcafedeadcafe;

/// The footer tail is a small structure that is written at the end of an SSTable
/// file. To preserve backward compatibility, the footer tail size must always
/// remain static.
pub struct SsTableFooterTail {
    pub sst_version: u16,
    /// The length of the footer metadata section, in bytes. This is used to read
    /// the footer from the end of a file. This length does includes the length
    /// of the footer tail itself.
    pub footer_size: u64,
    pub magic: u64,
}

impl SsTableFooterTail {
    pub fn new() -> Self {
        Self {
            sst_version: SST_VERSION,
            footer_size: FOOTER_SIZE as u64 + FOOTER_TAIL_SIZE as u64,
            magic: FOOTER_MAGIC,
        }
    }

    pub fn validate(&self) -> Result<(), SsTableError> {
        if self.magic != FOOTER_MAGIC {
            return ValidationSnafu {
                reason: "invalid footer magic number",
            }
            .fail();
        }
        Ok(())
    }
}

impl Codable for SsTableFooterTail {
    fn encode(&self, writer: &mut impl io::Write) -> Result<usize, io::Error> {
        writer.write_u16::<LittleEndian>(self.sst_version)?;
        writer.write_u64::<LittleEndian>(self.footer_size)?;
        writer.write_u64::<LittleEndian>(self.magic)?;
        Ok(size_of_val(&self.sst_version)
            + size_of_val(&self.footer_size)
            + size_of_val(&self.magic))
    }

    fn decode(reader: &mut impl io::Read) -> Result<Self, io::Error> {
        let sst_version = reader.read_u16::<LittleEndian>()?;
        let footer_size = reader.read_u64::<LittleEndian>()?;
        let magic = reader.read_u64::<LittleEndian>()?;
        Ok(Self {
            sst_version,
            footer_size,
            magic,
        })
    }
}

#[derive(Debug)]
// NB: If a new field is added to the footer, the FOOTER_SIZE constant must be
// updated.
pub struct SsTableFooter {
    /// The filter must lay before all other sections in the footer.
    pub filter_offset: u64,
    pub filter_size: u32,
    pub index_offset: u64,
    pub index_size: u32,
}

impl Codable for SsTableFooter {
    fn encode(&self, writer: &mut impl io::Write) -> Result<usize, io::Error> {
        writer.write_u64::<LittleEndian>(self.filter_offset)?;
        writer.write_u32::<LittleEndian>(self.filter_size)?;
        writer.write_u64::<LittleEndian>(self.index_offset)?;
        writer.write_u32::<LittleEndian>(self.index_size)?;
        Ok(size_of_val(&self.filter_offset)
            + size_of_val(&self.filter_size)
            + size_of_val(&self.index_offset)
            + size_of_val(&self.index_size))
    }

    fn decode(reader: &mut impl io::Read) -> Result<Self, io::Error> {
        let ret = Self {
            filter_offset: reader.read_u64::<LittleEndian>()?,
            filter_size: reader.read_u32::<LittleEndian>()?,
            index_offset: reader.read_u64::<LittleEndian>()?,
            index_size: reader.read_u32::<LittleEndian>()?,
        };
        Ok(ret)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::testutils::TestResult;

    #[test]
    fn footer_size() -> TestResult<()> {
        let mut bytes = Vec::new();
        SsTableFooter {
            filter_offset: 0,
            filter_size: 10,
            index_offset: 10,
            index_size: 10,
        }
        .encode(&mut bytes)
        .expect("failed to encode footer");
        assert_eq!(bytes.len(), FOOTER_SIZE);
        Ok(())
    }
}
