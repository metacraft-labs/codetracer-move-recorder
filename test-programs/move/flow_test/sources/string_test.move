/// `std::string::String` fixture for the Move recorder.
///
/// Pins that a `String` flowing through `string::utf8`, `string::append`,
/// `string::sub_string`, `string::length` surfaces as a typed
/// `ValueRecord::Struct` whose single `bytes: vector<u8>` field is a
/// typed `ValueRecord::Sequence<u8>` with the printable text recoverable
/// from the byte payload.  See
/// `tests/test_full_coverage.rs::test_string_test_via_ct_print_full`
/// for the strict shape pin.
module flow_test::string_test {
    use std::string;

    #[test]
    fun test_string() {
        let s: string::String = string::utf8(b"hello");
        let suffix: string::String = string::utf8(b" world");
        string::append(&mut s, suffix);
        let head: string::String = string::sub_string(&s, 0, 5);
        let n: u64 = string::length(&s);
        assert!(n == 11, 0);
        let h_bytes = string::bytes(&head);
        assert!(std::vector::length(h_bytes) == 5, 0);
    }
}
