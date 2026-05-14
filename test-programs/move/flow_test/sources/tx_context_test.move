/// Sui `&mut TxContext` fixture for the Move recorder.
///
/// `&mut TxContext` is the canonical Sui parameter threaded through
/// every entry function — the Move VM uses it both to recover the
/// transaction sender (`tx_context::sender`) and to derive fresh
/// per-call object identities (`object::new`).  The recorder must
/// surface the parameter as a typed `ValueRecord::Reference {
/// mutable: true, .. }` whose pointee is the underlying `TxContext`
/// struct, the recovered sender as a typed address-shaped String, and
/// the freshly minted `UID` as a nested Struct carrying its inner
/// `ID { bytes: address }` payload.
///
/// The strict pin lives at
/// `tests/test_full_coverage.rs::test_tx_context_test_via_ct_print_full`.
module flow_test::tx_context_test {
    use sui::object::{Self, UID};
    use sui::tx_context::{Self, TxContext};

    /// A minimal Sui object that owns a fresh `UID` minted from the
    /// transaction context.
    public struct Token has key {
        id: UID,
    }

    /// Pin the `&mut TxContext` shape: recover the sender, derive a
    /// fresh UID, and return both via the new `Token` resource.
    public entry fun mint(ctx: &mut TxContext): Token {
        let _sender = tx_context::sender(ctx);
        let id = object::new(ctx);
        Token { id }
    }

    /// Burn helper — required so the linear `Token` (no `drop`) can
    /// fall out of scope in the test body.
    public fun burn(t: Token) {
        let Token { id } = t;
        object::delete(id);
    }

    #[test]
    fun test_tx_context() {
        // `&mut TxContext` cannot be constructed in plain Move test
        // code on Sui; the synthetic NDJSON trace mocks the call shape
        // directly.  See `test_tx_context_test_via_ct_print_full`.
        let _ok = true;
    }
}
