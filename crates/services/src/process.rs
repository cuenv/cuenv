//! Service process spawning, display, and shutdown mechanics.

use std::path::Path;
use std::time::Duration;

use cuenv_core::manifest::{Entrypoint, Service};
use tokio::process::{Child, Command};

use crate::duration::parse_duration;

/// Process helper for one service definition.
pub(crate) struct ServiceProcess<'a> {
    name: &'a str,
    service: &'a Service,
    project_root: &'a Path,
}

impl<'a> ServiceProcess<'a> {
    pub(crate) fn new(name: &'a str, service: &'a Service, project_root: &'a Path) -> Self {
        Self {
            name,
            service,
            project_root,
        }
    }

    pub(crate) async fn spawn(&self) -> crate::Result<Child> {
        let (program, args) = self.resolve_command()?;
        let working_dir = self
            .service
            .dir
            .as_ref()
            .map(|dir| self.project_root.join(dir))
            .unwrap_or_else(|| self.project_root.to_path_buf());
        let (program, args) = wrap_with_supervisor(program, args);

        let mut command = Command::new(&program);
        command
            .args(&args)
            .current_dir(&working_dir)
            .stdout(std::process::Stdio::piped())
            .stderr(std::process::Stdio::piped());

        configure_process_group(&mut command);
        self.apply_environment(&mut command).await?;
        Ok(command.spawn()?)
    }

    pub(crate) async fn stop(&self, child: &mut Child) -> Option<i32> {
        let shutdown_config = self.service.shutdown.as_ref();
        let signal = shutdown_config
            .and_then(|shutdown| shutdown.signal.as_deref())
            .unwrap_or("SIGTERM");
        let timeout = shutdown_config
            .and_then(|shutdown| shutdown.timeout.as_deref())
            .and_then(|duration| parse_duration(duration).ok())
            .unwrap_or(Duration::from_secs(10));

        if let Some(pid) = child.id() {
            signal_process_group(pid, signal);

            let wait_result = tokio::time::timeout(timeout, child.wait()).await;
            if wait_result.is_err() {
                force_kill_process_group(pid);
                let _ = child.wait().await;
            }
        } else {
            let _ = child.kill().await;
        }

        let exit_code = child
            .try_wait()
            .ok()
            .flatten()
            .and_then(|status| status.code());
        cuenv_events::emit_service_stopped!(self.name, exit_code);
        exit_code
    }

    pub(crate) fn command_display(&self) -> String {
        match &self.service.entrypoint {
            Entrypoint::Task(task) => {
                if let Some(ref script) = task.script {
                    format!("script: {}", &script[..script.len().min(60)])
                } else if task.args.is_empty() {
                    task.command.clone()
                } else {
                    format!("{} {}", task.command, task.args.join(" "))
                }
            }
            Entrypoint::Script(script) => {
                format!("script: {}", &script.script[..script.script.len().min(60)])
            }
            Entrypoint::Command(command) => {
                let args: Vec<String> = command
                    .args
                    .iter()
                    .map(|arg| {
                        arg.as_str()
                            .map(str::to_string)
                            .unwrap_or_else(|| arg.to_string())
                    })
                    .collect();
                if args.is_empty() {
                    command.command.clone()
                } else {
                    format!("{} {}", command.command, args.join(" "))
                }
            }
        }
    }

    fn resolve_command(&self) -> crate::Result<(String, Vec<String>)> {
        match &self.service.entrypoint {
            Entrypoint::Task(task) => {
                if let Some(ref script) = task.script {
                    let (command, flag) = task
                        .script_shell
                        .as_ref()
                        .map_or(("bash", "-c"), |shell| shell.command_and_flag());
                    Ok((command.to_string(), vec![flag.to_string(), script.clone()]))
                } else {
                    Ok((task.command.clone(), task.args.to_vec()))
                }
            }
            Entrypoint::Script(script) => {
                let (command, flag) = script
                    .script_shell
                    .as_ref()
                    .map_or(("bash", "-c"), |shell| shell.command_and_flag());
                Ok((
                    command.to_string(),
                    vec![flag.to_string(), script.script.clone()],
                ))
            }
            Entrypoint::Command(command) => {
                let mut args = Vec::with_capacity(command.args.len());
                for (idx, arg) in command.args.iter().enumerate() {
                    match arg.as_str() {
                        Some(value) => args.push(value.to_string()),
                        None => {
                            return Err(crate::Error::service_failed(
                                self.name,
                                format!(
                                    "entrypoint.args[{idx}] is not a string ({}); \
                                     task-output references must be resolved before launch",
                                    arg
                                ),
                            ));
                        }
                    }
                }
                Ok((command.command.clone(), args))
            }
        }
    }

