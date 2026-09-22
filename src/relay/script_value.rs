// Copyright (c) 2026 Viktor Semenov
// SPDX-License-Identifier: Apache-2.0

// Decoder for the binary structured-clone format described in PROTOCOL.md
// Sec. 5. It is used only to read GAME_SETTINGS for lobby metadata (A6):
// the server never needs to produce these bytes, so there is no encoder.
// All integers are little-endian.

use std::collections::HashMap;

const TAG_VOID: u8 = 0x00;
const TAG_NULL: u8 = 0x01;
const TAG_ARRAY: u8 = 0x02;
const TAG_OBJECT: u8 = 0x03;
const TAG_STRING: u8 = 0x04;
const TAG_INT: u8 = 0x05;
const TAG_DOUBLE: u8 = 0x06;
const TAG_BOOLEAN: u8 = 0x07;
const TAG_PRIOR_OBJECT: u8 = 0x08;
const TAG_TYPED_ARRAY: u8 = 0x09;
const TAG_ARRAY_BUFFER: u8 = 0x0a;
const TAG_OBJECT_PROTOTYPE: u8 = 0x0b;
const TAG_OBJECT_NUMBER: u8 = 0x0c;
const TAG_OBJECT_STRING: u8 = 0x0d;
const TAG_OBJECT_BOOLEAN: u8 = 0x0e;
const TAG_OBJECT_MAP: u8 = 0x0f;
const TAG_OBJECT_SET: u8 = 0x10;

// The wire gives PRIOR_OBJECT's payload only as "number", with no width
// (PROTOCOL.md Sec. 5). u32 LE matches every other length field in the
// format and is what the existing skipper in messages/player_command.rs
// already assumes, so it is not a fresh guess.
//
// Nesting deep enough to matter here would mean a hostile or corrupt
// packet, since real game settings never nest this deep.
const MAX_DEPTH: usize = 64;
// Bounds the total number and byte size of every value produced while
// decoding, so a small packet of PRIOR_OBJECT backrefs to a large
// subtree cannot clone its way to unbounded memory: resolving a backref
// is charged the same as decoding its target fresh, and so is the copy
// kept in `refs` for later backrefs to name.
const MAX_NODES: usize = 100_000;
// Same budget, counted in bytes of string and ArrayBuffer payload, so a
// backref chain to one large string cannot stay under MAX_NODES while
// still cloning unbounded memory.
const MAX_BYTES: usize = 1 << 20;

#[derive(Debug, thiserror::Error, PartialEq)]
pub enum ScriptValueError {
    #[error("buffer too short")]
    Truncated,
    #[error("unknown tag {0:#04x}")]
    UnknownTag(u8),
    #[error("tag {0:#04x} is not decoded (PROTOCOL.md Sec. 5 leaves its exact layout ambiguous)")]
    Unsupported(u8),
    #[error("boolean payload must be 0 or 1, got {0}")]
    BadBoolean(u8),
    #[error("backref {0} does not name an already-decoded object")]
    BadBackref(u32),
    #[error("nesting exceeds the depth limit")]
    TooDeep,
    #[error("decoded value exceeds the size limit")]
    TooLarge,
    #[error("{0} trailing bytes after the root value")]
    TrailingBytes(usize),
}

// One decoded structured-clone value. Property order is kept because it is
// part of the wire format (Sec. 5's "property order" note), even though
// nothing here currently depends on it.
#[derive(Debug, Clone, PartialEq)]
pub enum ScriptValue {
    Undefined,
    Null,
    Bool(bool),
    Int(i32),
    Double(f64),
    String(String),
    Array {
        length: u32,
        props: Vec<(String, ScriptValue)>,
    },
    Object(Vec<(String, ScriptValue)>),
    NumberObject(f64),
    StringObject(String),
    BooleanObject(bool),
    Map(Vec<(ScriptValue, ScriptValue)>),
    Set(Vec<ScriptValue>),
    TypedArray {
        element_type: u8,
        offset: u32,
        length: u32,
        buffer: Box<ScriptValue>,
    },
    ArrayBuffer(Vec<u8>),
}

impl ScriptValue {
    // Object and Array both carry a props list; this is the common lookup
    // the lobby-attribute extraction needs for either.
    pub fn get(&self, key: &str) -> Option<&ScriptValue> {
        match self {
            ScriptValue::Object(props) | ScriptValue::Array { props, .. } => {
                props.iter().find(|(k, _)| k == key).map(|(_, v)| v)
            }
            _ => None,
        }
    }

