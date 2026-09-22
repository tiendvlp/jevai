//! The self-describing value type used wherever the API accepts
//! `string | object | array`.
//!
//! [`serde_json::Value`] would be the obvious choice, but it has no
//! [`rkyv::Archive`] implementation, so a message graph containing one cannot be
//! archived. [`Json`] is a local equivalent that derives both frameworks, plus
//! [`Map`], an order-preserving object representation.

use std::fmt;

use serde::de::{MapAccess, Visitor};
use serde::ser::SerializeMap;
use serde::{Deserialize, Deserializer, Serialize, Serializer};

/// An order-preserving, string-keyed map.
///
/// Every map in this API is small and order-sensitive: `questions` and
/// `criteria` are rendered into the prompt in the order you wrote them, and
/// `answers` mirrors `questions`. A [`std::collections::HashMap`] would scramble
/// that, and [`std::collections::BTreeMap`] would sort it, so both would change
/// the request the model actually sees. Backing this with a `Vec` of pairs keeps
/// insertion order, round-trips byte-for-byte, and archives under rkyv without a
/// custom impl.
///
/// Lookup is linear, which is the right trade at these sizes — a Choice is
/// capped at 255 options and a request holds a handful of questions.
#[derive(Clone, Debug, Default, PartialEq)]
#[cfg_attr(
    feature = "rkyv",
    derive(rkyv::Archive, rkyv::Serialize, rkyv::Deserialize)
)]
pub struct Map<V>(Vec<(String, V)>);

// `rkyv(derive(Debug))` cannot be used on a generic type: it would demand
// `V::Archived: Debug` unconditionally. Writing the impl by hand puts that
// bound where it belongs, and lets `ArchivedJson` derive `Debug` in turn.
/// Read access to a [`Map`] borrowed directly out of an rkyv buffer.
///
/// Mirrors the inherent methods on [`Map`] so archived and unarchived values
/// read the same way — the whole point of zero-copy is lost if reaching a value
/// requires deserializing the map first.
#[cfg(feature = "rkyv")]
impl<V: rkyv::Archive> ArchivedMap<V> {
    /// Number of entries.
    #[must_use]
    pub fn len(&self) -> usize {
        self.0.len()
    }

    /// Returns `true` if there are no entries.
    #[must_use]
    pub fn is_empty(&self) -> bool {
        self.0.is_empty()
    }

    /// Iterates entries in insertion order.
    pub fn iter(&self) -> impl Iterator<Item = (&str, &V::Archived)> {
        self.0.iter().map(|entry| (entry.0.as_ref(), &entry.1))
    }

    /// Iterates keys in insertion order.
    pub fn keys(&self) -> impl Iterator<Item = &str> {
        self.0.iter().map(|entry| entry.0.as_ref())
    }

    /// Iterates values in insertion order.
    pub fn values(&self) -> impl Iterator<Item = &V::Archived> {
        self.0.iter().map(|entry| &entry.1)
    }

    /// Returns the first value stored under `key`.
    #[must_use]
    pub fn get(&self, key: &str) -> Option<&V::Archived> {
        self.0
            .iter()
            .find(|entry| entry.0.as_ref() == key)
            .map(|entry| &entry.1)
    }
}

#[cfg(feature = "rkyv")]
impl<V> fmt::Debug for ArchivedMap<V>
where
    V: rkyv::Archive,
    V::Archived: fmt::Debug,
{
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_map()
            .entries(self.0.iter().map(|entry| (&entry.0, &entry.1)))
            .finish()
    }
}

impl<V> Map<V> {
    /// Creates an empty map.
    #[must_use]
    pub const fn new() -> Self {
        Self(Vec::new())
    }

    /// Appends an entry, returning `self` so calls can be chained.
    #[must_use]
    pub fn with(mut self, key: impl Into<String>, value: impl Into<V>) -> Self {
        self.0.push((key.into(), value.into()));
        self
    }

    /// Appends an entry.
    pub fn insert(&mut self, key: impl Into<String>, value: impl Into<V>) {
        self.0.push((key.into(), value.into()));
    }

    /// Returns the first value stored under `key`.
    #[must_use]
    pub fn get(&self, key: &str) -> Option<&V> {
        self.0.iter().find(|(k, _)| k == key).map(|(_, v)| v)
    }

    /// Returns `true` if `key` is present.
    #[must_use]
    pub fn contains_key(&self, key: &str) -> bool {
        self.get(key).is_some()
    }

