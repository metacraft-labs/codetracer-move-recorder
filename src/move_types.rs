//! Move trace format type definitions.
//!
//! These types mirror the Move VM trace format (version 3) JSON schema,
//! allowing deserialization from NDJSON trace files without depending on
//! the heavyweight Sui crate.

use serde::Deserialize;
use serde::de;

/// First line of the NDJSON trace file.
#[derive(Deserialize, Debug)]
pub struct VersionHeader {
    pub version: u32,
}

/// A single trace event (one JSON line after the version header).
#[derive(Deserialize, Debug)]
#[serde(tag = "type")]
pub enum TraceEvent {
    OpenFrame {
        frame: Frame,
        gas_left: u64,
    },
    CloseFrame {
        frame_id: u64,
        #[serde(default)]
        return_: Option<Vec<SerializableMoveValue>>,
        gas_left: u64,
    },
    Instruction {
        #[serde(default)]
        type_parameters: Vec<String>,
        pc: u64,
        gas_left: u64,
        instruction: String,
    },
    Effect {
        effect: Effect,
    },
    External {
        effect: ExternalEffect,
    },
}

/// A function frame opened during execution.
#[derive(Deserialize, Debug)]
pub struct Frame {
    pub frame_id: u64,
    pub function_name: String,
    pub module: ModuleId,
    #[serde(default)]
    pub type_instantiation: Vec<String>,
    #[serde(default)]
    pub parameters: Vec<TraceValue>,
    #[serde(default)]
    pub return_types: Vec<String>,
    #[serde(default)]
    pub locals_types: Vec<String>,
    #[serde(default)]
    pub is_native: bool,
}

/// Identifies a Move module.
#[derive(Deserialize, Debug)]
pub struct ModuleId {
    pub address: String,
    pub name: String,
}

/// An effect produced by executing an instruction.
#[derive(Deserialize, Debug)]
#[serde(tag = "type")]
pub enum Effect {
    Pop {
        value: TraceValue,
    },
    Push {
        value: TraceValue,
    },
    Read {
        location: Location,
        value: TraceValue,
    },
    Write {
        location: Location,
        value: TraceValue,
    },
    DataLoad {
        #[serde(default)]
        address: Option<String>,
    },
    ExecutionError {
        #[serde(default)]
        error: String,
    },
}

/// A local variable location within a frame.
#[derive(Deserialize, Debug)]
pub struct Location {
    pub frame_id: u64,
    pub local_index: u64,
}

/// A value on the stack or in a local variable.
#[derive(Deserialize, Debug)]
#[serde(tag = "type")]
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
#[derive(Deserialize, Debug, Clone)]
#[serde(tag = "type")]
pub enum SerializableMoveValue {
    U8 { value: u8 },
    U16 { value: u16 },
    U32 { value: u32 },
    U64 { value: u64 },
    U128 {
        #[serde(deserialize_with = "deserialize_u128_from_number")]
        value: u128,
    },
    U256 { value: String },
    Bool { value: bool },
    Address { value: String },
    Struct {
        fields: Vec<SerializableMoveValue>,
        #[serde(default)]
        type_: String,
    },
    Vector {
        elements: Vec<SerializableMoveValue>,
    },
    Variant {
        tag: u16,
        fields: Vec<SerializableMoveValue>,
        #[serde(default)]
        type_: String,
    },
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
