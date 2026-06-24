//! Small canonical binary-codec helpers used by signed protocol artifacts.
//!
//! This module is private on purpose.  A single implementation of bounds checks,
//! integer byte order, and trailing-byte rejection avoids subtle discrepancies
//! between messages, checkpoints, and recovery evidence.

use crate::error::{Result, codec};

pub(crate) struct Encoder {
    bytes: Vec<u8>,
}

impl Encoder {
    pub(crate) fn with_capacity(capacity: usize) -> Self {
        Self {
            bytes: Vec::with_capacity(capacity),
        }
    }

    pub(crate) fn u8(&mut self, value: u8) {
        self.bytes.push(value);
    }

    pub(crate) fn u16(&mut self, value: u16) {
        self.bytes.extend_from_slice(&value.to_be_bytes());
    }

    pub(crate) fn u32(&mut self, value: u32) {
        self.bytes.extend_from_slice(&value.to_be_bytes());
    }

    pub(crate) fn u64(&mut self, value: u64) {
        self.bytes.extend_from_slice(&value.to_be_bytes());
    }

    pub(crate) fn fixed(&mut self, value: &[u8]) {
        self.bytes.extend_from_slice(value);
    }

    pub(crate) fn length_prefixed(&mut self, value: &[u8]) -> Result<()> {
        let length = u32::try_from(value.len())
            .map_err(|_| codec("wire byte string is too large for a u32 length"))?;
        self.u32(length);
        self.fixed(value);
        Ok(())
    }

    pub(crate) fn into_bytes(self) -> Vec<u8> {
        self.bytes
    }
}

pub(crate) struct Decoder<'a> {
    input: &'a [u8],
    offset: usize,
}

impl<'a> Decoder<'a> {
    pub(crate) fn new(input: &'a [u8], maximum: usize, artifact: &str) -> Result<Self> {
        if input.len() > maximum {
            return Err(codec(format!(
                "{artifact} has {} bytes, exceeding limit {maximum}",
                input.len()
            )));
        }
        Ok(Self { input, offset: 0 })
    }

    pub(crate) fn u8(&mut self, field: &str) -> Result<u8> {
        Ok(self.take(1, field)?[0])
    }

    pub(crate) fn u16(&mut self, field: &str) -> Result<u16> {
        Ok(u16::from_be_bytes(self.fixed(field)?))
    }

    pub(crate) fn u32(&mut self, field: &str) -> Result<u32> {
        Ok(u32::from_be_bytes(self.fixed(field)?))
    }

    pub(crate) fn u64(&mut self, field: &str) -> Result<u64> {
        Ok(u64::from_be_bytes(self.fixed(field)?))
    }

    pub(crate) fn fixed<const N: usize>(&mut self, field: &str) -> Result<[u8; N]> {
        let mut output = [0_u8; N];
        output.copy_from_slice(self.take(N, field)?);
        Ok(output)
    }

    pub(crate) fn length_prefixed(&mut self, maximum: usize, field: &str) -> Result<&'a [u8]> {
        let length_u32 = self.u32(&format!("{field} length"))?;
        let length = usize::try_from(length_u32)
            .map_err(|_| codec(format!("{field} length does not fit this platform")))?;
        if length > maximum {
            return Err(codec(format!(
                "{field} length {length} exceeds limit {maximum}"
            )));
        }
        self.take(length, field)
    }

    pub(crate) fn finish(self, artifact: &str) -> Result<()> {
        if self.offset != self.input.len() {
            return Err(codec(format!(
                "{artifact} has {} trailing bytes",
                self.input.len() - self.offset
            )));
        }
        Ok(())
    }

    fn take(&mut self, length: usize, field: &str) -> Result<&'a [u8]> {
        let end = self
            .offset
            .checked_add(length)
            .ok_or_else(|| codec(format!("{field} offset overflow")))?;
        let value = self
            .input
            .get(self.offset..end)
            .ok_or_else(|| codec(format!("truncated {field}")))?;
        self.offset = end;
        Ok(value)
    }
}
