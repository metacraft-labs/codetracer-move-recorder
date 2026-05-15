/// Aptos `0x1::table::Table` fixture for the Move recorder.
///
/// `aptos_std::table::Table<K, V>` is the Aptos canonical
/// arbitrary-key map.  A `Table` is a struct holding a single
/// `handle: address` field that the Aptos VM uses to look up the
/// underlying storage.  The recorder must surface each `table::*`
/// call as a balanced Call/Return pair, the `Table` itself must
/// surface as `ValueRecord::Struct` with its `handle` field captured,
/// and contained values must surface with their declared runtime type.
///
/// The strict pin lives at
/// `tests/test_full_coverage.rs::test_table_test_via_ct_print_full`.
module flow_test::table_test {
    use aptos_std::table::{Self, Table};

    /// Aptos resource holding a Table<address, u64>.
    public struct Registry has key {
        entries: Table<address, u64>,
    }

    #[test]
    fun test_table() {
        let mut entries = table::new<address, u64>();
        let addr1: address = @0xAB;
        table::add(&mut entries, addr1, 100u64);
        let v: u64 = *table::borrow(&entries, addr1);
        let present: bool = table::contains(&entries, addr1);
        assert!(v == 100 && present, 0);
        // Drop the table — drop is allowed on an empty table only in
        // the synthetic fixture.  Real code would `move_to(registry)`.
        let _ = entries;
    }
}
