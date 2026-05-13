/// Move ability-matrix fixture for the recorder.
///
/// Move's four abilities — `copy`, `drop`, `store`, `key` — govern how
/// values of a struct type may be used.  In particular, a struct *without*
/// `drop` is a "hot potato" that must be consumed exactly once
/// (Move's flagship linear-types feature); the recorder must surface its
/// lifecycle from construction through consumption.
///
/// The strict pin lives at
/// `tests/test_full_coverage.rs::test_abilities_via_ct_print_full`.
module flow_test::abilities_test {

    /// Hot potato: no abilities (no `drop` ==> must be consumed; no
    /// `copy` ==> moves are linear; no `store` ==> cannot live in
    /// global storage).  This is the canonical Move pattern for an
    /// access-checked operation that must thread through the caller.
    public struct AccessToken {
        operation_id: u64,
    }

    /// Mint a hot-potato AccessToken.
    public fun mint_token(operation_id: u64): AccessToken {
        AccessToken { operation_id }
    }

    /// Consume the hot potato, returning its operation_id.
    public fun consume_token(t: AccessToken): u64 {
        let AccessToken { operation_id } = t;
        operation_id
    }

    /// A copy + drop struct — both copies and silent drops are legal.
    public struct Datum has copy, drop {
        x: u64,
    }

    /// A store-only struct — must be explicitly destructured but can
    /// live in global storage of resources that have `key`.
    public struct StorageItem has store {
        payload: u64,
    }

    /// Destroy a StorageItem (required because it lacks `drop`).
    public fun destroy_storage_item(s: StorageItem): u64 {
        let StorageItem { payload } = s;
        payload
    }

    #[test]
    fun test_abilities() {
        // Hot potato: minted and consumed in the same function — the
        // linearity invariant enforces this.
        let token = mint_token(7);
        let op = consume_token(token);
        assert!(op == 7, 0);

        // Copy + drop: a single binding produces several Move VM copies.
        let d = Datum { x: 42 };
        let d2 = d; // copy
        assert!(d.x == 42, 0);
        assert!(d2.x == 42, 0);

        // Store-only: explicit destructure required.
        let s = StorageItem { payload: 99 };
        let payload = destroy_storage_item(s);
        assert!(payload == 99, 0);
    }
}
