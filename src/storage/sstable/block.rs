use std::{fs, io};

use crate::storage::encoding::Codable;
use crate::storage::kv::{Key, Value};

use snafu::prelude::*;

pub const BLOCK_MAX_SIZE: u32 = 4 * 1024; // 4KB
pub struct SsTableBlock {
    data: Vec<u8>,
}

impl SsTableBlock {
    pub fn new() -> Self {
        Self { data: Vec::new() }
    }

    /// Returns the size of the block on disk in bytes.
    pub fn block_size(&self) -> usize {
        self.data.len()
    }

    pub fn encode(&self, writer: &mut impl io::Write) -> Result<usize, io::Error> {
        writer.write_all(&self.data)?;
        Ok(self.data.len())
    }

    pub fn decode(reader: &mut impl io::Read, size: usize) -> Result<Self, io::Error> {
        let mut data = vec![0u8; size];
        reader.read_exact(&mut data)?;
        Ok(Self { data })
    }
}

#[derive(Snafu, Debug)]
pub enum SsTableBlockBuilderError {
    #[snafu(context(false), display("SSTable io operation failed"))]
    IoError {
        source: io::Error,
        backtrace: snafu::Backtrace,
    },
    #[snafu(display("SSTable block is full"))]
    BlockFull { backtrace: snafu::Backtrace },
    #[snafu(display("SSTable block is empty"))]
    EmptyBlock { backtrace: snafu::Backtrace },
}

#[derive(Clone, Copy, Debug, PartialEq)]
pub struct SsTableBlockBuilderOpts {
    /// The maximum size of the block in bytes. This serves as a soft-limit;
    /// once the block exceeds or equals this size, the builder will no longer
    /// accepts new KV pairs.
    pub max_size: u32,
}

impl Default for SsTableBlockBuilderOpts {
    fn default() -> Self {
        Self {
            max_size: BLOCK_MAX_SIZE,
        }
    }
}

pub struct SsTableBlockBuilder {
    block: SsTableBlock,
    opts: SsTableBlockBuilderOpts,
}

impl SsTableBlockBuilder {
    pub fn new(opts: SsTableBlockBuilderOpts) -> Self {
        Self {
            block: SsTableBlock::new(),
            opts,
        }
    }

    /// Adds a key-value pair to the block. Returns true if the block is full
    /// after adding the pair. This assumes that the keys are added in sorted
    /// order.
    pub fn add_kv(&mut self, key: &Key, value: &Value) -> Result<bool, SsTableBlockBuilderError> {
        let mut written = self.block.data.len();
        if written >= self.opts.max_size as usize {
            return BlockFullSnafu.fail();
        }
        written = written.saturating_add(key.encode(&mut self.block.data)?);
        written = written.saturating_add(value.encode(&mut self.block.data)?);
        Ok(written >= self.opts.max_size as usize)
    }

    /// Checks if the block built so far is empty.
    pub fn is_empty(&self) -> bool {
        self.block.data.is_empty()
    }

    /// Finalizes the block and returns the built `SsTableBlock`.
    pub fn finish(self) -> Result<SsTableBlock, SsTableBlockBuilderError> {
        if self.block.data.is_empty() {
            return EmptyBlockSnafu.fail();
        }
        Ok(self.block)
    }
}

#[cfg(test)]
mod tests {}
