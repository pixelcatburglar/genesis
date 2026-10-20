//! `blue.marshal` — the binary object codec of the EVE build-1000 client.
//!
//! Reversed from blue.dll: reader `FUN_10028950`, writer `FUN_10029720`,
//! `Load` entry `FUN_10029390`, writer setup/finalise `FUN_10028550` /
//! `FUN_10028630`, share tracking `FUN_100287b0`. Full write-up in
//! docs/blue-marshal.md.
//!
//! Stream layout (all integers little-endian):
//!
//! ```text
//! u8   0x7E                 header; anything else is handed to cPickle.loads
//! u32  share count N
//! ...  one object
//! u32  share map[N]         slot (1..=N) of each 0x40-flagged object, in stream order
//! ```

mod value;

pub use value::Value;

use std::collections::HashMap;
use std::sync::OnceLock;

pub const HEADER: u8 = 0x7E;

/// Set on a container's tag byte when the object is referenced again later.
const FLAG_SHARED: u8 = 0x40;

pub mod tag {
    pub const NONE: u8 = 0x01;
    pub const BUILTIN_TYPE: u8 = 0x02;
    pub const LONG: u8 = 0x03;
    pub const INT32: u8 = 0x04;
    pub const INT16: u8 = 0x05;
    pub const INT8: u8 = 0x06;
    pub const INT_MINUS1: u8 = 0x07;
    pub const INT_0: u8 = 0x08;
    pub const INT_1: u8 = 0x09;
    pub const FLOAT: u8 = 0x0A;
    pub const FLOAT_0: u8 = 0x0B;
    // 0x0C is unassigned: the reader's default case ("Invalid type tag").
    pub const STRING_LONG: u8 = 0x0D;
    pub const STRING_EMPTY: u8 = 0x0E;
    pub const STRING_CHAR: u8 = 0x0F;
    pub const STRING_SHORT: u8 = 0x10;
    pub const STRING_TABLE: u8 = 0x11;
    pub const UNICODE: u8 = 0x12;
    pub const BUFFER: u8 = 0x13;
    pub const TUPLE: u8 = 0x14;
    pub const LIST: u8 = 0x15;
    pub const DICT: u8 = 0x16;
    pub const INSTANCE: u8 = 0x17;
    pub const BLUE_OBJECT: u8 = 0x18;
    pub const CALLBACK: u8 = 0x19;
    pub const PICKLE: u8 = 0x1A;
    pub const SHARED_REF: u8 = 0x1B;
    pub const CHECKSUM: u8 = 0x1C;
}

#[derive(Debug, thiserror::Error, PartialEq)]
pub enum Error {
    #[error("stream does not start with 0x7E (got {0:#04x}); cPickle streams are not supported")]
    NotMarshal(u8),
    #[error("unexpected end of stream at offset {0}")]
    Eof(usize),
    #[error("invalid type tag {tag:#04x} at offset {offset}")]
    InvalidTag { tag: u8, offset: usize },
    #[error("bogus data length: {len} bytes cannot hold a share map of {count} entries")]
    BogusLength { len: usize, count: usize },
    #[error("bogus map data in marshal stream")]
    BogusMap,
    #[error("string table index {0} out of range")]
    BadStringIndex(u8),
    #[error("shared reference {0} is unset or refers to an enclosing container")]
    BadSharedRef(u32),
    #[error("more objects are flagged shared than the share map holds")]
    ShareOverflow,
    #[error("trailing data after the object")]
    TrailingData,
}

/// The client's `script:/sys/strings.txt`: tag 0x11 indexes it from 1. blue.dll
/// splits on '\n' and drops the byte before it (the '\r' of a CRLF line); the
/// text after the final newline is the last entry.
static STRING_TABLE: &str = include_str!("../data/strings.txt");