    async fn apply_environment(&self, command: &mut Command) -> crate::Result<()> {
        let (resolved_env, secrets) =
            cuenv_core::environment::Environment::resolve_for_service_with_secrets(
                self.name,
                &self.service.env,
            )
            .await?;
        if !secrets.is_empty() {
            cuenv_events::register_secrets(secrets.into_iter());
        }
        for (key, value) in resolved_env {
            command.env(key, value);
        }
        Ok(())
    }
}

fn wrap_with_supervisor(program: String, args: Vec<String>) -> (String, Vec<String>) {
    #[cfg(target_os = "macos")]
    {
        if let Ok(exe) = std::env::current_exe() {
            let mut new_args = Vec::with_capacity(args.len() + 2);
            new_args.push("__supervise".to_string());
            new_args.push(program);
            new_args.extend(args);
            return (exe.to_string_lossy().into_owned(), new_args);
        }
    }
    (program, args)
}

fn configure_process_group(command: &mut Command) {
    #[cfg(unix)]
    {
        #[expect(
            unsafe_code,
            reason = "pre_exec runs in the forked child; setpgid/prctl are async-signal-safe and affect only the child"
        )]
        unsafe {
            command.pre_exec(|| {
                if libc::setpgid(0, 0) != 0 {
                    return Err(std::io::Error::last_os_error());
                }
                #[cfg(target_os = "linux")]
                {
                    if libc::prctl(libc::PR_SET_PDEATHSIG, libc::SIGKILL) != 0 {
                        return Err(std::io::Error::last_os_error());
                    }
                }
                Ok(())
            });
        }
    }
}

fn signal_process_group(pid: u32, signal: &str) {
    #[cfg(unix)]
    {
        let signal = match signal {
            "SIGINT" => libc::SIGINT,
            "SIGHUP" => libc::SIGHUP,
            "SIGQUIT" => libc::SIGQUIT,
            _ => libc::SIGTERM,
        };
        unsafe {
            libc::kill(-(pid as i32), signal);
        }
    }

    #[cfg(not(unix))]
    {
        let _ = (pid, signal);
    }
}

fn force_kill_process_group(pid: u32) {
    #[cfg(unix)]
    unsafe {
        libc::kill(-(pid as i32), libc::SIGKILL);
    }

    #[cfg(not(unix))]
    {
        let _ = pid;
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use cuenv_core::manifest::{Command as ManifestCommand, Script};
    use serde_json::json;

    #[test]
    fn command_display_renders_non_string_args() {
        let service = Service {
            entrypoint: Entrypoint::Command(ManifestCommand {
                command: "echo".to_string(),
                args: vec![json!("ready"), json!({"cuenvOutputRef": true})],
            }),
            ..Service::default()
        };

        let process = ServiceProcess::new("db", &service, Path::new("."));
        assert_eq!(
            process.command_display(),
            r#"echo ready {"cuenvOutputRef":true}"#
        );
    }

    #[test]
    fn command_args_reject_unresolved_output_refs() {
        let service = Service {
            entrypoint: Entrypoint::Command(ManifestCommand {
                command: "echo".to_string(),
                args: vec![json!({"cuenvOutputRef": true})],
            }),
            ..Service::default()
        };

        let process = ServiceProcess::new("db", &service, Path::new("."));
        let error = process.resolve_command().unwrap_err().to_string();
        assert!(error.contains("entrypoint.args[0] is not a string"));
    }

    #[test]
    fn script_entrypoint_uses_shell_flag() {
        let service = Service {
            entrypoint: Entrypoint::Script(Script {
                script: "echo ready".to_string(),
                ..Script::default()
            }),
            ..Service::default()
        };

        let process = ServiceProcess::new("db", &service, Path::new("."));
        let (command, args) = process.resolve_command().unwrap();
        assert_eq!(command, "bash");
        assert_eq!(args, vec!["-c", "echo ready"]);
    }
}
