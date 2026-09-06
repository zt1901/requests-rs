//! Bounded, reproducible property checks. Not a substitute for continuous fuzzing.
use quiche::h3::qpack::{Decoder, Error};
use quiche::h3::{Header, NameValue};
use std::collections::VecDeque;

struct Rng(u64);
impl Rng {
    fn next(&mut self) -> u64 {
        self.0 ^= self.0 << 13;
        self.0 ^= self.0 >> 7;
        self.0 ^= self.0 << 17;
        self.0
    }
    fn below(&mut self, n: usize) -> usize {
        self.next() as usize % n
    }
    fn bytes(&mut self, n: usize) -> Vec<u8> {
        (0..n).map(|_| self.next() as u8).collect()
    }
}

fn integer(mut n: u64, prefix: u8, tag: u8) -> Vec<u8> {
    let max = (1u64 << prefix) - 1;
    if n < max {
        return vec![tag | n as u8];
    }
    let mut out = vec![tag | max as u8];
    n -= max;
    while n >= 128 {
        out.push(n as u8 | 128);
        n >>= 7;
    }
    out.push(n as u8);
    out
}

fn literal(name: &[u8], value: &[u8]) -> Vec<u8> {
    let mut out = integer(name.len() as u64, 5, 0x40);
    out.extend(name);
    out.extend(integer(value.len() as u64, 7, 0));
    out.extend(value);
    out
}

fn fragmented(dec: &mut Decoder, data: &[u8], rng: &mut Rng) -> Result<(), Error> {
    let mut start = 0;
    while start < data.len() {
        // Empty calls must not change state or require new bytes.
        dec.control(&mut [])?;
        let end = (start + 1 + rng.below(7)).min(data.len());
        dec.control(&mut data[start..end].to_vec())?;
        start = end;
    }
    Ok(())
}

fn assert_budget(dec: &Decoder, max_capacity: u64) {
    let (size, capacity, entries, pending) = dec.resource_usage();
    assert!(size <= capacity && capacity <= max_capacity);
    assert!(entries as u64 <= capacity / 32);
    // At most one instruction, with bounded Huffman input plus integer overhead.
    assert!(pending as u64 <= max_capacity.saturating_mul(4).saturating_add(32));
}

#[test]
fn seeded_valid_stream_matches_independent_table_and_fragmentation_model() {
    for seed in 1..=128 {
        let mut rng = Rng(0x92_04_20_26 ^ seed);
        let max_capacity = 64 + 32 * rng.below(15) as u64;
        let mut whole = Decoder::with_capacity(max_capacity);
        let mut split = Decoder::with_capacity(max_capacity);
        let mut model: VecDeque<(Vec<u8>, Vec<u8>)> = VecDeque::new();
        let mut capacity = 0u64;
        let mut size = 0u64;
        let mut inserts = 0u64;
        for step in 0..128 {
            let op = rng.below(5);
            let (wire, entry) = if op == 0 || capacity < 48 {
                capacity = if step % 17 == 0 {
                    0
                } else {
                    48 + rng.below((max_capacity - 47) as usize) as u64
                };
                while size > capacity {
                    let (n, v) = model.pop_front().unwrap();
                    size -= (32 + n.len() + v.len()) as u64;
                }
                (integer(capacity, 5, 0x20), None)
            } else if op == 1 && !model.is_empty() {
                let relative = rng.below(model.len());
                let entry = model[model.len() - 1 - relative].clone();
                (integer(relative as u64, 5, 0), Some(entry))
            } else {
                let len = 1 + rng.below(7);
                let name = if op == 2 {
                    b":path".to_vec()
                } else {
                    rng.bytes(len)
                };
                let len = rng.below((capacity as usize - 32 - name.len()).min(24) + 1);
                let value = rng.bytes(len);
                let wire = if op == 2 {
                    let mut wire = integer(1, 6, 0xc0); // Static :path name.
                    wire.extend(integer(value.len() as u64, 7, 0));
                    wire.extend(&value);
                    wire
                } else {
                    literal(&name, &value)
                };
                (wire, Some((name, value)))
            };
            if let Some(entry) = entry {
                let entry_size = (32 + entry.0.len() + entry.1.len()) as u64;
                while size + entry_size > capacity {
                    let (n, v) = model.pop_front().unwrap();
                    size -= (32 + n.len() + v.len()) as u64;
                }
                size += entry_size;
                model.push_back(entry);
                inserts += 1;
            }
            whole.control(&mut wire.clone()).unwrap();
            fragmented(&mut split, &wire, &mut rng).unwrap();
            assert_eq!(whole.insert_count(), inserts, "seed={seed} step={step}");
            assert_eq!(split.insert_count(), inserts);
            assert_eq!(whole.resource_usage(), (size, capacity, model.len(), 0));
            assert_eq!(whole.resource_usage(), split.resource_usage());
            assert_budget(&whole, max_capacity);

            let mut block = if model.is_empty() {
                vec![0, 0]
            } else {
                let mut prefix = integer(inserts % (2 * (max_capacity / 32)) + 1, 8, 0);
                prefix.push(0); // Base = Required Insert Count = inserts.
                prefix
            };
            for index in 0..model.len() {
                block.extend(integer(index as u64, 6, 0x80));
            }
            let expected: Vec<_> = model.iter().rev().map(|(n, v)| Header::new(n, v)).collect();
            assert_eq!(whole.decode(&block, size).unwrap(), expected);
            assert_eq!(split.decode(&block, size).unwrap(), expected);
            if size > 0 {
                assert_eq!(
                    whole.decode(&block, size - 1),
                    Err(Error::HeaderListTooLarge)
                );
            }
        }
    }
}

