/// Wide-integer fixture for the Move recorder.
///
/// The pre-existing `test_boolean_and_integers` fixture had the Sui Move
/// VM constant-fold every wide-integer let-binding before they reached
/// the trace (because their results only fed dead `assert!` calls), so
/// the recorder's u128 / u256 paths went uncovered by an end-to-end
/// fixture.  This fixture **uses** each integer's value through a chain
/// of operations and a return so the constant-folder can't elide them.
///
/// The corresponding strict test pin lives at
/// `tests/test_full_coverage.rs::test_wide_integer_via_ct_print_full`
/// and uses a synthetic NDJSON trace that exercises u8/u16/u32/u64/u128
/// in one shot — it asserts the typed `Int` / `BigInt` payloads on the
/// merged step's vars.
module flow_test::wide_integer_test {
    /// Multiply every operand and *return* the wide product so the
    /// Sui compiler cannot reduce the binding to a constant in the
    /// constant-fold pass.  This is the canonical Move pattern for
    /// surfacing every integer width through the trace.
    public fun wide_product(
        a: u8,
        b: u16,
        c: u32,
        d: u64,
        e: u128,
    ): u128 {
        let wa = (a as u128);
        let wb = (b as u128);
        let wc = (c as u128);
        let wd = (d as u128);
        wa * wb * wc * wd * e
    }

    #[test]
    fun test_wide_integer() {
        let a: u8 = 7;
        let b: u16 = 11;
        let c: u32 = 13;
        let d: u64 = 17;
        // 18 * 1e18 fits in u128 but exceeds i64::MAX, forcing a BigInt.
        let e: u128 = 18_000_000_000_000_000_000;
        let p = wide_product(a, b, c, d, e);
        // 7 * 11 * 13 * 17 = 17017; 17017 * e = 306_306_000_000_000_000_000_000.
        assert!(p == 306_306_000_000_000_000_000_000, 0);
    }
}