    /// Number of entries.
    #[must_use]
    pub fn len(&self) -> usize {
        self.0.len()
    }

    /// Returns `true` if there are no entries.
    #[must_use]
    pub fn is_empty(&self) -> bool {
        self.0.is_empty()
    }

    /// Iterates entries in insertion order.
    pub fn iter(&self) -> impl Iterator<Item = (&str, &V)> {
        self.0.iter().map(|(k, v)| (k.as_str(), v))
    }

    /// Iterates keys in insertion order.
    pub fn keys(&self) -> impl Iterator<Item = &str> {
        self.0.iter().map(|(k, _)| k.as_str())
    }

    /// Iterates values in insertion order.
    pub fn values(&self) -> impl Iterator<Item = &V> {
        self.0.iter().map(|(_, v)| v)
    }
}

impl<V> From<Vec<(String, V)>> for Map<V> {
    fn from(entries: Vec<(String, V)>) -> Self {
        Self(entries)
    }
}

impl<V> FromIterator<(String, V)> for Map<V> {
    fn from_iter<I: IntoIterator<Item = (String, V)>>(iter: I) -> Self {
        Self(iter.into_iter().collect())
    }
}

impl<V, K: Into<String>> From<[(K, V); 0]> for Map<V> {
    fn from(_: [(K, V); 0]) -> Self {
        Self::new()
    }
}

impl<V> IntoIterator for Map<V> {
    type Item = (String, V);
    type IntoIter = std::vec::IntoIter<(String, V)>;

    fn into_iter(self) -> Self::IntoIter {
        self.0.into_iter()
    }
}

// Hand-written so the wire form is a JSON *object*. A derived impl on the inner
// `Vec<(String, V)>` would emit an array of pairs.
impl<V: Serialize> Serialize for Map<V> {
    fn serialize<S: Serializer>(&self, serializer: S) -> Result<S::Ok, S::Error> {
        let mut map = serializer.serialize_map(Some(self.0.len()))?;
        for (key, value) in &self.0 {
            map.serialize_entry(key, value)?;
        }
        map.end()
    }
}

impl<'de, V: Deserialize<'de>> Deserialize<'de> for Map<V> {
    fn deserialize<D: Deserializer<'de>>(deserializer: D) -> Result<Self, D::Error> {
        struct MapVisitor<V>(std::marker::PhantomData<V>);

        impl<'de, V: Deserialize<'de>> Visitor<'de> for MapVisitor<V> {
            type Value = Map<V>;

            fn expecting(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
                f.write_str("a JSON object")
            }

            fn visit_map<A: MapAccess<'de>>(self, mut access: A) -> Result<Self::Value, A::Error> {
                let mut entries = Vec::with_capacity(access.size_hint().unwrap_or(0));
                while let Some(entry) = access.next_entry()? {
                    entries.push(entry);
                }
                Ok(Map(entries))
            }
        }

        deserializer.deserialize_map(MapVisitor(std::marker::PhantomData))
    }
}

