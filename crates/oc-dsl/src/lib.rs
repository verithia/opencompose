//! # OpenCompose DSL
//!
//! A universal music description language for AI generation, human editing,
//! and multi-engine audio rendering.
//!
//! ## Architecture
//!
//! ```text
//! Text (DSL source) → Parser → AST → Validator → Valid AST
//!                                                      ↓
//!                                               Serializer → Text
//!                                                      ↓
//!                                               Interpreter → Engine commands
//! ```
//!
//! ## Quick Start
//!
//! ```
//! let source = r#"@project "Test"
//!   tempo: 120
//!   time_signature: 4/4
//!   key: C major
//!
//! @track piano | instrument: piano/acoustic_grand
//!   @phrase intro [1:1:0 - 2:4:0]
//!     C4 q mf
//!     E4 q mf articulation=legato
//!     G4 h f
//! "#;
//!
//! let project = oc_dsl::parse(source).unwrap();
//! assert_eq!(project.properties.tempo, 120.0);
//! assert_eq!(project.tracks.len(), 1);
//! assert_eq!(project.tracks[0].phrases[0].events.len(), 3);
//! ```

pub mod ai;
pub mod arrangement;
pub mod ast;
pub mod automation;
pub mod automation_normalization;
pub mod clips;
pub mod diagnostics;
pub mod diff;
pub mod dynamics;
pub mod source_map;
pub use parser::parse_document;
mod effect;
pub mod error;
pub mod formula;
pub mod gliss;
pub mod gm;
pub mod groups;
pub mod instruments;
pub mod media;
pub mod ops;
pub mod ornament;
pub mod parser;
pub mod performance;
pub mod project_document;
pub mod serializer;
pub mod timecode;
pub mod timing;
pub mod validator;

/// Builtin plugin id for the drum-kit instrument. `@kit` blocks lower to a
/// preset with this plugin so every render path resolves them uniformly.
pub const DRUMKIT_PLUGIN_ID: &str = "builtin.drumkit";

/// Version written to newly created projects and reported by application diagnostics.
/// The parser preserves version headers already present in older documents.
pub const DSL_VERSION: &str = "0.6.0";

/// Parse an OpenCompose DSL source string into a Project AST.
///
/// Parsing is pure: a track that names only an instrument family is normalized
/// when the project is materialized for rendering or editing, not here, so
/// `fmt`/`export` keep the document the user wrote.
pub fn parse(source: &str) -> Result<ast::Project, error::ParseError> {
    parser::parse(source)
}

/// Serialize a Project AST back to OpenCompose DSL text.
pub fn serialize(project: &ast::Project) -> String {
    serializer::serialize(project)
}

pub mod preset;

/// Validate a Project AST for semantic correctness.
pub fn validate(project: &ast::Project) -> Result<(), Vec<error::ValidationError>> {
    validator::validate(project)
}

#[cfg(test)]
mod pitch_tests;