fn string_table() -> &'static (Vec<&'static [u8]>, HashMap<&'static [u8], u8>) {
    static TABLE: OnceLock<(Vec<&'static [u8]>, HashMap<&'static [u8], u8>)> = OnceLock::new();
    TABLE.get_or_init(|| {
        let mut entries = Vec::new();
        let mut rest = STRING_TABLE.as_bytes();
        loop {
            match rest.iter().position(|&b| b == b'\n') {
                Some(nl) => {
                    entries.push(&rest[..nl.saturating_sub(1)]);
                    rest = &rest[nl + 1..];
                }
                None => {
                    entries.push(rest);
                    break;
                }
            }
        }
        let index = entries
            .iter()
            .enumerate()
            .filter(|(i, _)| *i < 255)
            .map(|(i, s)| (*s, (i + 1) as u8))
            .collect();
        (entries, index)
    })
}

/// The string-table entries, index 1 first.
pub fn string_table_entries() -> &'static [&'static [u8]] {
    &string_table().0
}

// ---------------------------------------------------------------- decoding

struct Reader<'a> {
    data: &'a [u8],
    pos: usize,
    map: Vec<u32>,
    next_share: usize,
    shared: Vec<Option<Value>>,
}

impl<'a> Reader<'a> {
    fn take(&mut self, n: usize) -> Result<&'a [u8], Error> {
        let end = self.pos.checked_add(n).ok_or(Error::Eof(self.pos))?;
        let s = self.data.get(self.pos..end).ok_or(Error::Eof(self.pos))?;
        self.pos = end;
        Ok(s)
    }

    fn u8(&mut self) -> Result<u8, Error> {
        Ok(self.take(1)?[0])
    }

    fn i16(&mut self) -> Result<i16, Error> {
        Ok(i16::from_le_bytes(self.take(2)?.try_into().unwrap()))
    }

    fn i32(&mut self) -> Result<i32, Error> {
        Ok(i32::from_le_bytes(self.take(4)?.try_into().unwrap()))
    }

    fn i64(&mut self) -> Result<i64, Error> {
        Ok(i64::from_le_bytes(self.take(8)?.try_into().unwrap()))
    }

    fn f64(&mut self) -> Result<f64, Error> {
        Ok(f64::from_le_bytes(self.take(8)?.try_into().unwrap()))
    }

    /// `FUN_100288d0`: a u8, or 0xFF followed by a u32.
    fn len(&mut self) -> Result<usize, Error> {
        match self.u8()? {
            0xFF => Ok(self.i32()? as u32 as usize),
            n => Ok(n as usize),
        }
    }

    /// Claims the next share-map slot. Containers do this *before* reading
    /// their children, so the order matches the writer's stream order.
    fn reserve(&mut self, flagged: bool) -> Result<Option<usize>, Error> {
        if !flagged {
            return Ok(None);
        }
        let slot = *self.map.get(self.next_share).ok_or(Error::ShareOverflow)? as usize;
        self.next_share += 1;
        Ok(Some(slot - 1))
    }

    fn fill(&mut self, slot: Option<usize>, v: &Value) {
        if let Some(s) = slot {
            self.shared[s] = Some(v.clone());
        }
    }

    fn object(&mut self) -> Result<Value, Error> {
        let offset = self.pos;
        let raw = self.u8()?;
        let flagged = raw & FLAG_SHARED != 0;
        Ok(match raw & 0x3F {
            tag::NONE => Value::None,
            tag::BUILTIN_TYPE => {
                let n = self.len()?;
                Value::BuiltinType(self.take(n)?.to_vec())
            }
            tag::LONG => Value::Long(self.i64()?),
            tag::INT32 => Value::Int(self.i32()?),
            tag::INT16 => Value::Int(self.i16()? as i32),
            tag::INT8 => Value::Int(self.u8()? as i8 as i32),
            tag::INT_MINUS1 => Value::Int(-1),
            tag::INT_0 => Value::Int(0),
            tag::INT_1 => Value::Int(1),
            tag::FLOAT => Value::Float(self.f64()?),
            tag::FLOAT_0 => Value::Float(0.0),
            tag::STRING_LONG => {
                let n = self.len()?;
                Value::Str(self.take(n)?.to_vec())
            }
            tag::STRING_EMPTY => Value::Str(Vec::new()),
            tag::STRING_CHAR => Value::Str(vec![self.u8()?]),
            tag::STRING_SHORT => {
                let n = self.u8()? as usize;
                Value::Str(self.take(n)?.to_vec())
            }
            tag::STRING_TABLE => {
                // blue.dll yields "" for an unknown index rather than failing.
                let i = self.u8()?;
                let entries = string_table_entries();
                match (i as usize).checked_sub(1).and_then(|i| entries.get(i)) {
                    Some(s) => Value::Str(s.to_vec()),
                    None => Value::Str(Vec::new()),
                }
            }
            tag::UNICODE => {
                let n = self.len()?;
                let units: Vec<u16> = self
                    .take(n / 2 * 2)?
                    .chunks_exact(2)
                    .map(|c| u16::from_le_bytes([c[0], c[1]]))
                    .collect();
                Value::Unicode(String::from_utf16_lossy(&units))
            }
            tag::BUFFER => {
                let n = self.len()?;
                let v = Value::Str(self.take(n)?.to_vec());
                let slot = self.reserve(flagged)?;
                self.fill(slot, &v);
                v
            }
            tag::TUPLE | tag::LIST => {
                let n = self.len()?;
                let slot = self.reserve(flagged)?;
                let mut items = Vec::with_capacity(n.min(4096));
                for _ in 0..n {
                    items.push(self.object()?);
                }
                let v = if raw & 0x3F == tag::TUPLE {
                    Value::Tuple(items)
                } else {
                    Value::List(items)
                };
                self.fill(slot, &v);
                v
            }
            tag::DICT => {
                let n = self.len()?;
                let slot = self.reserve(flagged)?;
                let mut pairs = Vec::with_capacity(n.min(4096));
                for _ in 0..n {
                    // Value first, then key.
                    let value = self.object()?;
                    let key = self.object()?;
                    pairs.push((key, value));
                }
                let v = Value::Dict(pairs);
                self.fill(slot, &v);
                v
            }
            tag::INSTANCE => {
                let guid = self.object()?;
                let state = self.object()?;
                let v = Value::Instance {
                    guid: Box::new(guid),
                    state: Box::new(state),
                };
                let slot = self.reserve(flagged)?;
                self.fill(slot, &v);
                v
            }
            tag::BLUE_OBJECT => {
                let n = self.len()?;
                let v = Value::BlueObject(self.take(n)?.to_vec());
                let slot = self.reserve(flagged)?;
                self.fill(slot, &v);
                v
            }
            tag::CALLBACK => Value::Custom(Box::new(self.object()?)),
            tag::PICKLE => {
                let n = self.len()?;
                Value::Pickle(self.take(n)?.to_vec())
            }
            tag::SHARED_REF => {
                let i = self.len()? as u32;
                (i as usize)
                    .checked_sub(1)
                    .and_then(|s| self.shared.get(s).cloned().flatten())
                    .ok_or(Error::BadSharedRef(i))?
            }
            // The checksum is never enabled by the client (packetCRC32 is
            // a separate machoNet framing option); skip it and read on.
            tag::CHECKSUM => {
                self.i32()?;
                self.object()?
            }
            t => return Err(Error::InvalidTag { tag: t, offset }),
        })
    }
}

