//! Tests for Aptos support (M5).
//!
//! These tests verify the Aptos adapter, replay pipeline, and REST API
//! type parsing without requiring the Aptos CLI or network access.

use std::path::Path;

use codetracer_trace_writer::TraceEventsFileFormat;

use codetracer_move_recorder::aptos_adapter::{
    self, AptosEnrichedEntry, AptosGasProfile, AptosRestConfig,
    AptosTraceEntry, GasProfileNode, merge_trace_and_gas, parse_gas_profile_json,
    parse_move_vm_trace_csv, parse_resources_response,
};
use codetracer_move_recorder::aptos_replay::{self, AptosReplayConfig};

// ---------------------------------------------------------------------------
// Test 1: Parse realistic MOVE_VM_TRACE CSV output
// ---------------------------------------------------------------------------

#[test]
fn test_parse_move_vm_trace_csv() {
    let csv_data = "\
0x1::coin::transfer,0
0x1::coin::transfer,1
0x1::coin::transfer,2
0x1::coin::withdraw,0
0x1::coin::withdraw,1
0x1::coin::withdraw,2
0x1::coin::withdraw,3
0x1::coin::deposit,0
0x1::coin::deposit,1
0x1::coin::deposit,2
";

    let entries = parse_move_vm_trace_csv(csv_data);
    assert_eq!(entries.len(), 10, "should parse all 10 CSV lines");

    // Verify first entry.
    assert_eq!(entries[0].function_name, "0x1::coin::transfer");
    assert_eq!(entries[0].pc, 0);

    // Verify a middle entry.
    assert_eq!(entries[3].function_name, "0x1::coin::withdraw");
    assert_eq!(entries[3].pc, 0);

    // Verify last entry.
    assert_eq!(entries[9].function_name, "0x1::coin::deposit");
    assert_eq!(entries[9].pc, 2);
}

#[test]
fn test_parse_move_vm_trace_csv_empty_lines() {
    let csv_data = "\
0x1::module::func,0

0x1::module::func,1

";
    let entries = parse_move_vm_trace_csv(csv_data);
    assert_eq!(entries.len(), 2, "should skip empty lines");
}

#[test]
fn test_parse_move_vm_trace_csv_empty_input() {
    let entries = parse_move_vm_trace_csv("");
    assert!(entries.is_empty(), "empty input should produce no entries");
}

// ---------------------------------------------------------------------------
// Test 2: Parse --profile-gas JSON output
// ---------------------------------------------------------------------------

#[test]
fn test_parse_gas_profile_json() {
    let json_data = r#"{
        "gas_used": 42000,
        "call_tree": {
            "name": "0x1::coin::transfer",
            "gas_cost": 5000,
            "total_gas": 42000,
            "children": [
                {
                    "name": "0x1::coin::withdraw",
                    "gas_cost": 15000,
                    "total_gas": 20000,
                    "children": [
                        {
                            "name": "0x1::account::get_balance",
                            "gas_cost": 5000,
                            "total_gas": 5000,
                            "children": []
                        }
                    ]
                },
                {
                    "name": "0x1::coin::deposit",
                    "gas_cost": 17000,
                    "total_gas": 17000,
                    "children": []
                }
            ]
        },
        "metadata": {
            "transaction_version": 123456789,
            "gas_unit_price": 100,
            "max_gas_amount": 100000
        }
    }"#;

    let profile = parse_gas_profile_json(json_data).expect("should parse gas profile JSON");

    assert_eq!(profile.gas_used, 42000);
    assert!(profile.call_tree.is_some());

    let tree = profile.call_tree.as_ref().unwrap();
    assert_eq!(tree.name, "0x1::coin::transfer");
    assert_eq!(tree.total_gas, 42000);
    assert_eq!(tree.children.len(), 2);

    // Verify nested child.
    assert_eq!(tree.children[0].name, "0x1::coin::withdraw");
    assert_eq!(tree.children[0].children.len(), 1);
    assert_eq!(
        tree.children[0].children[0].name,
        "0x1::account::get_balance"
    );

    // Verify metadata.
    let metadata = profile.metadata.as_ref().unwrap();
    assert_eq!(metadata.transaction_version, Some(123456789));
    assert_eq!(metadata.gas_unit_price, Some(100));
}

