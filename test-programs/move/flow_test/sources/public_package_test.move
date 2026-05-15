/// Move 2024 `public(package) fun` visibility fixture for the Move recorder.
///
/// Move 2024 introduces `public(package) fun` — a callee that any module
/// in the *same package* may call (regardless of an explicit `friend`
/// declaration), but no module in any *other* package can call.  This
/// is the modern, package-scoped replacement for the older
/// `public(friend)` mechanism.  The recorder must surface a call across
/// the package boundary as a normal `call_entry` / `call_exit` pair,
/// AND it must surface the visibility of the callee in the trace
/// (so downstream consumers can distinguish a `public(package)` call
/// from a `public` / `public(friend)` / `friend` call) — visibility is
/// metadata that the strict pin asserts as a separate `MoveCallVisibility`
/// io_event keyed by the qualified callee name.
///
/// The strict pin lives at
/// `tests/test_full_coverage.rs::test_public_package_test_via_ct_print_full`.
module flow_test::pkg_lib {
    /// `public(package)` — only modules in the same package may call this.
    /// The recorder surfaces the visibility tag (`"public(package)"`)
    /// alongside the call_entry so downstream consumers can recover the
    /// visibility-class metadata that the bytecode preserves.
    public(package) fun helper(): u64 {
        7
    }
}

module flow_test::pkg_app {
    use flow_test::pkg_lib;

    /// Call the `public(package)` helper from a peer module in the same
    /// package — the call must succeed and surface as a normal Call/
    /// Return pair across the package boundary.
    public fun call_helper(): u64 {
        pkg_lib::helper()
    }

    #[test]
    fun test_public_package() {
        let v = call_helper();
        assert!(v == 7, 0);
    }
}
