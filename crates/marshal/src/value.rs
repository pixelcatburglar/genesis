//! The Python object model as it crosses the wire.

/// A decoded (or to-be-encoded) Python 2.2 value.
///
/// Python 2.2 has no bool type: `True`/`False` are the ints 1/0, which is why
/// there is no `Bool` variant.
#[derive(Debug, Clone, PartialEq, Default)]
pub enum Value {
    #[default]
    None,
    Int(i32),
    /// Python `long`, carried as a signed 64-bit integer (tag 0x03).
    Long(i64),
    Float(f64),
    /// Python `str`: arbitrary bytes, not necessarily UTF-8.
    Str(Vec<u8>),
    Unicode(String),
    /// A `buffer` object; it decodes back as a plain `str`.
    Buffer(Vec<u8>),
    Tuple(Vec<Value>),
    List(Vec<Value>),
    /// Insertion-ordered key/value pairs.
    Dict(Vec<(Value, Value)>),
    /// An old-style class instance: `guid` is `"module.Class"` and `state` is
    /// whatever `__getstate__` returned (or the instance `__dict__`).
    Instance {
        guid: Box<Value>,
        state: Box<Value>,
    },
    /// A builtin type such as `int`, sent by name (tag 0x02).
    BuiltinType(Vec<u8>),
    /// A serialized blue object image (tag 0x18), kept opaque.
    BlueObject(Vec<u8>),
    /// A value produced by a custom marshal callback (tag 0x19).
    Custom(Box<Value>),
    /// A cPickle stream for objects the native marshaller cannot express (tag 0x1A).
    Pickle(Vec<u8>),
}

impl Value {
    pub fn str(s: impl AsRef<[u8]>) -> Value {
        Value::Str(s.as_ref().to_vec())
    }

    pub fn tuple(items: impl IntoIterator<Item = Value>) -> Value {
        Value::Tuple(items.into_iter().collect())
    }

    pub fn list(items: impl IntoIterator<Item = Value>) -> Value {
        Value::List(items.into_iter().collect())
    }

    pub fn dict(pairs: impl IntoIterator<Item = (Value, Value)>) -> Value {
        Value::Dict(pairs.into_iter().collect())
    }

    pub fn instance(guid: &str, state: Value) -> Value {
        Value::Instance {
            guid: Box::new(Value::str(guid)),
            state: Box::new(state),
        }
    }

    pub fn as_int(&self) -> Option<i64> {
        match self {
            Value::Int(i) => Some(*i as i64),
            Value::Long(i) => Some(*i),
            _ => None,
        }
    }

    /// A number as float (Python's int/long/float are interchangeable here).
    pub fn as_float(&self) -> Option<f64> {
        match self {
            Value::Int(i) => Some(*i as f64),
            Value::Long(i) => Some(*i as f64),
            Value::Float(f) => Some(*f),
            _ => None,
        }
    }

    /// Text as the client meant it: a `str` is Latin-1 bytes, a `unicode` is text.
    pub fn as_text(&self) -> Option<String> {
        match self {
            Value::Str(b) | Value::Buffer(b) => Some(b.iter().map(|&c| c as char).collect()),
            Value::Unicode(s) => Some(s.clone()),
            _ => None,
        }
    }

    /// The bytes of a `str`/`buffer`, or the UTF-8 of a `unicode`.
    pub fn as_bytes(&self) -> Option<&[u8]> {
        match self {
            Value::Str(b) | Value::Buffer(b) => Some(b),
            Value::Unicode(s) => Some(s.as_bytes()),
            _ => None,
        }
    }

    pub fn as_str(&self) -> Option<&str> {
        self.as_bytes().and_then(|b| std::str::from_utf8(b).ok())
    }

    pub fn as_seq(&self) -> Option<&[Value]> {
        match self {
            Value::Tuple(v) | Value::List(v) => Some(v),
            _ => None,
        }
    }

    pub fn is_none(&self) -> bool {
        matches!(self, Value::None)
    }

    /// Looks a string key up in a dict.
    pub fn get(&self, key: &str) -> Option<&Value> {
        match self {
            Value::Dict(pairs) => pairs
                .iter()
                .find(|(k, _)| k.as_bytes() == Some(key.as_bytes()))
                .map(|(_, v)| v),
            _ => None,
        }
    }
}

impl From<i32> for Value {
    fn from(v: i32) -> Self {
        Value::Int(v)
    }
}

impl From<i64> for Value {
    /// Values that fit an `int` stay `int`, as Python 2.2 would produce them.
    fn from(v: i64) -> Self {
        i32::try_from(v).map(Value::Int).unwrap_or(Value::Long(v))
    }
}

impl From<f64> for Value {
    fn from(v: f64) -> Self {
        Value::Float(v)
    }
}

impl From<bool> for Value {
    fn from(v: bool) -> Self {
        Value::Int(v as i32)
    }
}

impl From<&str> for Value {
    fn from(v: &str) -> Self {
        Value::str(v)
    }
}

impl From<String> for Value {
    fn from(v: String) -> Self {
        Value::Str(v.into_bytes())
    }
}

impl<T: Into<Value>> From<Option<T>> for Value {
    fn from(v: Option<T>) -> Self {
        v.map(Into::into).unwrap_or(Value::None)
    }
}
