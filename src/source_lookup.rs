//! Source code discovery for on-chain Move modules.
//!
//! Scans configured directories to find `.move` source files by module name.
//! Used by the replay pipeline to locate source code for traced transactions.

use std::collections::HashMap;
use std::path::PathBuf;

use walkdir::WalkDir;

use crate::source_map::SourceMapResolver;

/// Discovers Move source files in a set of search directories.
pub struct SourceLookup {
    /// Map from module name (lowercase) to file path.
    index: HashMap<String, PathBuf>,
}

impl SourceLookup {
    /// Build a source lookup index by scanning the given directories.
    ///
    /// Searches for `.move` files in each directory recursively.
    /// Files in `sources/` subdirectories are preferred.
    pub fn new(search_dirs: Vec<PathBuf>) -> Self {
        let mut index: HashMap<String, PathBuf> = HashMap::new();

        for dir in &search_dirs {
            if !dir.exists() {
                continue;
            }

            for entry in WalkDir::new(dir)
                .follow_links(true)
                .into_iter()
                .filter_map(|e| e.ok())
            {
                let path = entry.path();
                if path.extension().is_some_and(|ext| ext == "move")
                    && let Some(stem) = path.file_stem()
                {
                    let module_name = stem.to_string_lossy().to_lowercase();
                    // Prefer files in `sources/` subdirectories over others.
                    let in_sources_dir = path
                        .parent()
                        .and_then(|p| p.file_name())
                        .is_some_and(|name| name == "sources");

                    if !index.contains_key(&module_name) || in_sources_dir {
                        index.insert(module_name, path.to_path_buf());
                    }
                }
            }
        }

        Self { index }
    }

    /// Resolve a module name to its `.move` source file path.
    ///
    /// Returns `None` if the module is not found (does not fail).
    pub fn resolve(&self, module_name: &str) -> Option<PathBuf> {
        self.index.get(&module_name.to_lowercase()).cloned()
    }

    /// Build a `SourceMapResolver` from discovered source files.
    ///
    /// For now, returns an empty source map since `.mvsm` parsing is not
    /// yet implemented. The source files are still useful for display purposes.
    pub fn build_source_map(&self) -> SourceMapResolver {
        // Future milestone: parse .mvsm files to build real source maps.
        SourceMapResolver::empty()
    }
}