/// Decodes a complete marshal stream (`blue.marshal.Load`).
pub fn decode(data: &[u8]) -> Result<Value, Error> {
    let header = *data.first().ok_or(Error::Eof(0))?;
    if header != HEADER {
        return Err(Error::NotMarshal(header));
    }
    let count = i32::from_le_bytes(data.get(1..5).ok_or(Error::Eof(1))?.try_into().unwrap());
    let count = count.max(0) as usize;
    let map_len = count * 4;
    if count > 0 && data.len() < map_len + 5 {
        return Err(Error::BogusLength {
            len: data.len(),
            count,
        });
    }
    let body_end = data.len() - map_len;
    let map: Vec<u32> = data[body_end..]
        .chunks_exact(4)
        .map(|c| u32::from_le_bytes(c.try_into().unwrap()))
        .collect();
    if map.iter().any(|&s| s < 1 || s as usize > count) {
        return Err(Error::BogusMap);
    }
    let mut r = Reader {
        data: &data[..body_end],
        pos: 5,
        map,
        next_share: 0,
        shared: vec![None; count],
    };
    let v = r.object()?;
    if r.pos != body_end {
        return Err(Error::TrailingData);
    }
    Ok(v)
}

// ---------------------------------------------------------------- encoding

/// Encodes a value as a marshal stream (`blue.marshal.Save`).
///
/// The server never needs object sharing, so the share count is always 0.
pub fn encode(v: &Value) -> Vec<u8> {
    let mut out = vec![HEADER, 0, 0, 0, 0];
    write(&mut out, v);
    out
}

