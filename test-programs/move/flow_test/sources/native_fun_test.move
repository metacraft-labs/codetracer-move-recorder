/// `native fun` declarations fixture for the Move recorder.
///
/// `native fun` is the Move-language escape hatch into Rust-implemented
/// stdlib primitives (`std::vector::length`, `std::hash::sha2_256`,
/// ...).  The Move VM has no source bytecode for a native body, so a
/// native call's lifecycle in the trace is exactly an `OpenFrame` with
/// `is_native: true` followed by a `CloseFrame` carrying the native's
/// return value — no `Instruction` events fire between the two.  The
/// recorder must therefore surface each native call as a Call/Return
/// pair that brackets *zero* `step` events, and the function table
/// must list the native by name so consumers can distinguish a Move
/// stdlib hop from a user-defined function call.
///
/// The strict pin lives at
/// `tests/test_full_coverage.rs::test_native_fun_test_via_ct_print_full`.
module flow_test::native_fun_test {

    #[test]
    fun test_native_fun() {
        // `vector::length` is a paradigmatic Move stdlib native — its
        // body lives in Rust, the Move source for the module merely
        // declares `native fun length<Element>(v: &vector<Element>): u64`.
        let v: vector<u8> = b"abcde";
        let n = std::vector::length(&v);
        assert!(n == 5, 0);
    }
}