#[test]
fn test_parse_gas_profile_json_minimal() {
    // Minimal valid JSON with defaults.
    let json_data = r#"{"gas_used": 100}"#;
    let profile = parse_gas_profile_json(json_data).expect("should parse minimal gas profile");
    assert_eq!(profile.gas_used, 100);
    assert!(profile.call_tree.is_none());
    assert!(profile.metadata.is_none());
}

#[test]
fn test_parse_gas_profile_json_invalid() {
    let result = parse_gas_profile_json("not json at all");
    assert!(result.is_err(), "should fail on invalid JSON");
    let err_msg = result.unwrap_err().to_string();
    assert!(
        err_msg.contains("failed to parse gas profile JSON"),
        "error message should be descriptive, got: {err_msg}"
    );
}

// ---------------------------------------------------------------------------
// Test 3: Convert Aptos trace entries to CodeTracer events
// ---------------------------------------------------------------------------

#[test]
fn test_aptos_trace_to_codetracer() {
    let entries = vec![
        AptosEnrichedEntry {
            trace: AptosTraceEntry {
                function_name: "0x1::coin::transfer".to_string(),
                pc: 0,
            },
            gas_cost: Some(5000),
            total_gas: Some(42000),
        },
        AptosEnrichedEntry {
            trace: AptosTraceEntry {
                function_name: "0x1::coin::transfer".to_string(),
                pc: 1,
            },
            gas_cost: Some(5000),
            total_gas: Some(42000),
        },
        AptosEnrichedEntry {
            trace: AptosTraceEntry {
                function_name: "0x1::coin::withdraw".to_string(),
                pc: 0,
            },
            gas_cost: Some(15000),
            total_gas: Some(20000),
        },
        AptosEnrichedEntry {
            trace: AptosTraceEntry {
                function_name: "0x1::coin::deposit".to_string(),
                pc: 0,
            },
            gas_cost: None,
            total_gas: None,
        },
    ];

    let tmp = tempfile::TempDir::new().expect("failed to create temp dir");
    let out_dir = tmp.path().join("ct-out");
    let source_path = Path::new("transfer.move");

    aptos_adapter::convert_aptos_trace(
        &entries,
        source_path,
        &out_dir,
        TraceEventsFileFormat::Json,
    )
    .expect("convert_aptos_trace should succeed");

    // Verify 3-file output.
    assert!(out_dir.join("trace.bin").exists(), "trace.bin should exist");
    assert!(
        out_dir.join("trace_metadata.json").exists(),
        "trace_metadata.json should exist"
    );
    assert!(
        out_dir.join("trace_paths.json").exists(),
        "trace_paths.json should exist"
    );

    // Verify trace.bin is non-empty.
    let trace_size = std::fs::metadata(out_dir.join("trace.bin"))
        .expect("trace.bin metadata")
        .len();
    assert!(trace_size > 0, "trace.bin should be non-empty");

    // Verify metadata is valid JSON with program field.
    let metadata_str =
        std::fs::read_to_string(out_dir.join("trace_metadata.json")).expect("read metadata");
    let metadata: serde_json::Value =
        serde_json::from_str(&metadata_str).expect("metadata should be valid JSON");
    assert!(
        metadata.get("program").is_some(),
        "metadata should have 'program' field"
    );
}

#[test]
fn test_aptos_trace_to_codetracer_empty() {
    let tmp = tempfile::TempDir::new().expect("failed to create temp dir");
    let out_dir = tmp.path().join("ct-out");
    let source_path = Path::new("empty.move");

    let result = aptos_adapter::convert_aptos_trace(
        &[],
        source_path,
        &out_dir,
        TraceEventsFileFormat::Json,
    );

    assert!(result.is_err(), "should fail with empty entries");
    let err_msg = result.unwrap_err().to_string();
    assert!(
        err_msg.contains("no trace entries"),
        "error should mention no entries, got: {err_msg}"
    );
}

