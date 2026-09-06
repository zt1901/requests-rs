// Copyright (C) 2019, Cloudflare, Inc.
// All rights reserved.
//
// Redistribution and use in source and binary forms, with or without
// modification, are permitted provided that the following conditions are
// met:
//
//     * Redistributions of source code must retain the above copyright notice,
//       this list of conditions and the following disclaimer.
//
//     * Redistributions in binary form must reproduce the above copyright
//       notice, this list of conditions and the following disclaimer in the
//       documentation and/or other materials provided with the distribution.
//
// THIS SOFTWARE IS PROVIDED BY THE COPYRIGHT HOLDERS AND CONTRIBUTORS "AS
// IS" AND ANY EXPRESS OR IMPLIED WARRANTIES, INCLUDING, BUT NOT LIMITED TO,
// THE IMPLIED WARRANTIES OF MERCHANTABILITY AND FITNESS FOR A PARTICULAR
// PURPOSE ARE DISCLAIMED. IN NO EVENT SHALL THE COPYRIGHT HOLDER OR
// CONTRIBUTORS BE LIABLE FOR ANY DIRECT, INDIRECT, INCIDENTAL, SPECIAL,
// EXEMPLARY, OR CONSEQUENTIAL DAMAGES (INCLUDING, BUT NOT LIMITED TO,
// PROCUREMENT OF SUBSTITUTE GOODS OR SERVICES; LOSS OF USE, DATA, OR
// PROFITS; OR BUSINESS INTERRUPTION) HOWEVER CAUSED AND ON ANY THEORY OF
// LIABILITY, WHETHER IN CONTRACT, STRICT LIABILITY, OR TORT (INCLUDING
// NEGLIGENCE OR OTHERWISE) ARISING IN ANY WAY OUT OF THE USE OF THIS
// SOFTWARE, EVEN IF ADVISED OF THE POSSIBILITY OF SUCH DAMAGE.

use super::Error;
use super::Result;

use crate::h3::Header;
use std::collections::VecDeque;

use super::INDEXED;
use super::INDEXED_WITH_POST_BASE;
use super::LITERAL;
use super::LITERAL_WITH_NAME_REF;

#[derive(Clone, Copy, Debug, PartialEq)]
enum Representation {
    Indexed,
    IndexedWithPostBase,
    Literal,
    LiteralWithNameRef,
    LiteralWithPostBase,
}

impl Representation {
    pub fn from_byte(b: u8) -> Representation {
        if b & INDEXED == INDEXED {
            return Representation::Indexed;
        }

        if b & LITERAL_WITH_NAME_REF == LITERAL_WITH_NAME_REF {
            return Representation::LiteralWithNameRef;
        }

        if b & LITERAL == LITERAL {
            return Representation::Literal;
        }

        if b & INDEXED_WITH_POST_BASE == INDEXED_WITH_POST_BASE {
            return Representation::IndexedWithPostBase;
        }

        Representation::LiteralWithPostBase
    }
}

/// Helper for tracking decoded field list sizes.
///
/// The size of a field list is calculated based on the uncompressed size of
/// fields, including the length of the name and value in bytes plus an overhead
/// of 32 bytes for each field. See
/// <https://datatracker.ietf.org/doc/html/rfc9114#section-4.2.2>
struct FieldListSizeTracker {
    remaining: u64,
}

impl FieldListSizeTracker {
    /// Initialize tracker with the maximum field list size.
    ///
    /// The `max_size` parameter is the maximum size in bytes of the full
    /// decoded field list. See
    /// <https://datatracker.ietf.org/doc/html/rfc9114#section-4.2.2>
    fn new(max_size: u64) -> Self {
        Self {
            remaining: max_size,
        }
    }

    /// Mark the start of parsing a new field.
    ///
    /// Must be called when a new field is ready to be parsed.
    fn on_field_start(&mut self) -> Result<()> {
        // Each complete field has a 32-byte overhead, so subtract that first.
        self.remaining = self
            .remaining
            .checked_sub(32)
            .ok_or(Error::HeaderListTooLarge)?;

        Ok(())
    }

    /// Marks when a field part (either name or value) has been decoded.
    ///
    /// The `len` parameter is the size in bytes of the decoded part.
    ///
    /// Must be called when a new field part has been decoded.
    fn on_field_part_decoded(&mut self, len: u64) -> Result<()> {
        self.remaining = self
            .remaining
            .checked_sub(len)
            .ok_or(Error::HeaderListTooLarge)?;

        Ok(())
    }

