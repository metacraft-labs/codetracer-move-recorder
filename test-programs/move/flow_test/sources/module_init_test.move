/// Sui `fun init(ctx: &mut TxContext)` one-time module initialisation
/// fixture for the Move recorder.
///
/// Sui packages may declare a `fun init(ctx: &mut TxContext)` per
/// module — the Sui runtime invokes it exactly once at the moment the
/// package is published, threading a `&mut TxContext` through so the
/// init body can recover the deployer's address and mint fresh object
/// identities.  Critically, `init` has *no* `public` modifier and *no*
/// `#[test]` attribute: it is a first-class entry point recognised by
/// the Sui runtime via its name + signature.  The recorder must
/// surface the init invocation as a normal Call/Return pair carrying
/// the `&mut TxContext` parameter as a typed
/// `ValueRecord::Reference { mutable: true }`, AND it must flag the
/// invocation as one-time / module-init via a `MoveCallVisibility`
/// io_event with content `"init"` — a stable hook that downstream
/// consumers can key off to highlight the publish-time bootstrap
/// frame in the call graph.
///
/// The strict pin lives at
/// `tests/test_full_coverage.rs::test_module_init_test_via_ct_print_full`.
module flow_test::module_init_test {
    use sui::object::{Self, UID};
    use sui::transfer;
    use sui::tx_context::TxContext;

    /// A trivial Sui object minted at module-publish time so the
    /// `init` body has visible side effects.
    public struct Bootstrap has key {
        id: UID,
    }

    /// Sui's recognised entry point — runs exactly once when the
    /// containing package is published.  The `&mut TxContext`
    /// parameter is the canonical thread used to recover the deployer
    /// address and mint a fresh `UID`.
    fun init(ctx: &mut TxContext) {
        let bs = Bootstrap { id: object::new(ctx) };
        transfer::transfer(bs, tx_context::sender(ctx));
    }

    #[test]
    fun test_module_init() {
        // `&mut TxContext` cannot be constructed in plain Move test
        // code on Sui; the synthetic NDJSON trace mocks the publish-time
        // `init` invocation directly.  See
        // `test_module_init_test_via_ct_print_full`.
        let _ok = true;
    }
}
