/// Sui `sui::event::emit` fixture for the Move recorder.
///
/// Pins the recorder's typed `MoveEvent` io_event surface area: a call
/// to `sui::event::emit(MyEvent { ... })` must appear as a structured
/// `EventLogKind::TraceLogEvent` carrying the typed event payload in
/// the metadata + content fields, so downstream consumers can recover
/// (struct name, field map) without re-parsing the printed form.
module flow_test::event_emit_test {
    use sui::event;

    /// A simple event struct with `copy + drop` abilities so it can
    /// flow through `event::emit` without a `key` ability.
    public struct MyEvent has copy, drop {
        sender: address,
        amount: u64,
    }

    /// Build and emit a `MyEvent { sender: 0xCAFE, amount: 1000 }`.
    fun fire() {
        let ev = MyEvent { sender: @0xCAFE, amount: 1000 };
        event::emit(ev);
    }

    #[test]
    fun test_event_emit() {
        fire();
    }
}