    /// The remaining number of bytes in the tracker.
    fn left_usize(&self) -> usize {
        usize::try_from(self.remaining).unwrap_or(usize::MAX)
    }
}

/// A QPACK decoder.
#[derive(Default)]
pub struct Decoder {
    // Entries are oldest first; absolute indices are never reused.
    table: VecDeque<Entry>,
    max_capacity: u64,
    capacity: u64,
    size: u64,
    inserts: u64,
    pending: Vec<u8>,
    pending_needed: usize,
}

struct Entry {
    absolute: u64,
    name: Vec<u8>,
    value: Vec<u8>,
}

impl Entry {
    fn size(&self) -> u64 {
        self.name.len() as u64 + self.value.len() as u64 + 32
    }
}

enum Instruction {
    Capacity(u64),
    Insert(Vec<u8>, Vec<u8>),
}

enum ControlError {
    More(usize),
    Invalid,
}

type ControlResult<T> = std::result::Result<T, ControlError>;

/// A parser that reports exactly how much of the current instruction is needed.
/// This avoids repeatedly decoding long literals when QUIC delivers one byte at
/// a time, and avoids buffering an unbounded series of complete instructions.
struct ControlCursor<'a> {
    bytes: &'a [u8],
    offset: usize,
}

impl ControlCursor<'_> {
    fn first(&self) -> ControlResult<u8> {
        self.bytes
            .get(self.offset)
            .copied()
            .ok_or_else(|| ControlError::More(self.offset + 1))
    }

    fn int(&mut self, prefix: u32) -> ControlResult<u64> {
        let mask = (1u64 << prefix) - 1;
        let mut value = u64::from(self.first()?) & mask;
        self.offset += 1;
        if value < mask {
            return Ok(value);
        }
        let mut shift = 0;
        loop {
            let byte = self.first()?;
            self.offset += 1;
            let digit = u64::from(byte & 0x7f);
            if shift >= 64 || digit > (u64::MAX >> shift) {
                return Err(ControlError::Invalid);
            }
            value = value
                .checked_add(digit << shift)
                .ok_or(ControlError::Invalid)?;
            if byte & 0x80 == 0 {
                return Ok(value);
            }
            shift += 7;
            // A continuation at this position cannot fit in a u64. Fail now,
            // not after an attacker supplies another byte.
            if shift >= 64 {
                return Err(ControlError::Invalid);
            }
        }
    }

    fn string(&mut self, prefix: u32, max_len: u64) -> ControlResult<Vec<u8>> {
        let huffman = self.first()? & (1 << prefix) != 0;
        let len = self.int(prefix)?;
        // HPACK Huffman codes use at most 30 bits per decoded octet plus
        // at most seven padding bits. Check declared length before buffering.
        let max_encoded = if huffman {
            max_len.saturating_mul(4).saturating_add(1)
        } else {
            max_len
        };
        if len > max_encoded {
            return Err(ControlError::Invalid);
        }
        let len = usize::try_from(len).map_err(|_| ControlError::Invalid)?;
        let end = self.offset.checked_add(len).ok_or(ControlError::Invalid)?;
        if end > self.bytes.len() {
            return Err(ControlError::More(end));
        }
        let data = &self.bytes[self.offset..end];
        self.offset = end;
        if huffman {
            octets::Octets::with_slice(data)
                .get_huffman_decoded_with_max_length(usize::try_from(max_len).unwrap_or(usize::MAX))
                .map_err(|_| ControlError::Invalid)
        } else {
            Ok(data.to_vec())
        }
    }
}

impl Decoder {
    /// Creates a new QPACK decoder.
    pub fn new() -> Decoder {
        Decoder::default()
    }

    /// Creates a decoder with the advertised maximum dynamic table capacity.
    /// The encoder must still send a Set Dynamic Table Capacity instruction.
    pub fn with_capacity(max_capacity: u64) -> Decoder {
        Decoder {
            max_capacity,
            ..Decoder::default()
        }
    }

    /// Returns the number of successful insertions, including duplicates.
    pub fn insert_count(&self) -> u64 {
        self.inserts
    }