// ---------------------------------------------------------------------------
// Test 4: Verify REST API response type parsing
// ---------------------------------------------------------------------------

#[test]
fn test_aptos_rest_api_types() {
    let json_data = r#"[
        {
            "type": "0x1::coin::CoinStore<0x1::aptos_coin::AptosCoin>",
            "data": {
                "coin": {
                    "value": "1000000"
                },
                "frozen": false,
                "deposit_events": {
                    "counter": "5",
                    "guid": {
                        "id": {
                            "addr": "0xabc",
                            "creation_num": "2"
                        }
                    }
                },
                "withdraw_events": {
                    "counter": "3",
                    "guid": {
                        "id": {
                            "addr": "0xabc",
                            "creation_num": "3"
                        }
                    }
                }
            }
        },
        {
            "type": "0x1::account::Account",
            "data": {
                "authentication_key": "0xdeadbeef",
                "sequence_number": "42"
            }
        }
    ]"#;

    let resources = parse_resources_response(json_data).expect("should parse resources response");
    assert_eq!(resources.len(), 2);

    // Verify CoinStore resource.
    assert_eq!(
        resources[0].resource_type,
        "0x1::coin::CoinStore<0x1::aptos_coin::AptosCoin>"
    );
    assert_eq!(
        resources[0].data["coin"]["value"].as_str().unwrap(),
        "1000000"
    );
    assert_eq!(resources[0].data["frozen"].as_bool().unwrap(), false);

    // Verify Account resource.
    assert_eq!(resources[1].resource_type, "0x1::account::Account");
    assert_eq!(
        resources[1].data["sequence_number"].as_str().unwrap(),
        "42"
    );
}

#[test]
fn test_aptos_rest_api_types_empty_array() {
    let resources = parse_resources_response("[]").expect("should parse empty array");
    assert!(resources.is_empty());
}

#[test]
fn test_aptos_rest_api_types_invalid() {
    let result = parse_resources_response("not json");
    assert!(result.is_err(), "should fail on invalid JSON");
}

// ---------------------------------------------------------------------------
// Test 5: Verify ledger_version parameter handling
// ---------------------------------------------------------------------------

#[test]
fn test_historical_state_fetching() {
    // Test URL construction with ledger_version parameter.
    let config = AptosRestConfig::mainnet();
    let address = "0x1";

    // Without ledger version.
    let url_no_version = config.resources_url(address, None);
    assert_eq!(
        url_no_version,
        "https://fullnode.mainnet.aptoslabs.com/v1/accounts/0x1/resources"
    );

    // With ledger version.
    let url_with_version = config.resources_url(address, Some(123456789));
    assert_eq!(
        url_with_version,
        "https://fullnode.mainnet.aptoslabs.com/v1/accounts/0x1/resources?ledger_version=123456789"
    );

    // Testnet config.
    let testnet = AptosRestConfig::testnet();
    let url_testnet = testnet.resources_url("0xabc", Some(999));
    assert_eq!(
        url_testnet,
        "https://fullnode.testnet.aptoslabs.com/v1/accounts/0xabc/resources?ledger_version=999"
    );

    // Local config.
    let local = AptosRestConfig::local();
    let url_local = local.resources_url("0x1", None);
    assert_eq!(
        url_local,
        "http://localhost:8080/v1/accounts/0x1/resources"
    );
}

// ---------------------------------------------------------------------------
// Test 6: Verify limitation documentation/error messages
// ---------------------------------------------------------------------------

