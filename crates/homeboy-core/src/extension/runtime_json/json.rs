//! Insertion-ordered JSON value plus the exact text encodings the runtime
//! helpers emit.
//!
//! Workspace `serde_json` does not enable `preserve_order`, so its `Value`
//! sorts object keys. Sidecar records must round-trip with the caller's key
//! order intact, so the helpers parse into this ordered value instead.

use serde::de::{Deserialize, Deserializer, MapAccess, SeqAccess, Visitor};
use serde::ser::{Serialize, Serializer};
use serde_json::ser::{CompactFormatter, Formatter, PrettyFormatter};
use std::fmt;
use std::io;

#[derive(Debug, Clone, PartialEq)]
pub enum Json {
    Null,
    Bool(bool),
    Number(serde_json::Number),
    String(String),
    Array(Vec<Json>),
    Object(Vec<(String, Json)>),
}

impl Json {
    pub fn parse(raw: &str) -> Result<Self, String> {
        serde_json::from_str(raw).map_err(|error| error.to_string())
    }

    pub fn string(value: impl Into<String>) -> Self {
        Json::String(value.into())
    }

    /// Finite floats become numbers; non-finite values have no JSON form.
    pub fn float(value: f64) -> Self {
        serde_json::Number::from_f64(value)
            .map(Json::Number)
            .unwrap_or(Json::Null)
    }

    pub fn int(value: i64) -> Self {
        Json::Number(value.into())
    }

    pub fn get(&self, key: &str) -> Option<&Json> {
        match self {
            Json::Object(entries) => entries.iter().find(|(k, _)| k == key).map(|(_, v)| v),
            _ => None,
        }
    }

    /// Insert or replace `key`, keeping the original position on replace.
    pub fn insert(&mut self, key: &str, value: Json) {
        if let Json::Object(entries) = self {
            match entries.iter_mut().find(|(k, _)| k == key) {
                Some(entry) => entry.1 = value,
                None => entries.push((key.to_string(), value)),
            }
        }
    }

    /// Truthiness used to decide optional envelope keys: null, false, zero, and
    /// empty strings/containers are omitted.
    pub fn is_truthy(&self) -> bool {
        match self {
            Json::Null => false,
            Json::Bool(value) => *value,
            Json::Number(number) => number.as_f64().is_some_and(|value| value != 0.0),
            Json::String(value) => !value.is_empty(),
            Json::Array(items) => !items.is_empty(),
            Json::Object(entries) => !entries.is_empty(),
        }
    }

    /// Compact text with UTF-8 strings left unescaped.
    pub fn to_compact(&self) -> String {
        self.encode(CompactFormatter)
    }

    /// Compact text with non-ASCII escaped.
    pub fn to_compact_ascii(&self) -> String {
        self.encode(AsciiEscaping(CompactFormatter))
    }

    /// Two-space indented text with non-ASCII escaped.
    pub fn to_pretty_ascii(&self) -> String {
        self.encode(AsciiEscaping(PrettyFormatter::with_indent(b"  ")))
    }

    fn encode<F: Formatter>(&self, formatter: F) -> String {
        let mut out = Vec::new();
        let mut serializer = serde_json::Serializer::with_formatter(&mut out, formatter);
        self.serialize(&mut serializer)
            .expect("ordered JSON serializes into memory");
        String::from_utf8(out).expect("serde_json emits UTF-8")
    }
}

impl Serialize for Json {
    fn serialize<S: Serializer>(&self, serializer: S) -> Result<S::Ok, S::Error> {
        match self {
            Json::Null => serializer.serialize_unit(),
            Json::Bool(value) => serializer.serialize_bool(*value),
            Json::Number(number) => number.serialize(serializer),
            Json::String(value) => serializer.serialize_str(value),
            Json::Array(items) => serializer.collect_seq(items),
            Json::Object(entries) => {
                serializer.collect_map(entries.iter().map(|(key, value)| (key, value)))
            }
        }
    }
}

impl<'de> Deserialize<'de> for Json {
    fn deserialize<D: Deserializer<'de>>(deserializer: D) -> Result<Self, D::Error> {
        deserializer.deserialize_any(JsonVisitor)
    }
}

struct JsonVisitor;

