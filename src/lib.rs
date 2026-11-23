//! Universal Desktop Builder: detect, plan, build and package desktop apps.
//!
//! The crate is intentionally one library with a small binary front end. The
//! layers are separate: adapters produce plans, the scheduler executes them,
//! providers describe toolchains, and the executor knows nothing about
//! frameworks.

pub mod adapters;
pub mod artifacts;
pub mod builder;
pub mod cache;
pub mod capability;
pub mod cli;
pub mod config;
pub mod detect;
pub mod error;
pub mod executor;
pub mod host;
pub mod packaging;
pub mod pipeline;
pub mod planner;
pub mod project;
pub mod providers;
pub mod scheduler;
pub mod security;
pub mod signing;
pub mod storage;
pub mod target;
pub mod telemetry;
pub mod toolchain;
pub mod validation;

pub use error::{ErrorCode, Result, UbuilderError};