    /// Read-only internal diagnostics: table bytes, current capacity, entries,
    /// and retained incomplete encoder-instruction bytes (not allocation size).
    #[cfg(feature = "internal")]
    #[doc(hidden)]
    pub fn resource_usage(&self) -> (u64, u64, usize, usize) {
        (
            self.size,
            self.capacity,
            self.table.len(),
            self.pending.len(),
        )
    }

    /// Processes a (possibly fragmented) sequence of encoder instructions.
    /// Only a single incomplete instruction is retained between calls.
    pub fn control(&mut self, mut buf: &mut [u8]) -> Result<()> {
        if self.max_capacity == 0 && !buf.is_empty() {
            return Err(Error::InvalidEncoderInstruction);
        }
        while !buf.is_empty() {
            let needed = self.pending_needed.max(1);
            let take = (needed - self.pending.len()).min(buf.len());
            self.pending.extend_from_slice(&buf[..take]);
            buf = &mut buf[take..];
            if self.pending.len() < needed {
                continue;
            }
            match self.parse_instruction(&self.pending) {
                Ok(instruction) => {
                    self.apply_instruction(instruction)?;
                    self.pending.clear();
                    self.pending_needed = 1;
                }
                Err(ControlError::More(needed)) => {
                    self.pending_needed = needed;
                }
                Err(ControlError::Invalid) => {
                    return Err(Error::InvalidEncoderInstruction);
                }
            }
        }
        Ok(())
    }

    fn parse_instruction(&self, bytes: &[u8]) -> ControlResult<Instruction> {
        let mut cursor = ControlCursor { bytes, offset: 0 };
        let first = cursor.first()?;
        if first & 0xe0 == 0x20 {
            let capacity = cursor.int(5)?;
            if capacity > self.max_capacity {
                return Err(ControlError::Invalid);
            }
            return Ok(Instruction::Capacity(capacity));
        }
        let budget = self.capacity.checked_sub(32).ok_or(ControlError::Invalid)?;
        let name = if first & 0x80 != 0 {
            let index = cursor.int(6)?;
            let name = if first & 0x40 != 0 {
                lookup_static(index).map_err(|_| ControlError::Invalid)?.0
            } else {
                &self
                    .relative_entry(index)
                    .map_err(|_| ControlError::Invalid)?
                    .name
            };
            if name.len() as u64 > budget {
                return Err(ControlError::Invalid);
            }
            name.to_vec()
        } else if first & 0x40 != 0 {
            cursor.string(5, budget)?
        } else {
            let index = cursor.int(5)?;
            let entry = self
                .relative_entry(index)
                .map_err(|_| ControlError::Invalid)?;
            // Clone before insertion can evict the source (RFC 9204 3.2.2).
            return Ok(Instruction::Insert(entry.name.clone(), entry.value.clone()));
        };
        let remaining = budget
            .checked_sub(name.len() as u64)
            .ok_or(ControlError::Invalid)?;
        let value = cursor.string(7, remaining)?;
        Ok(Instruction::Insert(name, value))
    }

    fn apply_instruction(&mut self, instruction: Instruction) -> Result<()> {
        match instruction {
            Instruction::Capacity(capacity) => {
                self.capacity = capacity;
                self.evict_to(capacity);
            }
            Instruction::Insert(name, value) => {
                let size = (name.len() as u64)
                    .checked_add(value.len() as u64)
                    .and_then(|v| v.checked_add(32))
                    .ok_or(Error::InvalidEncoderInstruction)?;
                let target = self
                    .capacity
                    .checked_sub(size)
                    .ok_or(Error::InvalidEncoderInstruction)?;
                let next = self
                    .inserts
                    .checked_add(1)
                    .ok_or(Error::InvalidEncoderInstruction)?;
                self.evict_to(target);
                self.table.push_back(Entry {
                    absolute: self.inserts,
                    name,
                    value,
                });
                self.size += size;
                self.inserts = next;
            }
        }
        Ok(())
    }

    fn evict_to(&mut self, target: u64) {
        while self.size > target {
            if let Some(entry) = self.table.pop_front() {
                self.size -= entry.size();
            } else {
                break;
            }
        }
    }

    fn entry(&self, absolute: u64) -> Result<&Entry> {
        let first = self
            .table
            .front()
            .ok_or(Error::InvalidHeaderValue)?
            .absolute;
        let index = absolute
            .checked_sub(first)
            .ok_or(Error::InvalidHeaderValue)?;
        let index = usize::try_from(index).map_err(|_| Error::InvalidHeaderValue)?;
        self.table.get(index).ok_or(Error::InvalidHeaderValue)
    }