    pub fn as_str(&self) -> Option<&str> {
        match self {
            ScriptValue::String(s) | ScriptValue::StringObject(s) => Some(s),
            _ => None,
        }
    }

    // A number that round-trips through INT is encoded as INT (Sec. 5), so
    // a caller that just wants "the number" should not have to match twice.
    pub fn as_number(&self) -> Option<f64> {
        match self {
            ScriptValue::Int(n) => Some(*n as f64),
            ScriptValue::Double(n) | ScriptValue::NumberObject(n) => Some(*n),
            _ => None,
        }
    }

    // Array elements are properties named by decimal index (Sec. 5), so
    // "the array's elements in order" means walking 0..length and looking
    // each one up by its string name.
    pub fn array_len(&self) -> Option<u32> {
        match self {
            ScriptValue::Array { length, .. } => Some(*length),
            _ => None,
        }
    }

    pub fn array_get(&self, index: u32) -> Option<&ScriptValue> {
        self.get(&index.to_string())
    }
}

// What a decoded value cost to produce, in the two units MAX_NODES and
// MAX_BYTES budget separately: a doubling chain of small containers is
// caught by the node count, a chain of backrefs to one large string is
// caught by the byte count.
#[derive(Debug, Clone, Copy, Default)]
struct Cost {
    nodes: usize,
    bytes: usize,
}

impl Cost {
    fn add(self, other: Cost) -> Cost {
        Cost {
            nodes: self.nodes + other.nodes,
            bytes: self.bytes + other.bytes,
        }
    }

    // The cost of what was decoded between two `tree` readings, taken
    // before and after a container's payload, so a later backref to that
    // container can be charged as if it were decoded fresh again.
    fn since(self, start: Cost) -> Cost {
        Cost {
            nodes: self.nodes - start.nodes,
            bytes: self.bytes - start.bytes,
        }
    }
}

struct Decoder<'a> {
    data: &'a [u8],
    pos: usize,
    // Reference numbers are assigned from 1, in pre-order, to every tag that
    // can be named again later (Sec. 5). A slot is None while that value's
    // own payload is still being read, which is how a self-referential
    // cycle is told apart from a legitimate forward reference. A finished
    // slot also keeps the cost it was charged when decoded, so resolving a
    // PRIOR_OBJECT to it is charged that same cost again rather than for
    // free.
    refs: HashMap<u32, Option<(ScriptValue, Cost)>>,
    next_ref: u32,
    // The cost of the value tree currently being built; only read via
    // `since` to measure one container's own subtree.
    tree: Cost,
    // Every cost ever charged, including backref clones and refs-table
    // copies. This is what MAX_NODES and MAX_BYTES bound.
    spent: Cost,
}

impl<'a> Decoder<'a> {
    fn new(data: &'a [u8]) -> Self {
        Decoder {
            data,
            pos: 0,
            refs: HashMap::new(),
            next_ref: 1,
            tree: Cost::default(),
            spent: Cost::default(),
        }
    }

    // Charges `cost` against both the current subtree and the total spend,
    // so it counts toward an enclosing container's own subtree cost. Used
    // for freshly decoded values and for backref clones, which become part
    // of the tree they are read into.
    fn charge(&mut self, cost: Cost) -> Result<(), ScriptValueError> {
        self.tree = self.tree.add(cost);
        self.charge_copy(cost)
    }

    // Charges `cost` against the total spend only, for memory that is real
    // but not part of the value being built right now: the copy `fill_ref`
    // keeps in `refs` for a future backref.
    fn charge_copy(&mut self, cost: Cost) -> Result<(), ScriptValueError> {
        self.spent = self.spent.add(cost);
        if self.spent.nodes > MAX_NODES || self.spent.bytes > MAX_BYTES {
            return Err(ScriptValueError::TooLarge);
        }
        Ok(())
    }

    fn take_node(&mut self) -> Result<(), ScriptValueError> {
        self.charge(Cost { nodes: 1, bytes: 0 })
    }

    fn read_u8(&mut self) -> Result<u8, ScriptValueError> {
        let b = *self.data.get(self.pos).ok_or(ScriptValueError::Truncated)?;
        self.pos += 1;
        Ok(b)
    }

    fn read_u32(&mut self) -> Result<u32, ScriptValueError> {
        let end = self.pos.checked_add(4).ok_or(ScriptValueError::Truncated)?;
        let bytes = self
            .data
            .get(self.pos..end)
            .ok_or(ScriptValueError::Truncated)?;
        self.pos = end;
        Ok(u32::from_le_bytes(bytes.try_into().unwrap()))
    }

