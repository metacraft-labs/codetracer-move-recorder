/// `bcs::to_bytes` + `hash::sha2_256` + `hash::sha3_256` fixture for
/// the Move recorder.
///
/// Pins that each std-library native call surfaces with its full
/// byte-vector argument and digest return as typed
/// `ValueRecord::Sequence<u8>` entries — no truncation, no printed-form
/// fallback.  See `tests/test_full_coverage.rs::test_hash_builtins_test_via_ct_print_full`
/// for the strict shape pin.
module flow_test::hash_builtins_test {
    use std::bcs;
    use std::hash;

    /// A trivial payload struct: BCS-serializing it yields exactly
    /// `[3, 0, 0, 0, 0, 0, 0, 0, 4, 0, 0, 0, 0, 0, 0, 0]` (LE u64 + LE u64).
    public struct Point has copy, drop {
        x: u64,
        y: u64,
    }

    #[test]
    fun test_hash_builtins() {
        let p = Point { x: 3, y: 4 };
        let bytes: vector<u8> = bcs::to_bytes(&p);
        let d2: vector<u8> = hash::sha2_256(bytes);
        let bytes2: vector<u8> = bcs::to_bytes(&p);
        let d3: vector<u8> = hash::sha3_256(bytes2);
        // Touch the digests so the optimiser can't drop them.
        let len2 = std::vector::length(&d2);
        let len3 = std::vector::length(&d3);
        assert!(len2 == 32, 0);
        assert!(len3 == 32, 0);
    }
}