/// `FUN_10028740`.
fn write_len(out: &mut Vec<u8>, n: usize) {
    if n < 0xFF {
        out.push(n as u8);
    } else {
        out.push(0xFF);
        out.extend_from_slice(&(n as u32).to_le_bytes());
    }
}

fn write_int(out: &mut Vec<u8>, i: i32) {
    match i {
        -1 => out.push(tag::INT_MINUS1),
        0 => out.push(tag::INT_0),
        1 => out.push(tag::INT_1),
        -128..=127 => {
            out.push(tag::INT8);
            out.push(i as i8 as u8);
        }
        -32768..=32767 => {
            out.push(tag::INT16);
            out.extend_from_slice(&(i as i16).to_le_bytes());
        }
        _ => {
            out.push(tag::INT32);
            out.extend_from_slice(&i.to_le_bytes());
        }
    }
}

/// Mirrors the writer's string cases, string table first.
fn write_str(out: &mut Vec<u8>, s: &[u8]) {
    if let Some(&i) = string_table().1.get(s) {
        out.push(tag::STRING_TABLE);
        out.push(i);
        return;
    }
    match s.len() {
        0 => out.push(tag::STRING_EMPTY),
        1 => {
            out.push(tag::STRING_CHAR);
            out.push(s[0]);
        }
        n if n < 0x100 => {
            out.push(tag::STRING_SHORT);
            out.push(n as u8);
            out.extend_from_slice(s);
        }
        n => {
            out.push(tag::STRING_LONG);
            write_len(out, n);
            out.extend_from_slice(s);
        }
    }
}

