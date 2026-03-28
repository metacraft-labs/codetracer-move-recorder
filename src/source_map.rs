//! Source location tracking for Move bytecode.
//!
//! Maps (module_name, pc) pairs to (file, line) source locations.
//! In M2 we use a manually-constructed table; later milestones can
//! parse `.mvsm` source-map files.

/// Resolves bytecode program-counter values to source file locations.
pub struct SourceMapResolver {
    /// Entries: (module_name, pc, file_path, line_number).
    entries: Vec<(String, u64, String, u32)>,
}

impl SourceMapResolver {
    /// Build a resolver from explicit entries.
    pub fn from_entries(entries: Vec<(String, u64, String, u32)>) -> Self {
        Self { entries }
    }

    /// Look up the source location for a given module and program counter.
    ///
    /// Returns `Some((file_path, line_number))` if a mapping exists.
    pub fn lookup(&self, module: &str, pc: u64) -> Option<(&str, u32)> {
        self.entries
            .iter()
            .find(|(m, p, _, _)| m == module && *p == pc)
            .map(|(_, _, file, line)| (file.as_str(), *line))
    }

    /// Create an empty resolver (no source mappings).
    pub fn empty() -> Self {
        Self {
            entries: Vec::new(),
        }
    }

    /// Return all known file paths referenced in the source map.
    pub fn all_files(&self) -> Vec<&str> {
        let mut files: Vec<&str> = self.entries.iter().map(|(_, _, f, _)| f.as_str()).collect();
        files.sort();
        files.dedup();
        files
    }
}

impl Default for SourceMapResolver {
    fn default() -> Self {
        Self::empty()
    }
}