    fn relative_entry(&self, relative: u64) -> Result<&Entry> {
        let absolute = self
            .inserts
            .checked_sub(relative)
            .and_then(|v| v.checked_sub(1))
            .ok_or(Error::InvalidHeaderValue)?;
        self.entry(absolute)
    }

    /// Reconstructs the Required Insert Count using the current insertion count.
    /// Call once when a field section arrives and retain the result if blocked.
    pub fn required_insert_count(&self, buf: &[u8]) -> Result<u64> {
        let encoded = decode_int(&mut octets::Octets::with_slice(buf), 8)?;
        if encoded == 0 {
            return Ok(0);
        }
        // RFC 9204 section 4.5.1.1. Wide intermediates prevent wrapping near
        // u64::MAX without rejecting otherwise representable insert counts.
        let max_entries = u128::from(self.max_capacity / 32);
        let full_range = 2 * max_entries;
        if u128::from(encoded) > full_range {
            return Err(Error::InvalidHeaderValue);
        }
        let max_value = u128::from(self.inserts) + max_entries;
        let max_wrapped = (max_value / full_range) * full_range;
        let mut required = max_wrapped + u128::from(encoded) - 1;
        if required > max_value {
            if required <= full_range {
                return Err(Error::InvalidHeaderValue);
            }
            required -= full_range;
        }
        if required == 0 {
            return Err(Error::InvalidHeaderValue);
        }
        u64::try_from(required).map_err(|_| Error::InvalidHeaderValue)
    }

    /// Decodes a QPACK header block into a list of headers.
    pub fn decode(&mut self, buf: &[u8], max_size: u64) -> Result<Vec<Header>> {
        let required = self.required_insert_count(buf)?;
        self.decode_with_required(buf, max_size, required)
    }

    /// Decodes using the Required Insert Count frozen at initial receipt.
    /// This must not be recomputed when retrying after encoder-stream progress:
    /// later inserts can change the reconstruction's modulo window.
    pub fn decode_with_required(
        &mut self,
        buf: &[u8],
        max_size: u64,
        required: u64,
    ) -> Result<Vec<Header>> {
        let mut b = octets::Octets::with_slice(buf);
        let encoded = decode_int(&mut b, 8)?;
        let full_range = (self.max_capacity / 32) * 2;
        if required == 0 {
            if encoded != 0 {
                return Err(Error::InvalidHeaderValue);
            }
        } else if full_range == 0 || encoded != (required % full_range) + 1 {
            return Err(Error::InvalidHeaderValue);
        }
        let negative = b.peek_u8()? & 0x80 != 0;
        let delta = decode_int(&mut b, 7)?;
        let base = if negative {
            required.checked_sub(delta).and_then(|v| v.checked_sub(1))
        } else {
            required.checked_add(delta)
        }
        .ok_or(Error::InvalidHeaderValue)?;
        if required > self.inserts {
            return Err(Error::Blocked);
        }

        let mut out = Vec::new();
        let mut size_tracker = FieldListSizeTracker::new(max_size);
        let mut largest_reference = None;
        while b.cap() > 0 {
            let first = b.peek_u8()?;
            size_tracker.on_field_start()?;
            match Representation::from_byte(first) {
                Representation::Indexed | Representation::IndexedWithPostBase => {
                    let post =
                        Representation::from_byte(first) == Representation::IndexedWithPostBase;
                    let index = decode_int(&mut b, if post { 4 } else { 6 })?;
                    let (name, value) = if !post && first & 0x40 != 0 {
                        lookup_static(index)?
                    } else {
                        let entry =
                            self.field_entry(base, index, post, required, &mut largest_reference)?;
                        (entry.name.as_slice(), entry.value.as_slice())
                    };
                    size_tracker.on_field_part_decoded(name.len() as u64)?;
                    size_tracker.on_field_part_decoded(value.len() as u64)?;
                    out.push(Header::new(name, value));
                }
                Representation::Literal => {
                    let name = decode_literal(&mut b, 3, size_tracker.left_usize())?;
                    size_tracker.on_field_part_decoded(name.len() as u64)?;
                    let value = decode_str(&mut b, size_tracker.left_usize())?;
                    size_tracker.on_field_part_decoded(value.len() as u64)?;
                    out.push(Header(name, value));
                }
                Representation::LiteralWithNameRef | Representation::LiteralWithPostBase => {
                    let post =
                        Representation::from_byte(first) == Representation::LiteralWithPostBase;
                    let index = decode_int(&mut b, if post { 3 } else { 4 })?;
                    let name = if !post && first & 0x10 != 0 {
                        lookup_static(index)?.0
                    } else {
                        &self
                            .field_entry(base, index, post, required, &mut largest_reference)?
                            .name
                    };
                    size_tracker.on_field_part_decoded(name.len() as u64)?;
                    let value = decode_str(&mut b, size_tracker.left_usize())?;
                    size_tracker.on_field_part_decoded(value.len() as u64)?;
                    out.push(Header(name.to_vec(), value));
                }
            }
        }
        // The Required Insert Count is exactly one beyond the largest dynamic
        // reference (or zero for a section with only static/literal fields).
        if largest_reference.map(|v| v + 1).unwrap_or(0) != required {
            return Err(Error::InvalidHeaderValue);
        }
        Ok(out)
    }