#[test]
fn test_aptos_vs_sui_limitations() {
    let summary = aptos_adapter::aptos_limitations_summary();

    // Verify all key limitations are documented.
    assert!(
        summary.contains("NO VARIABLE VALUES"),
        "should document no variable values limitation"
    );
    assert!(
        summary.contains("NO STRUCTURED TRACE FORMAT"),
        "should document no structured trace format"
    );
    assert!(
        summary.contains("GAS-CENTRIC PROFILING"),
        "should document gas-centric profiling"
    );
    assert!(
        summary.contains("LIMITED SOURCE MAPPING"),
        "should document limited source mapping"
    );
    assert!(
        summary.contains("HISTORICAL STATE"),
        "should document historical state access"
    );

    // Verify it mentions Sui as the alternative.
    assert!(
        summary.contains("Sui"),
        "should reference Sui as the alternative for full debugging"
    );

    // Verify it mentions key technical details.
    assert!(
        summary.contains("MOVE_VM_TRACE"),
        "should mention MOVE_VM_TRACE"
    );
    assert!(
        summary.contains("flamegraph"),
        "should mention flamegraph format"
    );
    assert!(
        summary.contains("/v1/accounts/"),
        "should mention REST API endpoint"
    );
}

// ---------------------------------------------------------------------------
// Test 7: Verify replay configuration
// ---------------------------------------------------------------------------

#[test]
fn test_aptos_replay_config() {
    let config = AptosReplayConfig::new(123456789);

    assert_eq!(
        config.node_url,
        "https://fullnode.mainnet.aptoslabs.com/v1"
    );
    assert_eq!(config.txn_version, 123456789);
    assert!(config.source_dir.is_none());
    assert_eq!(
        config.out_dir,
        std::path::PathBuf::from("./ct-traces/")
    );
    assert!(config.profile_gas, "profile_gas should default to true");
}

#[test]
fn test_aptos_replay_config_custom() {
    let config = AptosReplayConfig {
        node_url: "http://localhost:8080/v1".to_string(),
        txn_version: 42,
        source_dir: Some(std::path::PathBuf::from("/tmp/sources")),
        out_dir: std::path::PathBuf::from("/tmp/output"),
        format: TraceEventsFileFormat::Json,
        profile_gas: false,
    };

    assert_eq!(config.node_url, "http://localhost:8080/v1");
    assert_eq!(config.txn_version, 42);
    assert_eq!(
        config.source_dir.as_ref().unwrap().to_str().unwrap(),
        "/tmp/sources"
    );
    assert!(!config.profile_gas);
}

// ---------------------------------------------------------------------------
// Test 8: Verify merging gas profiler data with trace data
// ---------------------------------------------------------------------------

#[test]
fn test_aptos_merge_gas_and_trace() {
    let trace_entries = vec![
        AptosTraceEntry {
            function_name: "0x1::coin::transfer".to_string(),
            pc: 0,
        },
        AptosTraceEntry {
            function_name: "0x1::coin::transfer".to_string(),
            pc: 1,
        },
        AptosTraceEntry {
            function_name: "0x1::coin::withdraw".to_string(),
            pc: 0,
        },
        AptosTraceEntry {
            function_name: "0x1::coin::withdraw".to_string(),
            pc: 1,
        },
        AptosTraceEntry {
            function_name: "0x1::coin::deposit".to_string(),
            pc: 0,
        },
        AptosTraceEntry {
            function_name: "unknown_function".to_string(),
            pc: 0,
        },
    ];

    let gas_profile = AptosGasProfile {
        gas_used: 42000,
        call_tree: Some(GasProfileNode {
            name: "0x1::coin::transfer".to_string(),
            gas_cost: 5000,
            total_gas: 42000,
            children: vec![
                GasProfileNode {
                    name: "0x1::coin::withdraw".to_string(),
                    gas_cost: 15000,
                    total_gas: 20000,
                    children: vec![],
                },
                GasProfileNode {
                    name: "0x1::coin::deposit".to_string(),
                    gas_cost: 17000,
                    total_gas: 17000,
                    children: vec![],
                },
            ],
        }),
        metadata: None,
    };

    let enriched = merge_trace_and_gas(&trace_entries, Some(&gas_profile));
    assert_eq!(enriched.len(), 6, "should produce one enriched entry per trace entry");

    // transfer entries should have gas data.
    assert_eq!(enriched[0].gas_cost, Some(5000));
    assert_eq!(enriched[0].total_gas, Some(42000));
    assert_eq!(enriched[1].gas_cost, Some(5000)); // Same function, same gas data.

    // withdraw entries should have gas data.
    assert_eq!(enriched[2].gas_cost, Some(15000));
    assert_eq!(enriched[2].total_gas, Some(20000));

    // deposit entry should have gas data.
    assert_eq!(enriched[4].gas_cost, Some(17000));
    assert_eq!(enriched[4].total_gas, Some(17000));

    // unknown_function should NOT have gas data (not in the gas profile).
    assert_eq!(enriched[5].gas_cost, None);
    assert_eq!(enriched[5].total_gas, None);
}

