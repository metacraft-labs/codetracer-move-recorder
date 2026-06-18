/// Multi-statement-per-line Move fixture for the column-aware recorder
/// verification (mirrors the EVM `ColumnAware.sol` fixture).
///
/// `run` packs three `let`-bindings onto a single source line so each
/// statement starts at a distinct column.  Under column-aware navigation
/// the recorder must surface a step for each statement with strictly
/// distinct column values; without column awareness the three statements
/// collapse onto `(line, column=1)` and only the first surfaces.
module column_aware::column_aware {
    const E_BAD: u64 = 1;

    /// Force the three single-line let-bindings to survive constant
    /// folding + dead-code elimination by routing each through a runtime
    /// vector and asserting on its dynamic length so the compiler must
    /// keep them as observable instructions with distinct source ranges.
    fun blackbox(v: &mut vector<u64>, x: u64) {
        std::vector::push_back(v, x);
    }

    #[test]
    fun test_multi_statement_line() {
        let mut v = std::vector::empty<u64>();
        blackbox(&mut v, 100); blackbox(&mut v, 200); blackbox(&mut v, 300);
        let len = std::vector::length(&v);
        assert!(len == 3, E_BAD);
    }
}
