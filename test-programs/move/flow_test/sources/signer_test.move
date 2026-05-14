/// `&signer` permission-checking fixture for the Move recorder
/// (Aptos shape).
///
/// Move's `signer` is the canonical capability handle for "the
/// transaction sender authorised this call".  Aptos modules typically
/// take `&signer` and gate sensitive operations behind a comparison
/// against a hard-coded admin address — `signer::address_of(admin) ==
/// @ADMIN`.  The recorder must surface the `&signer` parameter as a
/// typed `ValueRecord::Reference` whose pointee is the underlying
/// `Signer { address }` struct, and the comparison's boolean result
/// must surface as a typed `ValueRecord::Bool`.
///
/// The strict pin lives at
/// `tests/test_full_coverage.rs::test_signer_test_via_ct_print_full`.
module flow_test::signer_test {
    use std::signer;

    /// The canonical admin address — only this signer is authorised
    /// to call `authorize`.
    const ADMIN: address = @0xA11CE;

    /// Compare the signer's address against the admin constant and
    /// return the boolean verdict.  This is the canonical Aptos
    /// access-check shape — exposed as `entry` so the Aptos VM can
    /// thread the active signer in.
    public entry fun authorize(admin: &signer, target: address): bool {
        let sender = signer::address_of(admin);
        let _ = target; // unused in the toy example, but pinned for arg shape
        sender == ADMIN
    }

    #[test]
    fun test_signer() {
        // `&signer` cannot be constructed in test code on Aptos; the
        // synthetic NDJSON trace mocks the &signer arg shape directly.
        // The test driver (test_full_coverage.rs) invokes the recorder
        // against the synthetic trace and pins the recorder's surface
        // — see `test_signer_test_via_ct_print_full`.
        let _ok = true;
    }
}
