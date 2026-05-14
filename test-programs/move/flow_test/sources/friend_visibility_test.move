/// Friend-visibility fixture for the Move recorder.
///
/// Move's `friend` declaration grants a peer module access to
/// `public(friend) fun` declarations that are otherwise invisible
/// outside the defining module.  The recorder must surface a
/// cross-module call from a friend caller (`auth::query`) to a
/// `public(friend)` callee (`secrets::reveal`) as a normal
/// `call_entry` / `call_exit` pair, with both functions appearing
/// distinctly in the function table — visibility annotations are a
/// compile-time concern that should leave no trace at the recorder
/// level beyond the call boundary itself.
///
/// The strict pin lives at
/// `tests/test_full_coverage.rs::test_friend_visibility_test_via_ct_print_full`.
module flow_test::secrets {
    friend flow_test::auth;

    /// `public(friend)` — only modules listed via `friend` may call
    /// this.  Returns the canonical answer.
    public(friend) fun reveal(): u64 {
        42
    }
}

module flow_test::auth {
    use flow_test::secrets;

    /// Cross the friend boundary into `secrets::reveal` and surface
    /// the value to the caller.
    public fun query(): u64 {
        secrets::reveal()
    }

    #[test]
    fun test_friend_visibility() {
        let v = query();
        assert!(v == 42, 0);
    }
}