    fn field_entry(
        &self,
        base: u64,
        index: u64,
        post: bool,
        required: u64,
        largest: &mut Option<u64>,
    ) -> Result<&Entry> {
        let absolute = if post {
            base.checked_add(index)
        } else {
            base.checked_sub(index).and_then(|v| v.checked_sub(1))
        }
        .ok_or(Error::InvalidHeaderValue)?;
        if absolute >= required {
            return Err(Error::InvalidHeaderValue);
        }
        let entry = self.entry(absolute)?;
        *largest = Some(largest.map_or(absolute, |v| v.max(absolute)));
        Ok(entry)
    }
}

fn lookup_static(idx: u64) -> Result<(&'static [u8], &'static [u8])> {
    if idx >= super::static_table::STATIC_DECODE_TABLE.len() as u64 {
        return Err(Error::InvalidStaticTableIndex);
    }

    Ok(super::static_table::STATIC_DECODE_TABLE[idx as usize])
}

fn decode_int(b: &mut octets::Octets, prefix: usize) -> Result<u64> {
    let mask = 2u64.pow(prefix as u32) - 1;

    let mut val = u64::from(b.get_u8()?);
    val &= mask;

    if val < mask {
        return Ok(val);
    }

    let mut shift = 0;

    while b.cap() > 0 {
        let byte = b.get_u8()?;

        let digit = u64::from(byte & 0x7f);
        // checked_shl checks the shift count, not discarded high bits.
        // Reject an overflowing final digit instead of accepting its truncation.
        if shift >= 64 || digit > (u64::MAX >> shift) {
            return Err(Error::BufferTooShort);
        }
        let inc = digit << shift;

        val = val.checked_add(inc).ok_or(Error::BufferTooShort)?;

        shift += 7;

        if byte & 0x80 == 0 {
            return Ok(val);
        }
    }

    Err(Error::BufferTooShort)
}

fn decode_str(b: &mut octets::Octets, max_len: usize) -> Result<Vec<u8>> {
    decode_literal(b, 7, max_len)
}

