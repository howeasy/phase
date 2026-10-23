//! Display-only printing annotations a deck submission carries (the printings sidecar).
//!
//! CR 203.1 + CR 206.1: a card's illustration and expansion symbol have no effect on
//! game play, so a printing is display data and no rules path reads these types.

use std::fmt;
use std::marker::PhantomData;

use serde::de::{DeserializeOwned, DeserializeSeed, IgnoredAny, MapAccess, SeqAccess, Visitor};
use serde::{Deserialize, Deserializer, Serialize};
use serde_json::{Map, Number, Value};

/// One printing of a card: its set code and collector number.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct CardPrinting {
    pub set_code: String,
    pub collector_number: String,
}

/// The deck-list field a printing annotation names: one variant per card-section
/// field of `DeckData` (sticker sheets are not cards), serialized as that field's name.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum DeckSection {
    MainDeck,
    Sideboard,
    Commander,
    Companion,
    AttractionDeck,
    PlanarDeck,
    SchemeDeck,
    ContraptionDeck,
    SignatureSpell,
    /// A section name this build does not know (a newer peer's). Decoded so the
    /// annotation cannot fail its deck; never produced by this build.
    #[serde(other)]
    Unknown,
}

/// The owner's explicit printing for `count` copies of `name` in `section`.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct DeckPrinting {
    pub section: DeckSection,
    pub name: String,
    pub count: u32,
    pub printing: CardPrinting,
}

/// The single decoder for a deck list's `printings` field. Each element is read
/// whole and then decoded on its own; one that does not decode is dropped, and a
/// value that is not a sequence decodes as no annotations, so an annotation the
/// codec can read as a value never fails the deck that carries it. An element the
/// codec itself cannot read (a JSON number outside the finite f64 range; on the
/// wasm path a `Set` or an out-of-range BigInt) is the codec's frame error, exactly
/// as the same value in any other field would be; it is propagated, never swallowed,
/// because a streaming deserializer that failed inside an element cannot resume.
pub fn deserialize_deck_printings<'de, D>(deserializer: D) -> Result<Vec<DeckPrinting>, D::Error>
where
    D: Deserializer<'de>,
{
    deserializer.deserialize_any(LenientElements::<DeckPrinting>(PhantomData))
}

/// Reads each element whole through `AnyElement`, so a malformed element leaves the
/// stream intact, then decodes it into `T` from memory, dropping failures.
struct LenientElements<T>(PhantomData<fn() -> T>);

impl<'de, T: DeserializeOwned> Visitor<'de> for LenientElements<T> {
    type Value = Vec<T>;

    fn expecting(&self, f: &mut fmt::Formatter) -> fmt::Result {
        f.write_str("a sequence of annotations")
    }

    fn visit_seq<A: SeqAccess<'de>>(self, mut seq: A) -> Result<Vec<T>, A::Error> {
        let mut kept = Vec::new();
        // `?` here is the codec failing to read the element at all: propagated.
        while let Some(raw) = seq.next_element_seed(AnyElement {
            depth_left: ELEMENT_DEPTH,
        })? {
            // A malformed annotation is dropped, never an error.
            if let Ok(value) = T::deserialize(raw) {
                kept.push(value);
            }
        }
        Ok(kept)
    }

    fn visit_some<D: Deserializer<'de>>(self, d: D) -> Result<Vec<T>, D::Error> {
        d.deserialize_any(self)
    }
    fn visit_unit<E: serde::de::Error>(self) -> Result<Vec<T>, E> {
        Ok(Vec::new())
    }
    fn visit_none<E: serde::de::Error>(self) -> Result<Vec<T>, E> {
        Ok(Vec::new())
    }
    fn visit_bool<E: serde::de::Error>(self, _: bool) -> Result<Vec<T>, E> {
        Ok(Vec::new())
    }
    fn visit_i64<E: serde::de::Error>(self, _: i64) -> Result<Vec<T>, E> {
        Ok(Vec::new())
    }
    fn visit_u64<E: serde::de::Error>(self, _: u64) -> Result<Vec<T>, E> {
        Ok(Vec::new())
    }
    fn visit_f64<E: serde::de::Error>(self, _: f64) -> Result<Vec<T>, E> {
        Ok(Vec::new())
    }
    fn visit_str<E: serde::de::Error>(self, _: &str) -> Result<Vec<T>, E> {
        Ok(Vec::new())
    }
    fn visit_bytes<E: serde::de::Error>(self, _: &[u8]) -> Result<Vec<T>, E> {
        Ok(Vec::new())
    }
    fn visit_map<A: MapAccess<'de>>(self, mut map: A) -> Result<Vec<T>, A::Error> {
        while map.next_entry::<IgnoredAny, IgnoredAny>()?.is_some() {}
        Ok(Vec::new())
    }
}