#[test]
fn test_aptos_merge_gas_and_trace_no_gas() {
    let trace_entries = vec![
        AptosTraceEntry {
            function_name: "0x1::module::func".to_string(),
            pc: 0,
        },
    ];

    // Merge without gas profile data.
    let enriched = merge_trace_and_gas(&trace_entries, None);
    assert_eq!(enriched.len(), 1);
    assert!(enriched[0].gas_cost.is_none());
    assert!(enriched[0].total_gas.is_none());
}

#[test]
fn test_aptos_replay_from_existing_data() {
    let trace_csv = "\
0x1::module::init,0
0x1::module::init,1
0x1::module::init,2
0x1::module::process,0
0x1::module::process,1
";

    let gas_json = r#"{
        "gas_used": 1000,
        "call_tree": {
            "name": "0x1::module::init",
            "gas_cost": 300,
            "total_gas": 600,
            "children": [
                {
                    "name": "0x1::module::process",
                    "gas_cost": 300,
                    "total_gas": 300,
                    "children": []
                }
            ]
        }
    }"#;

    let tmp = tempfile::TempDir::new().expect("failed to create temp dir");
    let out_dir = tmp.path().join("ct-out");
    let source_path = Path::new("aptos_module.move");

    // Test with gas data.
    aptos_replay::aptos_replay_from_existing_data(
        trace_csv,
        Some(gas_json),
        source_path,
        &out_dir,
        TraceEventsFileFormat::Json,
    )
    .expect("aptos_replay_from_existing_data should succeed");

    // Verify 3-file output.
    assert!(out_dir.join("trace.bin").exists());
    assert!(out_dir.join("trace_metadata.json").exists());
    assert!(out_dir.join("trace_paths.json").exists());

    let trace_size = std::fs::metadata(out_dir.join("trace.bin"))
        .unwrap()
        .len();
    assert!(trace_size > 0, "trace.bin should be non-empty");
}

#[test]
fn test_aptos_replay_from_existing_data_no_gas() {
    let trace_csv = "0x1::simple::call,0\n0x1::simple::call,1\n";

    let tmp = tempfile::TempDir::new().expect("failed to create temp dir");
    let out_dir = tmp.path().join("ct-out-no-gas");
    let source_path = Path::new("simple.move");

    // Test without gas data.
    aptos_replay::aptos_replay_from_existing_data(
        trace_csv,
        None,
        source_path,
        &out_dir,
        TraceEventsFileFormat::Json,
    )
    .expect("should succeed without gas data");

    assert!(out_dir.join("trace.bin").exists());
    assert!(out_dir.join("trace_metadata.json").exists());
    assert!(out_dir.join("trace_paths.json").exists());
}

#[test]
fn test_aptos_replay_from_existing_data_empty_trace() {
    let tmp = tempfile::TempDir::new().expect("failed to create temp dir");
    let out_dir = tmp.path().join("ct-out-empty");
    let source_path = Path::new("empty.move");

    let result = aptos_replay::aptos_replay_from_existing_data(
        "",
        None,
        source_path,
        &out_dir,
        TraceEventsFileFormat::Json,
    );

    assert!(result.is_err(), "should fail with empty trace");
    let err_msg = result.unwrap_err().to_string();
    assert!(
        err_msg.contains("no trace entries"),
        "error should mention no entries, got: {err_msg}"
    );
}
