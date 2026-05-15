/// Multi-ability generic-constraints fixture for the Move recorder.
///
/// Move generic functions can carry ability constraints (`T: copy +
/// drop`, `T: store + key`).  Different instantiations of the same
/// generic must each register distinguishable runtime type identity
/// for any container struct that captures the constrained type — see
/// the M9 spec deliverable for the test_generic_constraints_test
/// fixture.  The recorder achieves this via the parameterised struct
/// key (`Container<u64>` vs `Container<bool>` in the type table).
///
/// The strict pin lives at
/// `tests/test_full_coverage.rs::test_generic_constraints_test_via_ct_print_full`.
module flow_test::generic_constraints_test {

    /// Container parameterised by a type with the multi-ability
    /// constraint `T: copy + drop + store`.
    public struct Container<T: copy + drop + store> has copy, drop {
        inner: T,
    }

    /// Box up an instance of `T` with the multi-ability constraint.
    public fun store_value<T: copy + drop + store>(v: T): Container<T> {
        Container<T> { inner: v }
    }

    /// Linear consume of a `T: drop` value (a weaker constraint than
    /// `store_value`).  Models the `discard<T: drop>` shape from the
    /// M9 spec.
    public fun discard<T: drop>(_v: T) {}

    #[test]
    fun test_generic_constraints() {
        // Two distinct instantiations of `store_value<T>` against
        // primitives that satisfy `copy + drop + store`.
        let cu: Container<u64> = store_value<u64>(42);
        let cb: Container<bool> = store_value<bool>(true);

        // A `discard<T: drop>` call against a third type to exercise
        // the weaker constraint shape.
        discard<u64>(7);

        // Burn the containers so the bytecode compiler keeps the
        // bindings alive.
        let Container<u64> { inner: ru } = cu;
        let Container<bool> { inner: rb } = cb;
        assert!(ru == 42 && rb, 0);
    }
}
