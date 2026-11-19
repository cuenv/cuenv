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
    #[cfg(target_os = "linux")]
    cuenv_task_exec::sandbox::dispatch_linux_launcher();

    let hardening_error = process_hardening::restrict_process_inspection();
    #[cfg(target_os = "linux")]
    cuenv_task_exec::sandbox::register_linux_launcher();
    app::run_main(hardening_error);
}
