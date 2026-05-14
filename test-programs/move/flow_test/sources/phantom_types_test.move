/// Phantom-type-parameter fixture for the Move recorder.
///
/// `struct TypedCoin<phantom T> has store { value: u64 }` is the
/// canonical Move pattern for tagging an otherwise-uniform payload
/// with a compile-time currency / capability marker (`USD`, `EUR`,
/// ...).  At runtime every `TypedCoin<T>` shares the same layout, so
/// without a type-args-aware type-table key the recorder would collapse
/// distinct phantom instantiations into a single `TypeId` and downstream
/// consumers (ct-print, frontend object inspector) would lose the tag.
///
/// The strict pin lives at
/// `tests/test_full_coverage.rs::test_phantom_types_test_via_ct_print_full`.
module flow_test::phantom_types_test {

    /// Phantom currency tags.  No fields — they exist only to
    /// distinguish `TypedCoin<USD>` from `TypedCoin<EUR>` at the
    /// type level.
    public struct USD {}
    public struct EUR {}

    /// Generic coin parameterised by a phantom currency tag.
    public struct TypedCoin<phantom T> has store {
        value: u64,
    }

    /// Mint a `TypedCoin<T>` for a phantom currency tag `T`.
    public fun mint<T>(value: u64): TypedCoin<T> {
        TypedCoin<T> { value }
    }

    /// Read the value of a coin without consuming it.
    public fun value<T>(c: &TypedCoin<T>): u64 {
        c.value
    }

    /// Burn a coin, returning its value.
    public fun burn<T>(c: TypedCoin<T>): u64 {
        let TypedCoin<T> { value } = c;
        value
    }

    #[test]
    fun test_phantom_types() {
        let usd_coin: TypedCoin<USD> = mint<USD>(100);
        let eur_coin: TypedCoin<EUR> = mint<EUR>(100);
        let usd_v = value<USD>(&usd_coin);
        let eur_v = value<EUR>(&eur_coin);
        assert!(usd_v == eur_v, 0);
        let burned_usd = burn<USD>(usd_coin);
        let burned_eur = burn<EUR>(eur_coin);
        assert!(burned_usd == burned_eur, 0);
    }
}