    fn read_f64(&mut self) -> Result<f64, ScriptValueError> {
        let end = self.pos.checked_add(8).ok_or(ScriptValueError::Truncated)?;
        let bytes = self
            .data
            .get(self.pos..end)
            .ok_or(ScriptValueError::Truncated)?;
        self.pos = end;
        Ok(f64::from_le_bytes(bytes.try_into().unwrap()))
    }

    fn read_bytes(&mut self, len: usize) -> Result<&'a [u8], ScriptValueError> {
        let end = self
            .pos
            .checked_add(len)
            .ok_or(ScriptValueError::Truncated)?;
        let bytes = self
            .data
            .get(self.pos..end)
            .ok_or(ScriptValueError::Truncated)?;
        self.pos = end;
        Ok(bytes)
    }

    // string_value := encoding_flag:u8(0|1) length:u32 chars (Sec. 5).
    // Flag 1 is Latin-1; flag 0 is UTF-16LE. Lone surrogates cannot survive
    // into a Rust String, so a UTF-16 string with one decodes lossily rather
    // than failing the whole settings blob over a display-only field.
    fn read_string(&mut self) -> Result<String, ScriptValueError> {
        let latin1 = self.read_u8()?;
        let len = self.read_u32()? as usize;
        if latin1 != 0 {
            let bytes = self.read_bytes(len)?;
            self.charge(Cost {
                nodes: 0,
                bytes: len,
            })?;
            Ok(bytes.iter().map(|&b| b as char).collect())
        } else {
            let byte_len = len.checked_mul(2).ok_or(ScriptValueError::Truncated)?;
            let bytes = self.read_bytes(byte_len)?;
            self.charge(Cost {
                nodes: 0,
                bytes: byte_len,
            })?;
            let units: Vec<u16> = bytes
                .chunks_exact(2)
                .map(|c| u16::from_le_bytes([c[0], c[1]]))
                .collect();
            Ok(String::from_utf16_lossy(&units))
        }
    }

    // props := count:u32 (property_key:string_value property_value:value){count}
    fn read_props(&mut self, depth: usize) -> Result<Vec<(String, ScriptValue)>, ScriptValueError> {
        let count = self.read_u32()?;
        let mut props = Vec::new();
        for _ in 0..count {
            let key = self.read_string()?;
            let value = self.read_value(depth)?;
            props.push((key, value));
        }
        Ok(props)
    }

    fn read_value(&mut self, depth: usize) -> Result<ScriptValue, ScriptValueError> {
        if depth > MAX_DEPTH {
            return Err(ScriptValueError::TooDeep);
        }
        // Taken before this node's own charge, so a container arm below
        // can measure its whole subtree, itself included, with `since`.
        let start = self.tree;
        self.take_node()?;
        let tag = self.read_u8()?;
        match tag {
            TAG_VOID => Ok(ScriptValue::Undefined),
            TAG_NULL => Ok(ScriptValue::Null),
            TAG_BOOLEAN => {
                let b = self.read_u8()?;
                match b {
                    0 => Ok(ScriptValue::Bool(false)),
                    1 => Ok(ScriptValue::Bool(true)),
                    other => Err(ScriptValueError::BadBoolean(other)),
                }
            }
            TAG_OBJECT_BOOLEAN => {
                let b = self.read_u8()?;
                match b {
                    0 => Ok(ScriptValue::BooleanObject(false)),
                    1 => Ok(ScriptValue::BooleanObject(true)),
                    other => Err(ScriptValueError::BadBoolean(other)),
                }
            }
            TAG_INT => Ok(ScriptValue::Int(self.read_u32()? as i32)),
            TAG_PRIOR_OBJECT => {
                let n = self.read_u32()?;
                // Some(None) is a reference to a value still being decoded:
                // a cycle, which structured clone cannot produce for these
                // tags, so it is treated the same as an out-of-range number.
                let cost = match self.refs.get(&n) {
                    Some(Some((_, cost))) => *cost,
                    Some(None) | None => return Err(ScriptValueError::BadBackref(n)),
                };
                // Charged, and found to fit the budget, before the clone
                // below runs: a hostile chain of backrefs to one large
                // subtree must not get even one more free clone of it.
                self.charge(cost)?;
                match self.refs.get(&n) {
                    Some(Some((value, _))) => Ok(value.clone()),
                    _ => unreachable!("checked above; refs is only ever appended to"),
                }
            }
            TAG_DOUBLE => Ok(ScriptValue::Double(self.read_f64()?)),
            TAG_OBJECT_NUMBER => Ok(ScriptValue::NumberObject(self.read_f64()?)),
            TAG_STRING => Ok(ScriptValue::String(self.read_string()?)),
            TAG_OBJECT_STRING => Ok(ScriptValue::StringObject(self.read_string()?)),
            TAG_ARRAY => {
                let reference = self.reserve_ref();
                let length = self.read_u32()?;
                let props = self.read_props(depth + 1)?;
                let value = ScriptValue::Array { length, props };
                let cost = self.tree.since(start);
                self.fill_ref(reference, &value, cost)?;
                Ok(value)
            }
            TAG_OBJECT => {
                let reference = self.reserve_ref();
                let props = self.read_props(depth + 1)?;
                let value = ScriptValue::Object(props);
                let cost = self.tree.since(start);
                self.fill_ref(reference, &value, cost)?;
                Ok(value)
            }
            TAG_TYPED_ARRAY => {
                // The view is numbered before its buffer (Sec. 5), so the
                // reference has to be reserved before decoding the buffer.
                let reference = self.reserve_ref();
                let element_type = self.read_u8()?;
                let offset = self.read_u32()?;
                let length = self.read_u32()?;
                let buffer = self.read_value(depth + 1)?;
                let value = ScriptValue::TypedArray {
                    element_type,
                    offset,
                    length,
                    buffer: Box::new(buffer),
                };
                let cost = self.tree.since(start);
                self.fill_ref(reference, &value, cost)?;
                Ok(value)
            }
            TAG_ARRAY_BUFFER => {
                let reference = self.reserve_ref();
                let len = self.read_u32()? as usize;
                let bytes = self.read_bytes(len)?;
                self.charge(Cost {
                    nodes: 0,
                    bytes: len,
                })?;
                let value = ScriptValue::ArrayBuffer(bytes.to_vec());
                let cost = self.tree.since(start);
                self.fill_ref(reference, &value, cost)?;
                Ok(value)
            }
            TAG_OBJECT_MAP => {
                let reference = self.reserve_ref();
                let count = self.read_u32()?;
                let mut entries = Vec::new();
                for _ in 0..count {
                    let key = self.read_value(depth + 1)?;
                    let val = self.read_value(depth + 1)?;
                    entries.push((key, val));
                }
                let value = ScriptValue::Map(entries);
                let cost = self.tree.since(start);
                self.fill_ref(reference, &value, cost)?;
                Ok(value)
            }
            TAG_OBJECT_SET => {
                let reference = self.reserve_ref();
                let count = self.read_u32()?;
                let mut entries = Vec::new();
                for _ in 0..count {
                    entries.push(self.read_value(depth + 1)?);
                }
                let value = ScriptValue::Set(entries);
                let cost = self.tree.since(start);
                self.fill_ref(reference, &value, cost)?;
                Ok(value)
            }
            // Nothing on the wire says which of the three OBJECT_PROTOTYPE
            // payload variants follows (Sec. 5), and GAME_SETTINGS never
            // needs a prototype object, so this is refused rather than
            // guessed (R7).
            TAG_OBJECT_PROTOTYPE => Err(ScriptValueError::Unsupported(TAG_OBJECT_PROTOTYPE)),
            other => Err(ScriptValueError::UnknownTag(other)),
        }
    }

    fn reserve_ref(&mut self) -> u32 {
        let n = self.next_ref;
        self.next_ref += 1;
        self.refs.insert(n, None);
        n
    }

    // Keeping this copy for a future PRIOR_OBJECT is real memory, so it is
    // charged before it is made; a subtree already at the budget's edge
    // must not get a second copy for free.
    fn fill_ref(
        &mut self,
        reference: u32,
        value: &ScriptValue,
        cost: Cost,
    ) -> Result<(), ScriptValueError> {
        self.charge_copy(cost)?;
        self.refs.insert(reference, Some((value.clone(), cost)));
        Ok(())
    }
}

pub fn decode(bytes: &[u8]) -> Result<ScriptValue, ScriptValueError> {
    let mut decoder = Decoder::new(bytes);
    let value = decoder.read_value(0)?;
    let remaining = decoder.data.len() - decoder.pos;
    if remaining != 0 {
        return Err(ScriptValueError::TrailingBytes(remaining));
    }
    Ok(value)
}

#[cfg(test)]
#[path = "../../tests/unit/relay/script_value.rs"]
mod tests;
