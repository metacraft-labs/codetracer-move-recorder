//! Move trace format type definitions.
//!
//! These types mirror the Move VM trace format (version 3) JSON schema
//! as produced by Sui ≥1.68.  The format uses serde's default external
//! tagging for enums, e.g. `{"OpenFrame": {...}}`.

// Doc-comment formatting in this file mixes prose with inline code
// blocks and signature snippets; clippy::doc_*_list_items flag the
// continuation indentation but the layout is intentional.
#![allow(clippy::doc_lazy_continuation, clippy::doc_overindented_list_items)]

use serde::Deserialize;
use serde::de;

/// First line of the NDJSON trace file.
#[derive(Deserialize, Debug)]
pub struct VersionHeader {
    pub version: u32,
}

/// A single trace event (one JSON line after the version header).
///
/// Sui ≥1.68 uses externally tagged enums (serde default), e.g.:
///   `{"OpenFrame": {"frame": {...}, "gas_left": 123}}`
#[derive(Deserialize, Debug)]
pub enum TraceEvent {
    OpenFrame {
        frame: Frame,
        gas_left: u64,
    },
    CloseFrame {
        frame_id: u64,
        #[serde(default, rename = "return_")]
        return_values: Vec<TraceValue>,
        gas_left: u64,
    },
    Instruction {
        #[serde(default)]
        type_parameters: Vec<serde_json::Value>,
        pc: u64,
        gas_left: u64,
        instruction: String,
    },
    Effect(Effect),
    External(ExternalEffect),
}

/// A function frame opened during execution.
#[derive(Deserialize, Debug)]
pub struct Frame {
    pub frame_id: u64,
    pub function_name: String,
    pub module: ModuleId,
    #[serde(default)]
    pub version_id: String,
    #[serde(default)]
    pub binary_member_index: u64,
    #[serde(default)]
    pub type_instantiation: Vec<serde_json::Value>,
    #[serde(default)]
    pub parameters: Vec<TraceValue>,
    #[serde(default)]
    pub return_types: Vec<LocalType>,
    #[serde(default)]
    pub locals_types: Vec<LocalType>,
    #[serde(default)]
    pub is_native: bool,
    /// Optional visibility tag carried per-frame.  Sui's real v3 trace
    /// format does not emit visibility today, so this field defaults
    /// to `None` and the M5–M8 fixtures that omit it stay
    /// byte-for-byte compatible.  Synthetic fixtures (notably the M9
    /// `public_package_test` and `module_init_test` fixtures) populate
    /// it with the canonical visibility-class string the bytecode
    /// preserves at compile time:
    ///   * `"public"`             — `public fun`
    ///   * `"public(package)"`    — Move 2024 `public(package) fun`
    ///   * `"public(friend)"`     — pre-2024 `public(friend) fun`
    ///   * `"friend"`             — `friend fun` (legacy)
    ///   * `"init"`               — Sui's one-time module-init entry
    ///                              (`fun init(ctx: &mut TxContext)`)
    /// When present and non-empty, the recorder surfaces it as a
    /// `MoveCallVisibility` `TraceLogEvent` immediately preceding the
    /// frame's `call_entry` so downstream consumers can recover the
    /// visibility-class metadata without re-parsing the source.
    #[serde(default)]
    pub visibility: Option<String>,
}

/// Identifies a Move module.
#[derive(Deserialize, Debug)]
pub struct ModuleId {
    pub address: String,
    pub name: String,
}

/// A local variable's type annotation in a frame.
#[derive(Deserialize, Debug)]
pub struct LocalType {
    #[serde(default)]
    pub type_: serde_json::Value,
    #[serde(default)]
    pub ref_type: Option<String>,
}

/// An effect produced by executing an instruction.
///
/// Externally tagged: `{"Push": {...}}`, `{"Write": {...}}`, etc.
#[derive(Deserialize, Debug)]
pub enum Effect {
    Pop(TraceValue),
    Push(TraceValue),
    Read {
        location: Location,
        root_value_read: TraceValue,
        #[serde(default)]
        moved: bool,
    },
    Write {
        location: Location,
        root_value_after_write: TraceValue,
    },
    DataLoad {
        #[serde(default)]
        address: Option<String>,
    },
    ExecutionError(String),
}

/// A variable location within a frame.
///
/// `Local` is serialized as `{"Local": [frame_id, local_index]}`.
/// `Indexed` is `{"Indexed": [<nested_location>, field_index]}` and
/// appears when accessing struct fields through references.
#[derive(Deserialize, Debug)]
pub enum Location {
    Local(u64, u64),
    Indexed(Box<Location>, u64),
}