impl<'de> Visitor<'de> for JsonVisitor {
    type Value = Json;

    fn expecting(&self, f: &mut fmt::Formatter) -> fmt::Result {
        f.write_str("any JSON value")
    }

    fn visit_unit<E>(self) -> Result<Json, E> {
        Ok(Json::Null)
    }

    fn visit_none<E>(self) -> Result<Json, E> {
        Ok(Json::Null)
    }

    fn visit_bool<E>(self, value: bool) -> Result<Json, E> {
        Ok(Json::Bool(value))
    }

    fn visit_i64<E>(self, value: i64) -> Result<Json, E> {
        Ok(Json::Number(value.into()))
    }

    fn visit_u64<E>(self, value: u64) -> Result<Json, E> {
        Ok(Json::Number(value.into()))
    }

    fn visit_f64<E>(self, value: f64) -> Result<Json, E> {
        Ok(Json::float(value))
    }

    fn visit_str<E>(self, value: &str) -> Result<Json, E> {
        Ok(Json::string(value))
    }

    fn visit_string<E>(self, value: String) -> Result<Json, E> {
        Ok(Json::String(value))
    }

    fn visit_seq<A: SeqAccess<'de>>(self, mut seq: A) -> Result<Json, A::Error> {
        let mut items = Vec::new();
        while let Some(item) = seq.next_element()? {
            items.push(item);
        }
        Ok(Json::Array(items))
    }

    fn visit_map<A: MapAccess<'de>>(self, mut map: A) -> Result<Json, A::Error> {
        // Duplicate keys keep their first position and last value.
        let mut object = Json::Object(Vec::new());
        while let Some((key, value)) = map.next_entry::<String, Json>()? {
            object.insert(&key, value);
        }
        Ok(object)
    }
}

/// Wraps a formatter so string contents escape every non-ASCII character (and
/// DEL) as `\uXXXX` (UTF-16 surrogate pairs above the BMP), the ASCII-safe
/// encoding the bench artifacts have always used.
struct AsciiEscaping<F>(F);

impl<F: Formatter> Formatter for AsciiEscaping<F> {
    fn write_string_fragment<W: ?Sized + io::Write>(
        &mut self,
        writer: &mut W,
        fragment: &str,
    ) -> io::Result<()> {
        let mut start = 0;
        for (index, ch) in fragment.char_indices() {
            if ch.is_ascii() && ch != '\x7f' {
                continue;
            }
            writer.write_all(fragment[start..index].as_bytes())?;
            let mut units = [0u16; 2];
            for unit in ch.encode_utf16(&mut units) {
                write!(writer, "\\u{unit:04x}")?;
            }
            start = index + ch.len_utf8();
        }
        writer.write_all(fragment[start..].as_bytes())
    }

    fn begin_array<W: ?Sized + io::Write>(&mut self, w: &mut W) -> io::Result<()> {
        self.0.begin_array(w)
    }

    fn end_array<W: ?Sized + io::Write>(&mut self, w: &mut W) -> io::Result<()> {
        self.0.end_array(w)
    }

    fn begin_array_value<W: ?Sized + io::Write>(
        &mut self,
        w: &mut W,
        first: bool,
    ) -> io::Result<()> {
        self.0.begin_array_value(w, first)
    }

    fn end_array_value<W: ?Sized + io::Write>(&mut self, w: &mut W) -> io::Result<()> {
        self.0.end_array_value(w)
    }

    fn begin_object<W: ?Sized + io::Write>(&mut self, w: &mut W) -> io::Result<()> {
        self.0.begin_object(w)
    }

    fn end_object<W: ?Sized + io::Write>(&mut self, w: &mut W) -> io::Result<()> {
        self.0.end_object(w)
    }

    fn begin_object_key<W: ?Sized + io::Write>(
        &mut self,
        w: &mut W,
        first: bool,
    ) -> io::Result<()> {
        self.0.begin_object_key(w, first)
    }

    fn begin_object_value<W: ?Sized + io::Write>(&mut self, w: &mut W) -> io::Result<()> {
        self.0.begin_object_value(w)
    }

    fn end_object_value<W: ?Sized + io::Write>(&mut self, w: &mut W) -> io::Result<()> {
        self.0.end_object_value(w)
    }
}
