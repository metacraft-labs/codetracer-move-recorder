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
    /// Open-ended JSON, as upstream declares it
    /// (`TraceEvent::External(Box<serde_json::Value>)` in
    /// `move-trace-format`).  Interpret it with [`ExternalEffect::classify`].
    External(serde_json::Value),
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
/// `Global` is `{"Global": trace_index}`: a reference into storage — on
/// Sui, an input object passed by reference — identified by the trace
/// position where it was introduced.
#[derive(Deserialize, Debug)]
pub enum Location {
    Local(u64, u64),
    Indexed(Box<Location>, u64),
    Global(u64),
}

/// The location a (possibly indexed) location is rooted in.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum RootLocation {
    /// Local `local_index` of the frame opened at trace index `frame_id`,
    /// which need not be the running frame: a callee writes its caller's
    /// local through a `&mut`.
    Local { frame_id: u64, local_index: u64 },
    /// A storage reference introduced at `trace_index`.
    Global { trace_index: u64 },
}

impl Location {
    /// Follow `Indexed` wrappers down to the root location.
    pub fn root(&self) -> RootLocation {
        match self {
            Location::Local(frame_id, local_index) => RootLocation::Local {
                frame_id: *frame_id,
                local_index: *local_index,
            },
            Location::Indexed(inner, _) => inner.root(),
            Location::Global(trace_index) => RootLocation::Global {
                trace_index: *trace_index,
            },
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
    /// Decimal digits.  Upstream serialises `U256` as its 32
    /// little-endian bytes (`move-core-types/src/u256.rs`); older fixtures
    /// carry a decimal string.  Both are accepted.
    U256 {
        #[serde(deserialize_with = "deserialize_u256_to_decimal")]
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
    /// A Move enum value (Move 2024 enums, and `Option`-like shapes).
    Variant(MoveVariant),
}

/// A Move enum value, normalised from either wire shape.
///
/// Upstream (`move-trace-format/src/value.rs`, `SimplifiedMoveVariant`):
/// `{"type": "Variant", "value": {"type_": <StructTag>, "variant_name": "Some",
/// "tag": 1, "fields": [["pos0", {...}], ...]}}`.  Older synthetic fixtures
/// use a flat shape with positional fields and a string type:
/// `{"type": "Variant", "tag": 1, "fields": [{...}], "type_": "0x1::option::Option"}`.
/// Fields of the flat shape get positional names (`pos0`, `pos1`, …), the
/// names the compiler gives positional variant fields.
#[derive(Debug, Clone)]
pub struct MoveVariant {
    /// The enum's type: a StructTag object upstream, a string in the flat shape.
    pub type_: serde_json::Value,
    /// The variant's name, when the trace carries it.
    pub variant_name: Option<String>,
    pub tag: u16,
    pub fields: Vec<(String, SerializableMoveValue)>,
}

impl MoveVariant {
    /// The enum's short type name (`Option`), or the flat shape's string.
    pub fn type_name(&self) -> String {
        match &self.type_ {
            serde_json::Value::String(s) => s.clone(),
            other => other
                .get("name")
                .and_then(|v| v.as_str())
                .unwrap_or("")
                .to_string(),
        }
    }

    /// The enum's type arguments, as upstream's StructTag carries them.
    pub fn type_args(&self) -> Vec<serde_json::Value> {
        self.type_
            .get("type_args")
            .and_then(|v| v.as_array())
            .cloned()
            .unwrap_or_default()
    }
}

impl<'de> Deserialize<'de> for MoveVariant {
    fn deserialize<D: de::Deserializer<'de>>(deserializer: D) -> Result<Self, D::Error> {
        #[derive(Deserialize)]
        struct Upstream {
            #[serde(default)]
            type_: serde_json::Value,
            #[serde(default)]
            variant_name: Option<String>,
            tag: u16,
            fields: Vec<(String, SerializableMoveValue)>,
        }
        #[derive(Deserialize)]
        #[serde(untagged)]
        enum Repr {
            Upstream {
                value: Upstream,
            },
            Flat {
                tag: u16,
                fields: Vec<SerializableMoveValue>,
                #[serde(default)]
                type_: String,
            },
        }
        Ok(match Repr::deserialize(deserializer)? {
            Repr::Upstream { value } => MoveVariant {
                type_: value.type_,
                variant_name: value.variant_name,
                tag: value.tag,
                fields: value.fields,
            },
            Repr::Flat { tag, fields, type_ } => MoveVariant {
                type_: serde_json::Value::String(type_),
                variant_name: None,
                tag,
                fields: fields
                    .into_iter()
                    .enumerate()
                    .map(|(i, v)| (format!("pos{i}"), v))
                    .collect(),
            },
        })
    }
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

fn deserialize_u256_to_decimal<'de, D>(deserializer: D) -> Result<String, D::Error>
where
    D: de::Deserializer<'de>,
{
    #[derive(Deserialize)]
    #[serde(untagged)]
    enum Raw {
        Text(String),
        LeBytes(Vec<u8>),
    }
    match Raw::deserialize(deserializer)? {
        Raw::Text(text) => Ok(text),
        Raw::LeBytes(bytes) if bytes.len() == 32 => Ok(le_bytes_to_decimal(&bytes)),
        Raw::LeBytes(bytes) => Err(de::Error::invalid_length(
            bytes.len(),
            &"32 little-endian bytes",
        )),
    }
}

/// Render an unsigned little-endian integer of any width in decimal, by
/// repeated division of the big-endian digit array by 10.
pub fn le_bytes_to_decimal(le_bytes: &[u8]) -> String {
    let mut be: Vec<u8> = le_bytes.iter().rev().copied().collect();
    let mut digits = Vec::new();
    while be.iter().any(|&b| b != 0) {
        let mut remainder: u16 = 0;
        for byte in be.iter_mut() {
            let acc = (remainder << 8) | u16::from(*byte);
            *byte = (acc / 10) as u8;
            remainder = acc % 10;
        }
        digits.push(b'0' + remainder as u8);
    }
    if digits.is_empty() {
        return "0".to_string();
    }
    digits.reverse();
    String::from_utf8(digits).expect("ASCII digits")
}

/// The interpretations of a `TraceEvent::External` payload.
///
/// Sui's adapter writes programmable-transaction-block (PTB) structure
/// through this channel — see `crates/sui-types/src/ptb_trace.rs`
/// (`PTBEvent`) in https://github.com/MystenLabs/sui:
///
///   * `{"Summary": {"name", "events": [PTBCommandInfo…]}}` once, before
///     the first command, listing every command of the block;
///   * `"MoveCallStart"` / `"MoveCallEnd"` around each `MoveCall` command;
///   * `{"ExternalEvent": {"description", "name", "values"}}` for each
///     non-Move command (`SplitCoins`, `TransferObjects`, `MakeMoveVec`, …).
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum ExternalEffect {
    /// The block's command list, one label per command in order:
    /// `<pkg>::<module>::<function>` for a Move call, the command name
    /// otherwise.
    PtbSummary(Vec<String>),
    /// A `MoveCall` command begins.
    MoveCallStart,
    /// A `MoveCall` command ends.
    MoveCallEnd,
    /// A non-Move command ran.
    PtbCommand { name: String, description: String },
    /// The `{"kind": …}` shape older synthetic fixtures use.
    Kind(String),
    /// Anything else, kept as compact JSON so nothing is dropped.
    Other(String),
}

impl ExternalEffect {
    /// Interpret an `External` payload.
    pub fn classify(value: &serde_json::Value) -> Self {
        use serde_json::Value;
        let text = |v: Option<&Value>| v.and_then(Value::as_str).unwrap_or("").to_string();
        match value {
            Value::String(marker) if marker == "MoveCallStart" => Self::MoveCallStart,
            Value::String(marker) if marker == "MoveCallEnd" => Self::MoveCallEnd,
            Value::Object(map) => {
                if let Some(summary) = map.get("Summary") {
                    let commands = summary
                        .get("events")
                        .and_then(Value::as_array)
                        .map(|events| events.iter().map(command_label).collect())
                        .unwrap_or_default();
                    return Self::PtbSummary(commands);
                }
                if let Some(event) = map.get("ExternalEvent") {
                    return Self::PtbCommand {
                        name: text(event.get("name")),
                        description: text(event.get("description")),
                    };
                }
                if let Some(Value::String(kind)) = map.get("kind") {
                    return Self::Kind(kind.clone());
                }
                Self::Other(value.to_string())
            }
            _ => Self::Other(value.to_string()),
        }
    }
}

/// Label one `PTBCommandInfo` from a PTB summary.
fn command_label(info: &serde_json::Value) -> String {
    if let Some(call) = info.get("MoveCall") {
        let part = |k: &str| call.get(k).and_then(|v| v.as_str()).unwrap_or("?");
        return format!("{}::{}::{}", part("pkg"), part("module"), part("function"));
    }
    if let Some(name) = info.get("ExternalEvent").and_then(|v| v.as_str()) {
        return name.to_string();
    }
    info.to_string()
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

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn u256_le_bytes_render_in_decimal() {
        let mut le = [0u8; 32];
        assert_eq!(le_bytes_to_decimal(&le), "0");
        le[0] = 0x01;
        le[1] = 0x01;
        assert_eq!(le_bytes_to_decimal(&le), "257");
        // 2^255: only the top bit of the last little-endian byte.
        let mut top = [0u8; 32];
        top[31] = 0x80;
        assert_eq!(
            le_bytes_to_decimal(&top),
            "57896044618658097711785492504343953926634992332820282019728792003956564819968"
        );
    }

    #[test]
    fn u256_accepts_both_wire_forms() {
        let from_bytes: SerializableMoveValue = serde_json::from_str(
            r#"{"type":"U256","value":[1,1,0,0,0,0,0,0,0,0,0,0,0,0,0,0,0,0,0,0,0,0,0,0,0,0,0,0,0,0,0,0]}"#,
        )
        .unwrap();
        let from_text: SerializableMoveValue =
            serde_json::from_str(r#"{"type":"U256","value":"257"}"#).unwrap();
        for value in [from_bytes, from_text] {
            match value {
                SerializableMoveValue::U256 { value } => assert_eq!(value, "257"),
                other => panic!("expected U256, got {other:?}"),
            }
        }
        // A byte array of the wrong width is refused, not truncated.
        assert!(
            serde_json::from_str::<SerializableMoveValue>(r#"{"type":"U256","value":[1,2,3]}"#)
                .is_err()
        );
    }

    #[test]
    fn variants_parse_from_the_upstream_and_the_flat_shape() {
        // Upstream `SimplifiedMoveVariant`, as `sui replay` 1.81 writes it.
        let upstream: SerializableMoveValue = serde_json::from_str(
            r#"{"type":"Variant","value":{"type_":{"address":"f4","module":"enum_option",
                "name":"Option","type_args":["u64"]},"variant_name":"Some","tag":1,
                "fields":[["pos0",{"type":"U64","value":7}]]}}"#,
        )
        .unwrap();
        let SerializableMoveValue::Variant(v) = upstream else {
            panic!("expected a variant")
        };
        assert_eq!(
            (v.type_name(), v.variant_name.as_deref(), v.tag),
            ("Option".into(), Some("Some"), 1)
        );
        assert_eq!(v.type_args(), vec![serde_json::json!("u64")]);
        assert!(
            matches!(v.fields.as_slice(), [(n, SerializableMoveValue::U64 { value: 7 })] if n == "pos0")
        );

        // CONTROL: the flat fixture shape still parses, with positional names.
        let flat: SerializableMoveValue = serde_json::from_str(
            r#"{"type":"Variant","tag":0,"fields":[{"type":"Bool","value":true}],"type_":"0x1::option::Option"}"#,
        )
        .unwrap();
        let SerializableMoveValue::Variant(f) = flat else {
            panic!("expected a variant")
        };
        assert_eq!(
            (f.type_name(), f.variant_name, f.tag),
            ("0x1::option::Option".into(), None, 0)
        );
        assert_eq!(f.fields[0].0, "pos0");
    }

    #[test]
    fn external_payloads_classify_as_ptb_structure() {
        let classify = |json: &str| ExternalEffect::classify(&serde_json::from_str(json).unwrap());
        assert_eq!(
            classify(r#""MoveCallStart""#),
            ExternalEffect::MoveCallStart
        );
        assert_eq!(classify(r#""MoveCallEnd""#), ExternalEffect::MoveCallEnd);
        assert_eq!(
            classify(
                r#"{"Summary":{"name":"PTBSummary","events":[
                    {"MoveCall":{"pkg":"0x2","module":"coin","function":"split"}},
                    {"ExternalEvent":"TransferObjects"}]}}"#
            ),
            ExternalEffect::PtbSummary(vec![
                "0x2::coin::split".to_string(),
                "TransferObjects".to_string()
            ])
        );
        assert_eq!(
            classify(
                r#"{"ExternalEvent":{"description":"MakeMoveVec: vector","name":"MakeMoveVec","values":[]}}"#
            ),
            ExternalEffect::PtbCommand {
                name: "MakeMoveVec".to_string(),
                description: "MakeMoveVec: vector".to_string()
            }
        );
        assert_eq!(
            classify(r#"{"kind":"transfer_object"}"#),
            ExternalEffect::Kind("transfer_object".to_string())
        );
        // An unrecognised payload is kept verbatim rather than dropped.
        assert_eq!(
            classify(r#"[1,2]"#),
            ExternalEffect::Other("[1,2]".to_string())
        );
    }
}
