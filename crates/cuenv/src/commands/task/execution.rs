//! Task selection and execution.

mod context;
mod environments;
mod help;
mod implementation;
mod listing;
mod picker;
mod selection;

pub(super) use implementation::execute_task_impl;
