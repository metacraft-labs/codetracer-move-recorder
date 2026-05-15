/// Move address-literal fixture for the recorder.
///
/// Move addresses can be written as hex literals (=@0x1cafe=) or as
/// named addresses defined in the package's =Move.toml= (=@flow_test=,
/// =@std=).  Both forms resolve at compile time to the canonical
/// 32-byte address payload that the Move VM stores.  The recorder
/// must capture the exact hex address text for each literal — the
/// same =ValueRecord::String= shape used elsewhere by the recorder
/// for typed `address` payloads (see =signer_test=, =tx_context_test=,
/// =object_lifecycle_test=, =table_test= for the same convention).
///
/// The strict pin lives at
/// `tests/test_full_coverage.rs::test_address_literals_test_via_ct_print_full`.
module flow_test::address_literals_test {

    /// Use each address through a function to keep the bytecode
    /// compiler from constant-folding them away.  Each call surfaces
    /// the address as a typed argument in the trace.
    fun id_addr(a: address): address { a }

    #[test]
    fun test_address_literals() {
        let a: address = @0x1cafe;
        let b: address = @flow_test;
        let c: address = @std;
        let _ra = id_addr(a);
        let _rb = id_addr(b);
        let _rc = id_addr(c);
    }
}