/// How many levels of one element are read. Every type this module decodes has its
/// deepest field two levels below the element, so any cap of 3 or more never changes
/// whether an element decodes. Levels below the cap are skipped unread with
/// `IgnoredAny` (serde_json skips iteratively, serde-wasm-bindgen without traversal),
/// which keeps this decoder's own recursion far under serde_json's 128-level limit.
/// This is not a resource bound: it never rejects anything.
const ELEMENT_DEPTH: u8 = 16;

/// Materialises one element as a `serde_json::Value` through the codec's
/// `deserialize_any`, from any self-describing codec. Unlike `Value`'s own
/// `Deserialize`, it accepts byte buffers and non-string map keys (both read and
/// dropped) and never treats an object key specially (`Value` reinterprets
/// serde_json's private raw-value token when the `raw_value` feature is on, which
/// `lobby-broker` enables in every server build).
#[derive(Clone, Copy)]
struct AnyElement {
    depth_left: u8,
}

impl AnyElement {
    fn child(self) -> Self {
        // Only called from a visitor, which runs only when `depth_left > 0`.
        Self {
            depth_left: self.depth_left - 1,
        }
    }
}

impl<'de> DeserializeSeed<'de> for AnyElement {
    type Value = Value;

    fn deserialize<D: Deserializer<'de>>(self, deserializer: D) -> Result<Value, D::Error> {
        if self.depth_left == 0 {
            IgnoredAny::deserialize(deserializer)?;
            return Ok(Value::Null);
        }
        deserializer.deserialize_any(self)
    }
}

impl<'de> Visitor<'de> for AnyElement {
    type Value = Value;

    fn expecting(&self, f: &mut fmt::Formatter) -> fmt::Result {
        f.write_str("any value")
    }

    fn visit_bool<E: serde::de::Error>(self, v: bool) -> Result<Value, E> {
        Ok(Value::Bool(v))
    }
    fn visit_i64<E: serde::de::Error>(self, v: i64) -> Result<Value, E> {
        Ok(Value::from(v))
    }
    fn visit_u64<E: serde::de::Error>(self, v: u64) -> Result<Value, E> {
        Ok(Value::from(v))
    }
    fn visit_i128<E: serde::de::Error>(self, v: i128) -> Result<Value, E> {
        Ok(Number::from_i128(v).map_or(Value::Null, Value::Number))
    }
    fn visit_u128<E: serde::de::Error>(self, v: u128) -> Result<Value, E> {
        Ok(Number::from_u128(v).map_or(Value::Null, Value::Number))
    }
    // A non-finite float (a JS `Infinity` or `NaN`) becomes `Null`.
    fn visit_f64<E: serde::de::Error>(self, v: f64) -> Result<Value, E> {
        Ok(Value::from(v))
    }
    fn visit_str<E: serde::de::Error>(self, v: &str) -> Result<Value, E> {
        Ok(Value::from(v))
    }
    fn visit_string<E: serde::de::Error>(self, v: String) -> Result<Value, E> {
        Ok(Value::String(v))
    }
    // Bytes (a JS Uint8Array/ArrayBuffer) have no JSON form and no annotation field holds them.
    fn visit_bytes<E: serde::de::Error>(self, _: &[u8]) -> Result<Value, E> {
        Ok(Value::Null)
    }
    fn visit_unit<E: serde::de::Error>(self) -> Result<Value, E> {
        Ok(Value::Null)
    }
    fn visit_none<E: serde::de::Error>(self) -> Result<Value, E> {
        Ok(Value::Null)
    }
    fn visit_some<D: Deserializer<'de>>(self, d: D) -> Result<Value, D::Error> {
        d.deserialize_any(self)
    }
    fn visit_newtype_struct<D: Deserializer<'de>>(self, d: D) -> Result<Value, D::Error> {
        d.deserialize_any(self)
    }

    fn visit_seq<A: SeqAccess<'de>>(self, mut seq: A) -> Result<Value, A::Error> {
        let mut items = Vec::new();
        while let Some(item) = seq.next_element_seed(self.child())? {
            items.push(item);
        }
        Ok(Value::Array(items))
    }

    fn visit_map<A: MapAccess<'de>>(self, mut map: A) -> Result<Value, A::Error> {
        let mut entries = Map::new();
        while let Some(key) = map.next_key_seed(self.child())? {
            let value = map.next_value_seed(self.child())?;
            // A non-string key (a JS `Map` with number keys) names no annotation field.
            if let Value::String(key) = key {
                entries.insert(key, value);
            }
        }
        Ok(Value::Object(entries))
    }
}