fn write(out: &mut Vec<u8>, v: &Value) {
    match v {
        Value::None => out.push(tag::NONE),
        Value::Int(i) => write_int(out, *i),
        Value::Long(i) => {
            out.push(tag::LONG);
            out.extend_from_slice(&i.to_le_bytes());
        }
        Value::Float(f) => {
            if *f == 0.0 {
                out.push(tag::FLOAT_0);
            } else {
                out.push(tag::FLOAT);
                out.extend_from_slice(&f.to_le_bytes());
            }
        }
        Value::Str(s) => write_str(out, s),
        Value::Unicode(s) => {
            let units: Vec<u16> = s.encode_utf16().collect();
            out.push(tag::UNICODE);
            write_len(out, units.len() * 2);
            for u in units {
                out.extend_from_slice(&u.to_le_bytes());
            }
        }
        Value::Buffer(b) => {
            out.push(tag::BUFFER);
            write_len(out, b.len());
            out.extend_from_slice(b);
        }
        Value::Tuple(items) | Value::List(items) => {
            out.push(if matches!(v, Value::Tuple(_)) {
                tag::TUPLE
            } else {
                tag::LIST
            });
            write_len(out, items.len());
            for item in items {
                write(out, item);
            }
        }
        Value::Dict(pairs) => {
            out.push(tag::DICT);
            write_len(out, pairs.len());
            for (k, val) in pairs {
                write(out, val);
                write(out, k);
            }
        }
        Value::Instance { guid, state } => {
            out.push(tag::INSTANCE);
            write(out, guid);
            write(out, state);
        }
        Value::BuiltinType(name) => {
            out.push(tag::BUILTIN_TYPE);
            write_len(out, name.len());
            out.extend_from_slice(name);
        }
        Value::BlueObject(b) => {
            out.push(tag::BLUE_OBJECT);
            write_len(out, b.len());
            out.extend_from_slice(b);
        }
        Value::Custom(inner) => {
            out.push(tag::CALLBACK);
            write(out, inner);
        }
        Value::Pickle(p) => {
            out.push(tag::PICKLE);
            write_len(out, p.len());
            out.extend_from_slice(p);
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn roundtrip(v: Value) {
        assert_eq!(decode(&encode(&v)).unwrap(), v);
    }

    #[test]
    fn string_table_matches_client_file() {
        let t = string_table_entries();
        assert_eq!(t.len(), 81);
        assert_eq!(t[0], b"*corpid");
        assert_eq!(t[80], b"util.Rowset");
        assert!(t.iter().all(|s| !s.contains(&b'\r')));
    }

    #[test]
    fn int_tags_follow_writer_ranges() {
        assert_eq!(encode(&Value::Int(-1))[5..], [0x07]);
        assert_eq!(encode(&Value::Int(0))[5..], [0x08]);
        assert_eq!(encode(&Value::Int(1))[5..], [0x09]);
        assert_eq!(encode(&Value::Int(-128))[5..], [0x06, 0x80]);
        assert_eq!(encode(&Value::Int(128))[5..], [0x05, 0x80, 0x00]);
        assert_eq!(encode(&Value::Int(40000))[5..], [0x04, 0x40, 0x9c, 0, 0]);
    }

    #[test]
    fn strings_use_table_then_length_classes() {
        assert_eq!(encode(&Value::str("util.Rowset"))[5..], [0x11, 81]);
        assert_eq!(encode(&Value::str(""))[5..], [0x0e]);
        assert_eq!(encode(&Value::str("x"))[5..], [0x0f, b'x']);
        assert_eq!(encode(&Value::str("xy"))[5..], [0x10, 2, b'x', b'y']);
        let long = vec![b'a'; 300];
        assert_eq!(
            encode(&Value::Str(long.clone()))[5..10],
            [0x0d, 0xff, 0x2c, 0x01, 0x00]
        );
        roundtrip(Value::Str(long));
    }

    #[test]
    fn dict_writes_value_before_key() {
        let v = Value::dict([(Value::str("k"), Value::Int(5))]);
        assert_eq!(encode(&v)[5..], [0x16, 1, 0x06, 5, 0x0f, b'k']);
        roundtrip(v);
    }

    #[test]
    fn roundtrips() {
        roundtrip(Value::tuple([
            Value::None,
            Value::Long(-1 << 40),
            Value::Float(1.5),
            Value::Float(0.0),
            Value::Unicode("Ægir".into()),
            Value::list([Value::Int(70000), Value::str("abc")]),
            Value::instance("macho.CallReq", Value::tuple([Value::Int(6)])),
            Value::BuiltinType(b"int".to_vec()),
        ]));
    }

    /// A stream as the client's writer produces it when one tuple appears twice:
    /// the first copy's tag carries 0x40, the second is `1B 01`, and the map
    /// holds slot 1.
    #[test]
    fn decodes_shared_objects() {
        let shared = [0x14 | 0x40, 1, 0x09];
        let mut s = vec![0x7e, 1, 0, 0, 0, 0x14, 2];
        s.extend_from_slice(&shared);
        s.extend_from_slice(&[0x1b, 1]);
        s.extend_from_slice(&1u32.to_le_bytes());
        let inner = Value::tuple([Value::Int(1)]);
        assert_eq!(decode(&s).unwrap(), Value::tuple([inner.clone(), inner]));
    }

    #[test]
    fn rejects_bad_streams() {
        assert_eq!(decode(&[0x80, 2]), Err(Error::NotMarshal(0x80)));
        assert_eq!(
            decode(&[0x7e, 0, 0, 0, 0, 0x0c]),
            Err(Error::InvalidTag {
                tag: 0x0c,
                offset: 5
            })
        );
        assert_eq!(
            decode(&[0x7e, 1, 0, 0, 0, 0x01, 5, 0, 0, 0]),
            Err(Error::BogusMap)
        );
    }
}
