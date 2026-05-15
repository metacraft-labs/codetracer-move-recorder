/// Sui dynamic-field fixture for the Move recorder.
///
/// `sui::dynamic_field::add` / `borrow` / `remove` is the canonical Sui
/// shape for attaching arbitrary key→value pairs to an existing object's
/// `UID` at runtime.  The recorder must surface each dynamic-field
/// operation as a Call/Return pair, the byte-vector key must surface as
/// a typed `ValueRecord::Sequence<u8>`, and the dynamic-field value
/// must surface with its registered runtime type.
///
/// The strict pin lives at
/// `tests/test_full_coverage.rs::test_dynamic_field_test_via_ct_print_full`.
module flow_test::dynamic_field_test {
    use sui::object::{Self, UID};
    use sui::dynamic_field;
    use sui::tx_context::TxContext;

    /// Sui object that holds dynamic fields under its UID.
    public struct Container has key {
        id: UID,
    }

    #[test]
    fun test_dynamic_field(ctx: &mut TxContext) {
        let mut parent = Container { id: object::new(ctx) };
        dynamic_field::add(&mut parent.id, b"key1", 42u64);
        let v: u64 = *dynamic_field::borrow<vector<u8>, u64>(&parent.id, b"key1");
        let removed: u64 = dynamic_field::remove(&mut parent.id, b"key1");
        assert!(v == 42 && removed == 42, 0);
        let Container { id } = parent;
        object::delete(id);
    }
}
