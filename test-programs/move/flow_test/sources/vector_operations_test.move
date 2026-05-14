/// `std::vector` advanced operations fixture for the Move recorder.
///
/// Pins that each mutating + observational vector op surfaces a
/// before/after `ValueRecord::Sequence` snapshot of the vector's
/// contents through Effect::Write events.  `vector::index_of` returns
/// `(bool, u64)` — the harness asserts on both halves directly through
/// the recorder's tuple-return path.  See
/// `tests/test_full_coverage.rs::test_vector_operations_test_via_ct_print_full`
/// for the strict shape pin.
module flow_test::vector_operations_test {
    use std::vector;

    #[test]
    fun test_vector_operations() {
        let v: vector<u64> = vector[10, 20, 30, 40];
        // swap_remove(v, 1): removes index 1 (=20) by swapping with the last
        // element. Result: v = [10, 40, 30], removed = 20.
        let removed = vector::swap_remove(&mut v, 1);
        assert!(removed == 20, 0);
        // pop_back(v): pops 30. Result: v = [10, 40], popped = 30.
        let popped = vector::pop_back(&mut v);
        assert!(popped == 30, 0);
        // contains(v, 40) -> true; contains(v, 99) -> false.
        let has_40 = vector::contains(&v, &40);
        let has_99 = vector::contains(&v, &99);
        assert!(has_40, 0);
        assert!(!has_99, 0);
        // reverse(v): v = [40, 10].
        vector::reverse(&mut v);
        // append(v, [7, 8]): v = [40, 10, 7, 8].
        let other: vector<u64> = vector[7, 8];
        vector::append(&mut v, other);
        // index_of(v, &7) -> (true, 2).
        let (found, idx) = vector::index_of(&v, &7);
        assert!(found, 0);
        assert!(idx == 2, 0);
        // borrow_mut(v, 0) and overwrite via dereference assignment.
        let r = vector::borrow_mut(&mut v, 0);
        *r = 100;
        // Final v = [100, 10, 7, 8].
        assert!(*vector::borrow(&v, 0) == 100, 0);
    }
}
