//! Declarative, local-first pipeline engine.
//!
//! Layers are one-way: parser → validator → compiler → planner → runtime →
//! history. YAML never reaches the executor and expressions never reach a shell.

pub mod actions;
pub mod artifacts;
pub mod cache;
pub mod compiler;
pub mod diagnostics;
pub mod events;
pub mod expression;
pub mod graph;
pub mod history;
pub mod ir;
pub mod matrix;
pub mod parser;
pub mod planner;
pub mod runtime;
pub mod schema;
pub mod validator;

pub use compiler::compile;
pub use parser::ParsedWorkflow;