fn decode_literal(b: &mut octets::Octets, prefix: usize, max_len: usize) -> Result<Vec<u8>> {
    let huff = b.peek_u8()? & (1 << prefix) != 0;
    let len = usize::try_from(decode_int(b, prefix)?).map_err(|_| Error::InvalidHeaderValue)?;
    if !huff && len > max_len {
        return Err(Error::HeaderListTooLarge);
    }
    let mut val = b.get_bytes(len)?;
    if huff {
        val.get_huffman_decoded_with_max_length(max_len)
            .map_err(|_| Error::HeaderListTooLarge)
    } else {
        Ok(val.to_vec())
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn dynamic_control_fragmentation_and_eviction() {
        let mut decoder = Decoder::with_capacity(64);
        // capacity 64, literal a:b, duplicate relative 0 (evicts original).
        let bytes = [0x3f, 0x21, 0x41, b'a', 0x01, b'b', 0x00];
        for byte in bytes {
            decoder.control(&mut [byte]).unwrap();
        }
        assert_eq!(decoder.insert_count(), 2);
        assert_eq!(decoder.size, 34);
        assert_eq!(decoder.table.len(), 1);
        assert_eq!(decoder.entry(1).unwrap().name, b"a");
        assert!(decoder.entry(0).is_err());
        decoder.control(&mut [0x20]).unwrap();
        assert!(decoder.table.is_empty());
        assert_eq!(decoder.insert_count(), 2);
    }

    #[test]
    fn dynamic_control_rejects_capacity_index_and_oversized_literal() {
        assert_eq!(
            Decoder::new().control(&mut [0x20]),
            Err(Error::InvalidEncoderInstruction)
        );
        let mut decoder = Decoder::with_capacity(64);
        assert_eq!(
            decoder.control(&mut [0x3f, 0x22]),
            Err(Error::InvalidEncoderInstruction)
        );
        let mut decoder = Decoder::with_capacity(64);
        decoder.control(&mut [0x3f, 0x21]).unwrap();
        assert_eq!(
            decoder.control(&mut [0x00]),
            Err(Error::InvalidEncoderInstruction)
        );
        let mut decoder = Decoder::with_capacity(64);
        decoder.control(&mut [0x3f, 0x21]).unwrap();
        // Length 33 cannot fit into capacity 64, even with an empty value.
        assert_eq!(
            decoder.control(&mut [0x5f, 0x02]),
            Err(Error::InvalidEncoderInstruction)
        );
    }

    #[test]
    fn dynamic_field_representations_and_limits() {
        let mut decoder = Decoder::with_capacity(128);
        decoder
            .control(&mut [0x3f, 0x61, 0x41, b'a', 1, b'b', 0x41, b'c', 1, b'd'])
            .unwrap();
        let expected = vec![Header::new(b"a", b"b"), Header::new(b"c", b"d")];
        assert_eq!(
            decoder.decode(&[3, 0, 0x81, 0x80], 68),
            Ok(expected.clone())
        );
        assert_eq!(decoder.decode(&[3, 0x81, 0x10, 0x11], 68), Ok(expected));
        assert_eq!(
            decoder.decode(&[3, 0, 0x41, 1, b'x', 0x40, 1, b'y'], 68),
            Ok(vec![Header::new(b"a", b"x"), Header::new(b"c", b"y")])
        );
        assert_eq!(
            decoder.decode(&[3, 0x81, 0, 1, b'x', 1, 1, b'y'], 68),
            Ok(vec![Header::new(b"a", b"x"), Header::new(b"c", b"y")])
        );
        assert_eq!(
            decoder.decode(&[3, 0, 0x81, 0x80], 67),
            Err(Error::HeaderListTooLarge)
        );
        assert_eq!(
            decoder.decode(&[2, 0, 0x10], 100),
            Err(Error::InvalidHeaderValue)
        );
        assert_eq!(
            decoder.decode(&[3, 0, 0x81], 100),
            Err(Error::InvalidHeaderValue)
        );
        assert_eq!(
            decoder.decode(&[3, 0x82, 0x10], 100),
            Err(Error::InvalidHeaderValue)
        );
    }

    #[test]
    fn blocked_fields_and_frozen_required_count() {
        let mut decoder = Decoder::with_capacity(64);
        let section = [2, 0, 0x80];
        let required = decoder.required_insert_count(&section).unwrap();
        assert_eq!(required, 1);
        assert_eq!(
            decoder.decode_with_required(&section, 100, required),
            Err(Error::Blocked)
        );
        decoder
            .control(&mut [0x3f, 0x21, 0x41, b'a', 1, b'b'])
            .unwrap();
        assert_eq!(
            decoder.decode_with_required(&section, 100, required),
            Ok(vec![Header::new(b"a", b"b")])
        );
        decoder.control(&mut [0, 0]).unwrap();
        assert_eq!(decoder.required_insert_count(&section), Ok(5));
        // The original entry was illegally evicted by this test's encoder:
        // report an invalid reference, never wait for an unrelated future one.
        assert_eq!(
            decoder.decode_with_required(&section, 100, required),
            Err(Error::InvalidHeaderValue)
        );
    }

    #[test]
    fn required_count_wrap_and_arithmetic_boundaries() {
        let mut decoder = Decoder::with_capacity(100);
        decoder.inserts = 10;
        assert_eq!(decoder.required_insert_count(&[4]), Ok(9));
        assert_eq!(
            decoder.required_insert_count(&[7]),
            Err(Error::InvalidHeaderValue)
        );
        decoder.inserts = u64::MAX;
        assert!(decoder.required_insert_count(&[4]).is_ok());
        let mut decoder = Decoder::with_capacity(u64::MAX);
        decoder.inserts = u64::MAX;
        assert_eq!(
            decoder.decode_with_required(&[0, 0x80], 100, 0),
            Err(Error::InvalidHeaderValue)
        );
        // Base = Required + Delta cannot wrap to a valid-looking index.
        let mut prefix = vec![0; 32];
        let mut b = octets::OctetsMut::with_slice(&mut prefix);
        let range = (u64::MAX / 32) * 2;
        super::super::encode_int((u64::MAX % range) + 1, 0, 8, &mut b).unwrap();
        super::super::encode_int(1, 0, 7, &mut b).unwrap();
        let len = b.off();
        assert_eq!(
            decoder.decode_with_required(&prefix[..len], 100, u64::MAX),
            Err(Error::InvalidHeaderValue)
        );
    }

    #[test]
    fn required_insert_count_is_not_silently_ignored() {
        let mut decoder = Decoder::new();
        assert_eq!(
            decoder.decode(&[0x01, 0x00, 0xd9], u64::MAX),
            Err(Error::InvalidHeaderValue),
        );
        assert!(decoder.decode(&[0x00, 0x00, 0xd9], u64::MAX).is_ok());
    }

    #[test]
    fn decode_int_rejects_overflowing_final_digit() {
        let mut encoded = vec![0xff];
        encoded.extend_from_slice(&[0x80; 9]);
        encoded.push(0x02);
        assert_eq!(
            decode_int(&mut octets::Octets::with_slice(&encoded), 8),
            Err(Error::BufferTooShort),
        );
    }

    #[test]
    fn decode_int1() {
        let encoded = [0b01010, 0x02];
        let mut b = octets::Octets::with_slice(&encoded);

        assert_eq!(decode_int(&mut b, 5), Ok(10));
    }

    #[test]
    fn decode_int2() {
        let encoded = [0b11111, 0b10011010, 0b00001010];
        let mut b = octets::Octets::with_slice(&encoded);

        assert_eq!(decode_int(&mut b, 5), Ok(1337));
    }

    #[test]
    fn decode_int3() {
        let encoded = [0b101010];
        let mut b = octets::Octets::with_slice(&encoded);

        assert_eq!(decode_int(&mut b, 8), Ok(42));
    }

    /// LiteralWithNameRef with the dynamic table flag (S=0) must be
    /// rejected when no dynamic entry exists.
    #[test]
    fn literal_with_name_ref_dynamic_table_rejected() {
        let mut dec = Decoder::new();

        // QPACK header block:
        //   [0x00, 0x00]  required insert count=0, base=0
        //   [0x40]        LiteralWithNameRef (0b0100_0000), S=0 (dynamic),
        //                 name_idx=0
        //   [0x01, 0x61]  value: non-huffman, length=1, 'a'
        let encoded = [0x00, 0x00, 0x40, 0x01, 0x61];

        assert_eq!(
            dec.decode(&encoded, u64::MAX),
            Err(super::super::Error::InvalidHeaderValue),
        );
    }

    /// Non-Huffman value in decode_str must be rejected before allocation
    /// when it exceeds the remaining budget (max_len).
    ///
    /// Uses a LiteralWithNameRef with static `:authority` (index 0, 10
    /// bytes) and a non-Huffman value of 5 bytes. Budget is set so only
    /// 4 bytes remain for the value after the name is charged.
    #[test]
    fn non_huffman_value_exceeding_budget_rejected() {
        // QPACK header block:
        //   [0x00, 0x00]  required insert count=0, base=0
        //   [0x50]        LiteralWithNameRef, S=1 (static), name_idx=0
        //                 (:authority, 10 bytes)
        //   [0x05]        value: non-huffman (bit 7=0), length=5
        //   "abcde"       value bytes
        let encoded = [0x00, 0x00, 0x50, 0x05, 0x61, 0x62, 0x63, 0x64, 0x65];

        // Exact fit: 32 (overhead) + 10 (name) + 5 (value) = 47.
        assert!(Decoder::new().decode(&encoded, 47).is_ok());

        // One byte too small: budget = 46.
        // After overhead (32) and name (10): 4 bytes remain, but
        // value is 5 bytes → rejected.
        assert_eq!(
            Decoder::new().decode(&encoded, 46),
            Err(super::super::Error::HeaderListTooLarge),
        );
    }

    /// Non-Huffman name in the Literal arm must be rejected before
    /// allocation when it exceeds the remaining budget.
    ///
    /// Uses a Literal with a 10-byte non-Huffman name and a 1-byte
    /// non-Huffman value. Budget is set so only 9 bytes remain for the
    /// name after overhead.
    #[test]
    fn non_huffman_name_exceeding_budget_rejected() {
        // QPACK header block:
        //   [0x00, 0x00]  required insert count=0, base=0
        //   [0x27, 0x03]  Literal (0b0010_0000), N=0, H=0 (no huffman),
        //                 name_len=10 (3-bit prefix 0b111 + overflow 0x03)
        //   "x-custom99"  name bytes (10 bytes)
        //   [0x01, 0x61]  value: non-huffman, length=1, 'a'
        let encoded = [
            0x00, 0x00, // header block prefix
            0x27, 0x03, // Literal, name_len=10
            0x78, 0x2d, 0x63, 0x75, 0x73, 0x74, 0x6f, 0x6d, 0x39, 0x39, // "x-custom99"
            0x01, 0x61, // value: length=1, 'a'
        ];

        // Exact fit: 32 (overhead) + 10 (name) + 1 (value) = 43.
        assert!(Decoder::new().decode(&encoded, 43).is_ok());

        // Two bytes too small: budget = 41.
        // After overhead (32): 9 bytes remain, name is 10 bytes →
        // rejected at name check.
        assert_eq!(
            Decoder::new().decode(&encoded, 41),
            Err(super::super::Error::HeaderListTooLarge),
        );
    }

    /// Verify that both Literal and LiteralWithNameRef charge the name to
    /// the budget *before* Huffman-decoding the value, so the max_len
    /// passed to the Huffman decoder is equally strict in both paths.
    #[test]
    fn literal_with_name_ref_value_budget_ordering() {
        use crate::h3::qpack;

        // Static table index 0 = `:authority` (10 bytes).
        // We'll encode the same value two ways:
        //   1. LiteralWithNameRef using `:authority` (encoder matches static
        //      table)
        //   2. Literal using a custom 10-byte name not in the static table
        //
        // Both have name_len=10, so budget arithmetic is comparable.
        let value = b"aaaaaaaaaaaaaaaa"; // 16 bytes; Huffman compresses to 10

        // -- Encode as LiteralWithNameRef --
        let headers_nameref = vec![crate::h3::Header::new(b":authority", value)];
        let mut buf = [0u8; 64];
        let mut enc = qpack::Encoder::new();
        let nameref_len = enc.encode(&headers_nameref, &mut buf).unwrap();
        let encoded_nameref = buf[..nameref_len].to_vec();

        // -- Encode as Literal (name not in static table) --
        let headers_literal = vec![crate::h3::Header::new(b"x-custom99", value)];
        let mut buf = [0u8; 64];
        let mut enc = qpack::Encoder::new();
        let literal_len = enc.encode(&headers_literal, &mut buf).unwrap();
        let encoded_literal = buf[..literal_len].to_vec();

        // Exact budget: 32 (overhead) + 10 (name) + 16 (value) = 58.
        // Both representations succeed.
        assert_eq!(
            Decoder::new().decode(&encoded_nameref, 58),
            Ok(headers_nameref.clone()),
        );
        assert_eq!(
            Decoder::new().decode(&encoded_literal, 58),
            Ok(headers_literal.clone()),
        );

        // One byte too small: budget = 57.
        // Total decoded field size = 10 + 16 = 26, overhead = 32,
        // so 32 + 26 = 58 > 57. Both reject with HeaderListTooLarge.
        //
        // Both paths now follow the same budget ordering:
        //   1. name charged first (10 bytes) → remaining = 15
        //   2. decode_str max_len = 15
        //   3. Huffman decode produces 16 bytes > 15 → rejected
        assert_eq!(
            Decoder::new().decode(&encoded_nameref, 57),
            Err(super::super::Error::HeaderListTooLarge),
        );
        assert_eq!(
            Decoder::new().decode(&encoded_literal, 57),
            Err(super::super::Error::HeaderListTooLarge),
        );
    }
}
