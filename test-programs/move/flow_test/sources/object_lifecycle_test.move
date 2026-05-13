/// Sui object-lifecycle fixture for the Move recorder.
///
/// `object::new` / `transfer::transfer` / `transfer::share_object` is the
/// canonical Sui shape for any non-trivial Move application.  This
/// fixture instantiates a small object with a `sui::object::UID` identity
/// field, mutates it through a mutable reference, and transfers it to an
/// address — exercising the recorder's struct-with-UID and `External`
/// (transfer side effect) paths.
///
/// The strict pin lives at
/// `tests/test_full_coverage.rs::test_object_lifecycle_via_ct_print_full`.
/// We compile this against the Sui stdlib (already in
/// `Move.toml`'s build environment) but the synthetic NDJSON does not
/// require live Sui APIs — it shapes the trace to mirror what
/// `sui move test --trace-execution` would emit for the same source.
module flow_test::object_lifecycle_test {
    use sui::object::{Self, UID};
    use sui::transfer;
    use sui::tx_context::TxContext;

    /// Sui object with a `key` ability and a `UID` identity.
    public struct Counter has key {
        id: UID,
        value: u64,
    }

    /// Create a Counter with starting value 0 and transfer it to the
    /// transaction sender.  Standard Sui object-construction shape.
    public entry fun create(ctx: &mut TxContext) {
        let counter = Counter { id: object::new(ctx), value: 0 };
        transfer::transfer(counter, tx_context::sender(ctx));
    }

    /// Mutate the counter through a mutable reference.
    public fun increment(c: &mut Counter, by: u64) {
        c.value = c.value + by;
    }

    /// Read the counter's value without consuming it.
    public fun value(c: &Counter): u64 {
        c.value
    }

    /// Destroy a Counter (test-only — production code would `freeze` /
    /// `transfer` it instead of unpacking).
    public fun destroy(c: Counter) {
        let Counter { id, value: _ } = c;
        object::delete(id);
    }
}
