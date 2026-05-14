/// Standard library `Option<T>` fixture for the Move recorder.
///
/// Exercises the `std::option::Option` API: `some`, `none`,
/// `is_some`, `is_none`, `borrow`, `extract`, `destroy_some`.
/// The recorder must surface every Option value as a typed
/// `ValueRecord::Variant` (`0x1::option::Option::Variant#1` for `Some`,
/// `0x1::option::Option::Variant#0` for `None`) and `option::borrow`'s
/// `&u64` argument as a typed `ValueRecord::Reference`.  See
/// `tests/test_full_coverage.rs::test_option_test_via_ct_print_full`
/// for the strict shape pin.
module flow_test::option_test {
    use std::option;

    /// Borrow the inner u64 of a `Some` option through `option::borrow`,
    /// returning a copy.
    fun borrow_inner(opt: &option::Option<u64>): u64 {
        *option::borrow(opt)
    }

    #[test]
    fun test_option() {
        let some_val: option::Option<u64> = option::some<u64>(42);
        let none_val: option::Option<u64> = option::none<u64>();
        // Discriminator probes.
        let s_is_some: bool = option::is_some(&some_val);
        let n_is_none: bool = option::is_none(&none_val);
        assert!(s_is_some, 0);
        assert!(n_is_none, 0);
        // Reference borrow + readback.
        let copied: u64 = borrow_inner(&some_val);
        assert!(copied == 42, 0);
        // Linear consumption.
        let extracted: u64 = option::extract(&mut some_val);
        assert!(extracted == 42, 0);
        option::destroy_none(some_val);
        option::destroy_none(none_val);
    }
}