/// Any JSON value the API accepts.
///
/// Integers and floats are kept as separate variants so a round-trip does not
/// silently rewrite `3` as `3.0` — relevant because `instructions` and
/// `criteria` can carry arbitrary caller data (IDs, counts) that the model reads
/// back verbatim.
#[derive(Clone, Debug, Default, PartialEq, Serialize, Deserialize)]
#[cfg_attr(
    feature = "rkyv",
    derive(rkyv::Archive, rkyv::Serialize, rkyv::Deserialize),
    rkyv(derive(Debug)),
    // `Json` is recursive, so rkyv's "perfect derive" would recurse forever
    // generating `Json: Archive` bounds. `omit_bounds` on the two recursive
    // variants breaks the cycle, and these clauses restore the bounds the
    // generated impls actually need.
    rkyv(serialize_bounds(
        __S: rkyv::ser::Writer + rkyv::ser::Allocator,
        __S::Error: rkyv::rancor::Source,
    )),
    rkyv(deserialize_bounds(__D::Error: rkyv::rancor::Source)),
    rkyv(bytecheck(bounds(
        __C: rkyv::validation::ArchiveContext,
        __C::Error: rkyv::rancor::Source,
    )))
)]
#[serde(untagged)]
pub enum Json {
    /// `null`
    #[default]
    Null,
    /// `true` / `false`
    Bool(bool),
    /// A JSON number with no fractional part.
    Int(i64),
    /// A JSON number with a fractional part or exponent.
    Float(f64),
    /// A JSON string.
    Str(String),
    /// A JSON array.
    Array(#[cfg_attr(feature = "rkyv", rkyv(omit_bounds))] Vec<Json>),
    /// A JSON object.
    Object(#[cfg_attr(feature = "rkyv", rkyv(omit_bounds))] Map<Json>),
}

impl Json {
    /// Borrows the value as a string, if it is one.
    #[must_use]
    pub fn as_str(&self) -> Option<&str> {
        match self {
            Self::Str(s) => Some(s),
            _ => None,
        }
    }

    /// Reads the value as an `f64`, widening an [`Json::Int`] if needed.
    #[must_use]
    pub fn as_f64(&self) -> Option<f64> {
        match *self {
            Self::Int(i) => Some(i as f64),
            Self::Float(f) => Some(f),
            _ => None,
        }
    }

    /// Borrows the value as an object, if it is one.
    #[must_use]
    pub fn as_object(&self) -> Option<&Map<Json>> {
        match self {
            Self::Object(map) => Some(map),
            _ => None,
        }
    }

    /// Borrows the value as an array, if it is one.
    #[must_use]
    pub fn as_array(&self) -> Option<&[Json]> {
        match self {
            Self::Array(items) => Some(items),
            _ => None,
        }
    }
}

impl From<&str> for Json {
    fn from(value: &str) -> Self {
        Self::Str(value.to_owned())
    }
}

impl From<String> for Json {
    fn from(value: String) -> Self {
        Self::Str(value)
    }
}

impl From<bool> for Json {
    fn from(value: bool) -> Self {
        Self::Bool(value)
    }
}

impl From<i64> for Json {
    fn from(value: i64) -> Self {
        Self::Int(value)
    }
}

impl From<f64> for Json {
    fn from(value: f64) -> Self {
        Self::Float(value)
    }
}

impl From<Map<Json>> for Json {
    fn from(value: Map<Json>) -> Self {
        Self::Object(value)
    }
}

impl<T: Into<Json>> From<Vec<T>> for Json {
    fn from(value: Vec<T>) -> Self {
        Self::Array(value.into_iter().map(Into::into).collect())
    }
}

impl Json {
    /// Converts to a [`serde_json::Value`] for interop with the wider ecosystem.
    #[must_use]
    pub fn to_serde(&self) -> serde_json::Value {
        match self {
            Self::Null => serde_json::Value::Null,
            Self::Bool(b) => serde_json::Value::Bool(*b),
            Self::Int(i) => serde_json::Value::Number((*i).into()),
            Self::Float(f) => serde_json::Number::from_f64(*f)
                .map_or(serde_json::Value::Null, serde_json::Value::Number),
            Self::Str(s) => serde_json::Value::String(s.clone()),
            Self::Array(items) => {
                serde_json::Value::Array(items.iter().map(Self::to_serde).collect())
            }
            Self::Object(map) => serde_json::Value::Object(
                map.iter()
                    .map(|(k, v)| (k.to_owned(), v.to_serde()))
                    .collect(),
            ),
        }
    }

    /// Converts from a [`serde_json::Value`].
    ///
    /// Object key order is preserved only if `serde_json` is built with its
    /// `preserve_order` feature; by default it sorts keys, which reorders
    /// `criteria` relative to what the caller wrote. Build [`Json`] directly
    /// where option order matters.
    #[must_use]
    pub fn from_serde(value: &serde_json::Value) -> Self {
        match value {
            serde_json::Value::Null => Self::Null,
            serde_json::Value::Bool(b) => Self::Bool(*b),
            serde_json::Value::Number(n) => n
                .as_i64()
                .map_or_else(|| Self::Float(n.as_f64().unwrap_or(f64::NAN)), Self::Int),
            serde_json::Value::String(s) => Self::Str(s.clone()),
            serde_json::Value::Array(items) => {
                Self::Array(items.iter().map(Self::from_serde).collect())
            }
            serde_json::Value::Object(map) => Self::Object(
                map.iter()
                    .map(|(k, v)| (k.clone(), Self::from_serde(v)))
                    .collect(),
            ),
        }
    }
}

impl From<&serde_json::Value> for Json {
    fn from(value: &serde_json::Value) -> Self {
        Self::from_serde(value)
    }
}

impl From<Json> for serde_json::Value {
    fn from(value: Json) -> Self {
        value.to_serde()
    }
}
