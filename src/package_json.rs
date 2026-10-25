//! A `package.json` as Node reads it: its objects keep their keys in source order, because
//! that is the order `exports` and `imports` conditions are tried in.
//!
//! serde_json keeps that order only with its `preserve_order` feature, and Cargo turns a feature
//! on for every crate built beside the one that asked — so an embedder's own JSON would change
//! order because this runtime resolves packages. This type keeps the order itself.

use std::fmt;

use serde::de::{Deserialize, Deserializer, MapAccess, SeqAccess, Visitor};

/// A JSON value whose objects are in source order.
#[derive(Clone, Debug, PartialEq)]
pub(crate) enum Json {
    Null,
    Bool(bool),
    Number(f64),
    String(String),
    Array(Vec<Json>),
    Object(Object),
}

/// A JSON object's members, in source order. A key given twice keeps its first place and its
/// last value, as `JSON.parse` does.
#[derive(Clone, Debug, Default, PartialEq)]
pub(crate) struct Object(Vec<(String, Json)>);

impl Object {
    pub(crate) fn get(&self, key: &str) -> Option<&Json> {
        self.0.iter().find(|(k, _)| k == key).map(|(_, v)| v)
    }

    pub(crate) fn iter(&self) -> impl Iterator<Item = (&String, &Json)> {
        self.0.iter().map(|(k, v)| (k, v))
    }

    pub(crate) fn keys(&self) -> impl Iterator<Item = &String> {
        self.0.iter().map(|(k, _)| k)
    }

    pub(crate) fn len(&self) -> usize {
        self.0.len()
    }

    fn insert(&mut self, key: String, value: Json) {
        match self.0.iter_mut().find(|(k, _)| *k == key) {
            Some(slot) => slot.1 = value,
            None => self.0.push((key, value)),
        }
    }
}

impl<'a> IntoIterator for &'a Object {
    type Item = (&'a String, &'a Json);
    type IntoIter = Box<dyn Iterator<Item = (&'a String, &'a Json)> + 'a>;

    fn into_iter(self) -> Self::IntoIter {
        Box::new(self.iter())
    }
}

impl Json {
    /// A member of an object; `None` for anything else.
    pub(crate) fn get(&self, key: &str) -> Option<&Json> {
        self.as_object()?.get(key)
    }

    pub(crate) fn is_null(&self) -> bool {
        matches!(self, Self::Null)
    }

    pub(crate) fn as_str(&self) -> Option<&str> {
        match self {
            Self::String(text) => Some(text),
            _ => None,
        }
    }

    pub(crate) fn as_object(&self) -> Option<&Object> {
        match self {
            Self::Object(object) => Some(object),
            _ => None,
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
        f.write_str("a JSON value")
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
        Ok(Json::Number(value as f64))
    }

    fn visit_u64<E>(self, value: u64) -> Result<Json, E> {
        Ok(Json::Number(value as f64))
    }

    fn visit_f64<E>(self, value: f64) -> Result<Json, E> {
        Ok(Json::Number(value))
    }

    fn visit_str<E>(self, value: &str) -> Result<Json, E> {
        Ok(Json::String(value.to_owned()))
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
        let mut object = Object::default();
        while let Some((key, value)) = map.next_entry::<String, Json>()? {
            object.insert(key, value);
        }
        Ok(Json::Object(object))
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn objects_keep_source_order_and_a_repeated_key_its_first_place() {
        let json: Json =
            serde_json::from_str(r#"{"z":1,"import":{"b":2,"a":3},"default":"x","z":4}"#)
                .unwrap();
        let keys: Vec<_> = json.as_object().unwrap().keys().cloned().collect();
        assert_eq!(keys, ["z", "import", "default"]);
        assert_eq!(json.get("z"), Some(&Json::Number(4.)));
        let inner: Vec<_> = json.get("import").unwrap().as_object().unwrap().keys().cloned().collect();
        assert_eq!(inner, ["b", "a"]);
        assert_eq!(json.get("default").and_then(Json::as_str), Some("x"));
    }
}
