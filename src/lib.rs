//! jj-plan library interface.
//!
//! This module re-exports the public API surface needed by integration tests
//! and the binary entry point (`main.rs`). The binary delegates to this
//! library for all module access; this file exists so that
//! `cargo test --test <name>` can `use jj_plan::...` to access platform
//! services, types, and error types.

pub mod auth;
pub mod commands;
pub mod dispatch;
#[macro_use]
mod debug;
pub mod error;
pub mod flush;
pub mod markdown;
pub mod merge;
pub mod platform;
pub mod template;
pub mod jj_binary;
pub mod plan_dir;
pub mod plan_file;
pub mod plan_registry;
pub mod pr_cache;
pub mod stack_builder;
pub mod stack_context;
pub mod stack_render;
pub mod submit;
pub mod sync;
pub mod types;
pub mod workspace;
pub mod wrap;