#[test]
fn seeded_malformed_streams_are_fragmentation_invariant_and_bounded() {
    let corpus = [
        vec![0x3f, 0x21, 0x41, b'x', 1, b'v'],
        vec![
            0x3f, 0x61, 0xc0, 0x8c, 0xf1, 0xe3, 0xc2, 0xe5, 0xf2, 0x3a, 0x6b, 0xa0, 0xab, 0x90,
            0xf4, 0xff,
        ],
        vec![
            0x3f, 0x21, 0x41, b'x', 0x7f, 0xff, 0xff, 0xff, 0xff, 0xff, 0xff, 0xff, 0xff, 0xff, 2,
        ],
    ];
    for seed in 1..=4096 {
        let mut rng = Rng(0xdead_9204_5eed ^ seed);
        let max_capacity = [0, 1, 31, 32, 33, 64, 128, 4096, u64::MAX][rng.below(9)];
        let mut whole = Decoder::with_capacity(max_capacity);
        let mut split = Decoder::with_capacity(max_capacity);
        let mut wire = if seed % 2 == 0 {
            corpus[rng.below(corpus.len())].clone()
        } else {
            let len = rng.below(257);
            rng.bytes(len)
        };
        if !wire.is_empty() {
            for _ in 0..rng.below(4) {
                let index = rng.below(wire.len());
                wire[index] ^= 1 << rng.below(8);
            }
        }
        let a = whole.control(&mut wire.clone());
        let b = fragmented(&mut split, &wire, &mut rng);
        assert_eq!(a, b, "seed={seed} wire={wire:02x?}");
        assert_eq!(whole.insert_count(), split.insert_count(), "seed={seed}");
        assert_eq!(
            whole.resource_usage(),
            split.resource_usage(),
            "seed={seed}"
        );
        assert_budget(&whole, max_capacity);
        assert_budget(&split, max_capacity);
        let len = rng.below(257);
        let mut block = rng.bytes(len);
        if block.len() >= 2 && seed % 3 == 0 {
            block[0] = 0;
            block[1] = 0;
        }
        let budget = rng.below(1025) as u64;
        let a = whole.decode(&block, budget);
        assert_eq!(
            a,
            split.decode(&block, budget),
            "seed={seed} block={block:02x?}"
        );
        if let Ok(headers) = a {
            let decoded: usize = headers
                .iter()
                .map(|h| 32 + h.name().len() + h.value().len())
                .sum();
            assert!(decoded as u64 <= budget);
        }
    }
}

#[test]
fn bounded_huffman_and_integer_mutations_never_exceed_field_budget() {
    // RFC 7541 Huffman "www.example.com"; both string bytes and length prefixes
    // are mutated, with repeated 0xff/EOS and overlong integer tails included.
    let valid = [
        0, 0, 0x50, 0x8c, 0xf1, 0xe3, 0xc2, 0xe5, 0xf2, 0x3a, 0x6b, 0xa0, 0xab, 0x90, 0xf4, 0xff,
    ];
    for position in 0..valid.len() {
        for byte in 0..=255u8 {
            let mut wire = valid.to_vec();
            wire[position] = byte;
            for budget in [0, 31, 32, 60, 64, 128] {
                let mut dec = Decoder::with_capacity(128);
                if let Ok(headers) = dec.decode(&wire, budget) {
                    assert!(
                        headers
                            .iter()
                            .map(|h| 32 + h.name().len() + h.value().len())
                            .sum::<usize>()
                            <= budget as usize
                    );
                }
                assert_eq!(dec.resource_usage(), (0, 0, 0, 0));
            }
        }
    }
    for len in 0..=32 {
        for byte in [0x80, 0xfe, 0xff] {
            let mut wire = vec![0xff];
            wire.extend(std::iter::repeat_n(byte, len));
            assert!(Decoder::new().decode(&wire, 1024).is_err());
        }
    }
}
