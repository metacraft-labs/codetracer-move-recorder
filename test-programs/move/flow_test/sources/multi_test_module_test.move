/// Multi-#[test]-per-module fixture for the Move recorder.
///
/// `sui move test --trace` produces one NDJSON trace per `#[test]`
/// function in the module.  The recorder must process each trace
/// independently, registering each test body's call graph in its own
/// function table without cross-contamination from peer tests.
///
/// This fixture defines three independent `#[test]` functions in a
/// single module, each exercising a different recorder code path:
///   * `test_arithmetic` calls a pure `add(a, b): u64` helper.
///   * `test_resource_lifecycle` constructs and destructures a
///     small `Counter` struct via a `make_counter` helper.
///   * `test_event_emit` invokes `sui::event::emit` with a typed
///     payload.
///
/// The strict pin lives at
/// `tests/test_full_coverage.rs::test_multi_test_module_test_via_ct_print_full`.
module flow_test::multi_test_module_test {
    use sui::event;

    public struct Counter has copy, drop {
        value: u64,
    }

    public struct Bumped has copy, drop {
        amount: u64,
    }

    fun add(a: u64, b: u64): u64 { a + b }

    fun make_counter(v: u64): Counter { Counter { value: v } }

    #[test]
    fun test_arithmetic() {
        let s = add(2, 3);
        assert!(s == 5, 0);
    }

    #[test]
    fun test_resource_lifecycle() {
        let c = make_counter(7);
        let Counter { value } = c;
        assert!(value == 7, 0);
    }

    #[test]
    fun test_event_emit() {
        event::emit(Bumped { amount: 9 });
    }
}