impl Location {
    /// Extract the innermost local variable index.
    ///
    /// For `Local(frame_id, idx)` returns `idx`.
    /// For `Indexed(inner, _)` recurses into `inner` to find the base local.
    pub fn local_index(&self) -> u64 {
        match self {
            Location::Local(_, idx) => *idx,
            Location::Indexed(inner, _) => inner.local_index(),
        }
    }
}

/// A value on the stack or in a local variable.
///
/// Externally tagged: `{"RuntimeValue": {"value": {...}}}`.
#[derive(Deserialize, Debug)]
pub enum TraceValue {
    RuntimeValue {
        value: SerializableMoveValue,
    },
    ImmRef {
        location: Location,
        snapshot: SerializableMoveValue,
    },
    MutRef {
        location: Location,
        snapshot: SerializableMoveValue,
    },
}

/// Concrete Move value (the `value` / `snapshot` fields).
///
/// Still internally tagged with `"type"`: `{"type": "U64", "value": 10}`.
#[derive(Deserialize, Debug, Clone)]
#[serde(tag = "type")]
pub enum SerializableMoveValue {
    U8 {
        value: u8,
    },
    U16 {
        value: u16,
    },
    U32 {
        value: u32,
    },
    U64 {
        value: u64,
    },
    U128 {
        #[serde(deserialize_with = "deserialize_u128_from_number")]
        value: u128,
    },
    U256 {
        value: String,
    },
    Bool {
        value: bool,
    },
    Address {
        value: String,
    },
    /// Struct values have a nested `value` containing `type_` and `fields`.
    ///
    /// JSON: `{"type": "Struct", "value": {"type_": {...}, "fields": [["x", {...}], ...]}}`
    Struct {
        value: StructContent,
    },
    Vector {
        #[serde(alias = "value")]
        elements: Vec<SerializableMoveValue>,
    },
    Variant {
        tag: u16,
        fields: Vec<SerializableMoveValue>,
        #[serde(default)]
        type_: String,
    },
}

/// The inner content of a Struct value.
#[derive(Deserialize, Debug, Clone)]
pub struct StructContent {
    #[serde(default)]
    pub type_: serde_json::Value,
    /// Fields are name-value pairs: `[["field_name", {value}], ...]`
    pub fields: Vec<(String, SerializableMoveValue)>,
}

/// Custom deserializer for u128 values.
///
/// serde_json does not support u128 deserialization in internally tagged enums
/// (`#[serde(tag = "type")]`).  This workaround accepts the value as either a
/// JSON number (via its string representation) or a JSON string, then parses it
/// into a `u128`.
fn deserialize_u128_from_number<'de, D>(deserializer: D) -> Result<u128, D::Error>
where
    D: de::Deserializer<'de>,
{
    struct U128Visitor;

    impl<'de> de::Visitor<'de> for U128Visitor {
        type Value = u128;

        fn expecting(&self, formatter: &mut std::fmt::Formatter) -> std::fmt::Result {
            formatter.write_str("a u128 value as a number or string")
        }

        fn visit_u64<E: de::Error>(self, v: u64) -> Result<u128, E> {
            Ok(v as u128)
        }

        fn visit_i64<E: de::Error>(self, v: i64) -> Result<u128, E> {
            u128::try_from(v).map_err(de::Error::custom)
        }

        fn visit_str<E: de::Error>(self, v: &str) -> Result<u128, E> {
            v.parse::<u128>().map_err(de::Error::custom)
        }

        fn visit_map<A: de::MapAccess<'de>>(self, mut map: A) -> Result<u128, A::Error> {
            // When serde_json encounters a number in an internally tagged enum,
            // it may wrap it in a map with a single "$serde_json::private::Number" key.
            // We handle this by extracting the string representation and parsing it.
            let mut value: Option<String> = None;
            while let Some(key) = map.next_key::<String>()? {
                if key.contains("Number") || key.starts_with('$') {
                    value = Some(map.next_value()?);
                } else {
                    let _: de::IgnoredAny = map.next_value()?;
                }
            }
            match value {
                Some(s) => s.parse::<u128>().map_err(de::Error::custom),
                None => Err(de::Error::custom("expected a number value in map")),
            }
        }
    }

    deserializer.deserialize_any(U128Visitor)
}

/// External effect (simplified — we just capture the kind string).
#[derive(Deserialize, Debug)]
pub struct ExternalEffect {
    #[serde(default)]
    pub kind: String,
}

impl TraceValue {
    /// Extract the inner serializable value regardless of ref wrapper.
    pub fn inner_value(&self) -> &SerializableMoveValue {
        match self {
            TraceValue::RuntimeValue { value } => value,
            TraceValue::ImmRef { snapshot, .. } => snapshot,
            TraceValue::MutRef { snapshot, .. } => snapshot,
        }
    }
}
