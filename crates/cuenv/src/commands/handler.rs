//! Command handler traits and implementations for lifecycle event management.
//!
//! This module provides the `CommandHandler` trait that standardizes command
//! execution with automatic event emission (start, complete), eliminating
//! boilerplate in individual command implementations.

mod implementation;
mod task_handler;

pub use implementation::{
    AllowHandler, CiHandler, CommandHandler, CommandRunner, DenyHandler, EnvCheckHandler,
    EnvInspectHandler, EnvListHandler, EnvLoadHandler, EnvPrintHandler, EnvStatusHandler,
    ExecHandler, ExportHandler, SyncHandler,
};
pub use task_handler::TaskHandler;
