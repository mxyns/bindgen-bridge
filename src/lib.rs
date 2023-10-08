/// The module handling the importation of C types into Rust
/// Generates the mappings and allows to share them to other crates by generating static code
pub mod import;

/// The module handling the exportation of Rust types to C
/// Uses the generated mappings to build a cbindgen.toml config file, from a template, with the
/// correct [export.rename] section
pub mod export;

/// Common Result Wrapper
pub type Result<T> = core::result::Result<T, Box<dyn std::error::Error>>;