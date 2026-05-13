/// Variant / enum constructors fixture for the Move recorder.
///
/// Exercises Sui Move 2024 enum syntax (public enum) and the standard
/// library's `Option<T>` shape so the recorder's `Variant` conversion
/// path is reachable.  The canonical recorder output for a Move
/// variant is a typed `ValueRecord::Variant { discriminator, contents,
/// type_id }` carrying the unit / tuple payload as a `Struct` inside
/// `contents` — see
/// `tests/test_full_coverage.rs::test_variant_constructors_via_ct_print_full`
/// for the strict shape pin.
///
/// Without the variant fix, the recorder fell through to a
/// printed-form `ValueRecord::String { text: "Variant#N(...)" }` —
/// the original M8 known limitation.
module flow_test::variant_constructors_test {
    use std::option;

    // -----------------------------------------------------------------------
    // A two-variant enum demonstrating unit + tuple payloads.
    // -----------------------------------------------------------------------

    public enum Shape has copy, drop {
        /// Unit variant: no payload.
        Circle,
        /// Tuple variant: a width and a height.
        Rect(u64, u64),
    }

    /// Build a `Some(42)` Option value and surface it as the canonical
    /// trace-level Variant whose discriminator is `1` (Some).
    fun make_some(): option::Option<u64> {
        option::some(42)
    }

    /// Build a `None` Option value (discriminator `0`).
    fun make_none(): option::Option<u64> {
        option::none<u64>()
    }

    /// Build a `Rect(3, 5)` Shape variant.
    fun make_rect(): Shape {
        Shape::Rect(3, 5)
    }

    #[test]
    fun test_variant_constructors() {
        let some = make_some();
        let none = make_none();
        let rect = make_rect();
        // Pattern-match the rect into its tuple payload.
        let area = match (rect) {
            Shape::Circle => 0,
            Shape::Rect(w, h) => w * h,
        };
        assert!(area == 15, 0);
        // Burn the Option values so they aren't dead-code-eliminated.
        let _: u64 = option::destroy_with_default(some, 0);
        option::destroy_none(none);
    }
}
