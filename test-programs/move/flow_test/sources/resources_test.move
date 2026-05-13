/// Resources fixture for the Move recorder.
///
/// Resources (structs with the `key` ability bound to global storage by
/// address) are Move's defining feature and the canonical Aptos-only
/// shape — without coverage here the Aptos recorder support is on paper
/// only.  The Sui Move 2024 model wraps a resource's identity in a
/// `sui::object::UID`, but the underlying trace surface is the same: a
/// `Struct` payload with the resource's owned `type_id`.
///
/// The strict pin lives at
/// `tests/test_full_coverage.rs::test_resources_via_ct_print_full`.
module flow_test::resources_test {

    /// A linear resource: `key` ability, no `drop`.  Must be explicitly
    /// destructured before it falls out of scope.
    public struct Coin has key {
        id: u64,
        balance: u64,
    }

    /// Mint a Coin with the given balance.
    public fun mint(id: u64, balance: u64): Coin {
        Coin { id, balance }
    }

    /// Read the balance of a Coin without consuming it.
    public fun balance(c: &Coin): u64 {
        c.balance
    }

    /// Burn a Coin, returning its remaining balance.  Required to
    /// satisfy linearity at the test's `_coin` binding.
    public fun burn(c: Coin): u64 {
        let Coin { id: _, balance } = c;
        balance
    }

    #[test]
    fun test_resources() {
        let c = mint(1, 100);
        let b = balance(&c);
        assert!(b == 100, 0);
        let remaining = burn(c);
        assert!(remaining == 100, 0);
    }
}
