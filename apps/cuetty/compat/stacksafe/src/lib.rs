//! Adapter for the stacksafe 0.1 API used by GPUI 0.2.
//!
//! GPUI uses the default-feature API, which is provided by stacksafe 1.x.
//! The former `derive-visitor` feature is intentionally not implemented.

#![forbid(unsafe_code)]

pub use stacksafe_upstream::{
    StackSafe, get_minimum_stack_size, get_stack_allocation_size, internal, set_minimum_stack_size,
    set_stack_allocation_size, stacksafe,
};
