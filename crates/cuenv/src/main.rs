//! cuenv CLI Application
//!
//! Production-grade CUE environment toolchain providing command-line interface
//! for CUE package evaluation, environment variable management, and task orchestration.

mod app;
mod async_dispatch;
mod hook_supervisor;
mod oci_activate;
mod process_hardening;
mod sync_dispatch;

fn main() {
    let hardening_error = process_hardening::restrict_process_inspection();
    app::run_main(hardening_error);
}
