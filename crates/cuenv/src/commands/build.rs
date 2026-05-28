//! Implementation of the `cuenv build` command.
//!
//! Evaluates the CUE configuration and discovers container image definitions.
//! Image execution is implemented through the local Docker CLI.

use std::collections::HashMap;
use std::path::{Path, PathBuf};
use std::process::Command;

use cuenv_core::manifest::{ContainerImage, Project};
use cuenv_events::emit_stdout;

use super::{CommandExecutor, relative_path_from_root};

/// Options for the `cuenv build` command.
pub struct BuildOptions {
    /// Path to directory containing CUE files.
    pub path: String,
    /// CUE package name to evaluate.
    pub package: String,
    /// Image names to build (empty = list all).
    pub names: Vec<String>,
    /// Label filters to select images by labels.
    pub labels: Vec<String>,
}

/// Execute the `cuenv build` command.
///
/// Evaluates CUE configuration, discovers image definitions, and either
/// lists available images or builds selected images with Docker.
///
/// # Errors
///
/// Returns an error if CUE evaluation or deserialization fails.
pub fn execute_build(options: &BuildOptions, executor: &CommandExecutor) -> cuenv_core::Result<()> {
    let target_path =
        Path::new(&options.path)
            .canonicalize()
            .map_err(|e| cuenv_core::Error::Io {
                source: e,
                path: Some(Path::new(&options.path).to_path_buf().into_boxed_path()),
                operation: "canonicalize path".to_string(),
            })?;

    let module = executor.get_module(&target_path)?;
    let relative_path = relative_path_from_root(&module.root, &target_path);

    let instance = module.get(&relative_path).ok_or_else(|| {
        cuenv_core::Error::configuration(format!(
            "No CUE instance found at path: {} (relative: {})",
            target_path.display(),
            relative_path.display()
        ))
    })?;

    let project: Project = instance.deserialize()?;

    if project.images.is_empty() {
        emit_stdout!("cuenv build: no images defined in configuration");
        return Ok(());
    }

    let filters = ImageFilters {
        names: &options.names,
        labels: &options.labels,
    };

    if filters.is_empty() {
        emit_available_images(&project.images);
        return Ok(());
    }

    let selected = matching_images(&project.images, &filters);

    if selected.is_empty() {
        return Err(cuenv_core::Error::configuration(
            "cuenv build: no images match the specified filters",
        ));
    }

    emit_build_plan(&selected);
    build_images(&target_path, &selected)
}

struct ImageFilters<'a> {
    names: &'a [String],
    labels: &'a [String],
}

impl ImageFilters<'_> {
    fn is_empty(&self) -> bool {
        self.names.is_empty() && self.labels.is_empty()
    }

    fn matches(&self, name: &str, image: &ContainerImage) -> bool {
        let name_match = self.names.is_empty() || self.names.iter().any(|item| item == name);
        let label_match =
            self.labels.is_empty() || self.labels.iter().any(|label| image.labels.contains(label));
        name_match && label_match
    }
}

fn emit_available_images(images: &HashMap<String, ContainerImage>) {
    emit_stdout!("Available images:\n");
    for (name, image) in sorted_images(images) {
        let desc = image
            .description
            .as_deref()
            .map_or(String::new(), |d| format!("  {d}"));
        let tags = if image.tags.is_empty() {
            String::new()
        } else {
            format!(" [{}]", image.tags.join(", "))
        };
        emit_stdout!(format!("  {name}{tags}{desc}"));
    }
}

fn emit_build_plan(images: &[(&str, &ContainerImage)]) {
    for (name, image) in images {
        let registry = image
            .registry
            .as_deref()
            .map_or(String::from("local"), |r| r.to_string());
        let platforms = if image.platform.is_empty() {
            String::from("native")
        } else {
            image.platform.join(", ")
        };
        emit_stdout!(format!(
            "cuenv build: {name} (context: {}, dockerfile: {}, registry: {registry}, platform: {platforms})",
            image.context, image.dockerfile
        ));
    }
}

fn matching_images<'a>(
    all_images: &'a HashMap<String, ContainerImage>,
    filters: &ImageFilters<'_>,
) -> Vec<(&'a str, &'a ContainerImage)> {
    sorted_images(all_images)
        .into_iter()
        .filter(|(name, image)| filters.matches(name, image))
        .collect()
}

fn sorted_images(images: &HashMap<String, ContainerImage>) -> Vec<(&str, &ContainerImage)> {
    let mut sorted: Vec<_> = images
        .iter()
        .map(|(name, image)| (name.as_str(), image))
        .collect();
    sorted.sort_by_key(|(name, _)| *name);
    sorted
}

fn build_images(project_dir: &Path, images: &[(&str, &ContainerImage)]) -> cuenv_core::Result<()> {
    images
        .iter()
        .try_for_each(|(name, image)| build_one_image(project_dir, name, image))
}

fn build_one_image(
    project_dir: &Path,
    name: &str,
    image: &ContainerImage,
) -> cuenv_core::Result<()> {
    if image.installable.is_some() && !image.context.is_empty() {
        return Err(cuenv_core::Error::configuration(format!(
            "cuenv build: image '{name}' sets both 'context' and 'installable'; choose one"
        )));
    }

    if image.installable.is_none() && image.context.is_empty() {
        return Err(cuenv_core::Error::configuration(format!(
            "cuenv build: image '{name}' must set either 'context' (Dockerfile) or 'installable' (Nix)"
        )));
    }

    if let Some(installable) = &image.installable {
        let invocation = NixBuildInvocation::new(name, image, installable, project_dir)?;
        invocation.run()
    } else {
        let invocation = DockerBuildInvocation::new(project_dir, name, image)?;
        emit_stdout!(format!("cuenv build: running {}", invocation.display()));
        invocation.run()
    }
}

struct DockerBuildInvocation {
    program: &'static str,
    args: Vec<String>,
    current_dir: PathBuf,
}

impl DockerBuildInvocation {
    fn new(project_dir: &Path, name: &str, image: &ContainerImage) -> cuenv_core::Result<Self> {
        if image.registry.is_none() && image.platform.len() > 1 {
            return Err(cuenv_core::Error::configuration(format!(
                "cuenv build: image '{name}' targets multiple platforms but has no registry; \
                 set registry to push a multi-platform image"
            )));
        }

        if image.registry.is_some() && image.tags.is_empty() {
            return Err(cuenv_core::Error::configuration(format!(
                "cuenv build: image '{name}' has a registry but no tags; \
                 add at least one tag to push to the registry"
            )));
        }

        let push = image.registry.is_some();
        let mut args = if push {
            vec![
                "buildx".to_string(),
                "build".to_string(),
                "--push".to_string(),
            ]
        } else {
            vec!["build".to_string()]
        };

        if !image.platform.is_empty() {
            args.extend(["--platform".to_string(), image.platform.join(",")]);
        }

        args.extend([
            "-f".to_string(),
            path_argument(Path::new(&image.context).join(&image.dockerfile)),
        ]);

        if let Some(target) = &image.target {
            args.extend(["--target".to_string(), target.clone()]);
        }

        let mut build_args: Vec<_> = image.build_args.iter().collect();
        build_args.sort_by_key(|(key, _)| *key);
        for (key, value) in build_args {
            let value = value.as_str().ok_or_else(|| {
                cuenv_core::Error::configuration(format!(
                    "cuenv build: image '{name}' build arg '{key}' uses an unresolved image output reference"
                ))
            })?;
            args.extend(["--build-arg".to_string(), format!("{key}={value}")]);
        }

        args.extend(
            image_refs(name, image)
                .into_iter()
                .flat_map(|tag| ["-t".to_string(), tag]),
        );
        args.push(path_argument(Path::new(&image.context)));

        Ok(Self {
            program: "docker",
            args,
            current_dir: project_dir.to_path_buf(),
        })
    }

    fn run(&self) -> cuenv_core::Result<()> {
        let status = Command::new(self.program)
            .args(&self.args)
            .current_dir(&self.current_dir)
            .status()
            .map_err(|source| cuenv_core::Error::Io {
                source,
                path: None,
                operation: "run docker build".to_string(),
            })?;

        if status.success() {
            Ok(())
        } else {
            Err(cuenv_core::Error::execution(format!(
                "docker build failed with status {status}"
            )))
        }
    }

    fn display(&self) -> String {
        std::iter::once(self.program.to_string())
            .chain(self.args.iter().cloned())
            .collect::<Vec<_>>()
            .join(" ")
    }
}

fn image_refs(name: &str, image: &ContainerImage) -> Vec<String> {
    let repository = image.repository.as_deref().unwrap_or(name);
    let base = image.registry.as_deref().map_or_else(
        || repository.to_string(),
        |registry| format!("{}/{}", registry.trim_end_matches('/'), repository),
    );

    image
        .tags
        .iter()
        .map(|tag| format!("{base}:{tag}"))
        .collect()
}

fn path_argument(path: impl AsRef<Path>) -> String {
    path.as_ref().to_string_lossy().into_owned()
}

/// Builds a Nix-native image: `nix build` the installable, `docker load` the
/// resulting archive, then tag (and optionally push) the configured references.
struct NixBuildInvocation {
    installable: String,
    refs: Vec<String>,
    push: bool,
    current_dir: PathBuf,
}

impl NixBuildInvocation {
    fn new(
        name: &str,
        image: &ContainerImage,
        installable: &str,
        project_dir: &Path,
    ) -> cuenv_core::Result<Self> {
        let push = image.registry.is_some();
        if push && image.tags.is_empty() {
            return Err(cuenv_core::Error::configuration(format!(
                "cuenv build: image '{name}' has a registry but no tags; \
                 add at least one tag to push to the registry"
            )));
        }

        Ok(Self {
            installable: installable.to_string(),
            refs: image_refs(name, image),
            push,
            current_dir: project_dir.to_path_buf(),
        })
    }

    fn run(&self) -> cuenv_core::Result<()> {
        emit_stdout!(format!(
            "cuenv build: running nix build {}",
            self.installable
        ));
        let build_output =
            run_capture(&self.current_dir, "nix", &nix_build_args(&self.installable))?;
        let archive = build_output.lines().next().unwrap_or("").trim();
        if archive.is_empty() {
            return Err(cuenv_core::Error::execution(format!(
                "cuenv build: nix build produced no output path for '{}'",
                self.installable
            )));
        }

        emit_stdout!(format!("cuenv build: running docker load -i {archive}"));
        let load_output = run_capture(&self.current_dir, "docker", &docker_load_args(archive))?;
        let loaded = parse_loaded_image(&load_output).ok_or_else(|| {
            cuenv_core::Error::execution(format!(
                "cuenv build: could not determine loaded image from docker load output:\n{load_output}"
            ))
        })?;

        for reference in &self.refs {
            emit_stdout!(format!("cuenv build: tagging {loaded} as {reference}"));
            run_status(
                &self.current_dir,
                "docker",
                &docker_tag_args(&loaded, reference),
            )?;
            if self.push {
                emit_stdout!(format!("cuenv build: running docker push {reference}"));
                run_status(&self.current_dir, "docker", &docker_push_args(reference))?;
            }
        }

        Ok(())
    }
}

fn nix_build_args(installable: &str) -> Vec<String> {
    vec![
        "build".to_string(),
        installable.to_string(),
        "--no-link".to_string(),
        "--print-out-paths".to_string(),
    ]
}

fn docker_load_args(archive: &str) -> Vec<String> {
    vec!["load".to_string(), "-i".to_string(), archive.to_string()]
}

fn docker_tag_args(source: &str, dest: &str) -> Vec<String> {
    vec!["tag".to_string(), source.to_string(), dest.to_string()]
}

fn docker_push_args(reference: &str) -> Vec<String> {
    vec!["push".to_string(), reference.to_string()]
}

/// Parses the local image reference from `docker load` output, which prints
/// either `Loaded image: repo:tag` or `Loaded image ID: sha256:...`.
fn parse_loaded_image(stdout: &str) -> Option<String> {
    stdout.lines().rev().find_map(|line| {
        line.trim()
            .strip_prefix("Loaded image ID: ")
            .or_else(|| line.trim().strip_prefix("Loaded image: "))
            .map(|reference| reference.trim().to_string())
    })
}

fn run_capture(dir: &Path, program: &str, args: &[String]) -> cuenv_core::Result<String> {
    let output = Command::new(program)
        .args(args)
        .current_dir(dir)
        .output()
        .map_err(|source| cuenv_core::Error::Io {
            source,
            path: None,
            operation: format!("run {program}"),
        })?;

    if output.status.success() {
        Ok(String::from_utf8_lossy(&output.stdout).into_owned())
    } else {
        Err(cuenv_core::Error::execution(format!(
            "{program} {} failed with status {}",
            args.join(" "),
            output.status
        )))
    }
}

fn run_status(dir: &Path, program: &str, args: &[String]) -> cuenv_core::Result<()> {
    let status = Command::new(program)
        .args(args)
        .current_dir(dir)
        .status()
        .map_err(|source| cuenv_core::Error::Io {
            source,
            path: None,
            operation: format!("run {program}"),
        })?;

    if status.success() {
        Ok(())
    } else {
        Err(cuenv_core::Error::execution(format!(
            "{program} {} failed with status {status}",
            args.join(" ")
        )))
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use cuenv_core::manifest::ImageOutputRef;

    fn test_image(tags: Vec<&str>, labels: Vec<&str>) -> ContainerImage {
        ContainerImage {
            image_type: "image".to_string(),
            ref_output: ImageOutputRef {
                cuenv_output_ref: true,
                cuenv_image: "test".to_string(),
                cuenv_output: "ref".to_string(),
            },
            digest: ImageOutputRef {
                cuenv_output_ref: true,
                cuenv_image: "test".to_string(),
                cuenv_output: "digest".to_string(),
            },
            context: ".".to_string(),
            dockerfile: "Dockerfile".to_string(),
            installable: None,
            build_args: HashMap::new(),
            target: None,
            tags: tags.into_iter().map(String::from).collect(),
            registry: None,
            repository: None,
            platform: vec![],
            depends_on: vec![],
            labels: labels.into_iter().map(String::from).collect(),
            inputs: vec![],
            description: None,
        }
    }

    #[test]
    fn test_matching_images_no_filters() {
        let mut images = HashMap::new();
        images.insert("api".to_string(), test_image(vec!["latest"], vec![]));
        images.insert("worker".to_string(), test_image(vec!["latest"], vec![]));

        let filters = ImageFilters {
            names: &[],
            labels: &[],
        };
        let result = matching_images(&images, &filters);
        let names: Vec<_> = result.iter().map(|(name, _)| *name).collect();
        assert_eq!(names, vec!["api", "worker"]);
    }

    #[test]
    fn test_matching_images_by_name() {
        let mut images = HashMap::new();
        images.insert("api".to_string(), test_image(vec!["latest"], vec![]));
        images.insert("worker".to_string(), test_image(vec!["latest"], vec![]));

        let names = vec!["api".to_string()];
        let filters = ImageFilters {
            names: &names,
            labels: &[],
        };
        let result = matching_images(&images, &filters);
        assert_eq!(result.len(), 1);
        assert_eq!(result[0].0, "api");
    }

    #[test]
    fn test_matching_images_by_label() {
        let mut images = HashMap::new();
        images.insert("api".to_string(), test_image(vec!["latest"], vec!["ci"]));
        images.insert("worker".to_string(), test_image(vec!["latest"], vec![]));

        let labels = vec!["ci".to_string()];
        let filters = ImageFilters {
            names: &[],
            labels: &labels,
        };
        let result = matching_images(&images, &filters);
        assert_eq!(result.len(), 1);
        assert_eq!(result[0].0, "api");
    }

    #[test]
    fn test_matching_images_requires_name_and_label_when_both_are_set() {
        let mut images = HashMap::new();
        images.insert("api".to_string(), test_image(vec!["latest"], vec!["ci"]));
        images.insert("worker".to_string(), test_image(vec!["latest"], vec!["ci"]));

        let names = vec!["api".to_string()];
        let labels = vec!["ci".to_string()];
        let filters = ImageFilters {
            names: &names,
            labels: &labels,
        };
        let result = matching_images(&images, &filters);
        assert_eq!(result.len(), 1);
        assert_eq!(result[0].0, "api");
    }

    #[test]
    fn test_docker_invocation_for_local_image() {
        let image = test_image(vec!["latest"], vec![]);
        let result = DockerBuildInvocation::new(Path::new("/workspace/app"), "api", &image);
        assert!(result.is_ok());
        let invocation = result.unwrap_or_else(|_| unreachable!("result was checked above"));

        assert_eq!(invocation.program, "docker");
        assert_eq!(invocation.current_dir, PathBuf::from("/workspace/app"));
        assert_eq!(
            invocation.args,
            vec!["build", "-f", "./Dockerfile", "-t", "api:latest", "."]
        );
    }

    #[test]
    fn test_docker_invocation_pushes_registry_image() {
        let mut image = test_image(vec!["v1"], vec![]);
        image.context = "docker/api".to_string();
        image.dockerfile = "Containerfile".to_string();
        image.registry = Some("ghcr.io/acme".to_string());
        image.repository = Some("services/api".to_string());
        image.platform = vec!["linux/amd64".to_string(), "linux/arm64".to_string()];

        let result = DockerBuildInvocation::new(Path::new("/workspace/app"), "api", &image);
        assert!(result.is_ok());
        let invocation = result.unwrap_or_else(|_| unreachable!("result was checked above"));

        assert_eq!(
            invocation.args,
            vec![
                "buildx",
                "build",
                "--push",
                "--platform",
                "linux/amd64,linux/arm64",
                "-f",
                "docker/api/Containerfile",
                "-t",
                "ghcr.io/acme/services/api:v1",
                "docker/api"
            ]
        );
    }

    #[test]
    fn test_multi_platform_local_image_requires_registry() {
        let mut image = test_image(vec!["latest"], vec![]);
        image.platform = vec!["linux/amd64".to_string(), "linux/arm64".to_string()];

        let result = DockerBuildInvocation::new(Path::new("/workspace/app"), "api", &image);
        assert!(result.is_err());
        let message = result
            .err()
            .map(|error| error.to_string())
            .unwrap_or_default();

        assert!(message.contains("multiple platforms"));
    }

    #[test]
    fn test_registry_image_requires_tags() {
        let mut image = test_image(vec![], vec![]);
        image.registry = Some("ghcr.io/acme".to_string());

        let result = DockerBuildInvocation::new(Path::new("/workspace/app"), "api", &image);
        assert!(result.is_err());
        let message = result
            .err()
            .map(|error| error.to_string())
            .unwrap_or_default();

        assert!(message.contains("no tags"));
    }

    #[test]
    fn test_nix_build_args() {
        assert_eq!(
            nix_build_args(".#images.api"),
            vec!["build", ".#images.api", "--no-link", "--print-out-paths"]
        );
    }

    #[test]
    fn test_docker_delivery_args() {
        assert_eq!(
            docker_load_args("/nix/store/x.tar.gz"),
            vec!["load", "-i", "/nix/store/x.tar.gz"]
        );
        assert_eq!(
            docker_tag_args("src:latest", "ghcr.io/acme/api:v1"),
            vec!["tag", "src:latest", "ghcr.io/acme/api:v1"]
        );
        assert_eq!(
            docker_push_args("ghcr.io/acme/api:v1"),
            vec!["push", "ghcr.io/acme/api:v1"]
        );
    }

    #[test]
    fn test_parse_loaded_image_named() {
        let out = "Some preamble\nLoaded image: api:latest\n";
        assert_eq!(parse_loaded_image(out).as_deref(), Some("api:latest"));
    }

    #[test]
    fn test_parse_loaded_image_id() {
        let out = "Loaded image ID: sha256:deadbeef\n";
        assert_eq!(parse_loaded_image(out).as_deref(), Some("sha256:deadbeef"));
    }

    #[test]
    fn test_parse_loaded_image_none() {
        assert_eq!(parse_loaded_image("nothing useful here"), None);
    }

    #[test]
    fn test_nix_invocation_local_refs() {
        let mut image = test_image(vec!["latest"], vec![]);
        image.context = String::new();
        image.installable = Some(".#images.api".to_string());

        let invocation =
            NixBuildInvocation::new("api", &image, ".#images.api", Path::new("/workspace/app"))
                .unwrap_or_else(|_| unreachable!("local nix image is valid"));

        assert!(!invocation.push);
        assert_eq!(invocation.refs, vec!["api:latest"]);
        assert_eq!(invocation.current_dir, PathBuf::from("/workspace/app"));
    }

    #[test]
    fn test_nix_invocation_registry_refs() {
        let mut image = test_image(vec!["v1", "latest"], vec![]);
        image.context = String::new();
        image.installable = Some(".#images.api".to_string());
        image.registry = Some("ghcr.io/acme".to_string());
        image.repository = Some("services/api".to_string());

        let invocation =
            NixBuildInvocation::new("api", &image, ".#images.api", Path::new("/workspace/app"))
                .unwrap_or_else(|_| unreachable!("registry nix image is valid"));

        assert!(invocation.push);
        assert_eq!(
            invocation.refs,
            vec![
                "ghcr.io/acme/services/api:v1",
                "ghcr.io/acme/services/api:latest"
            ]
        );
    }

    #[test]
    fn test_nix_invocation_registry_requires_tags() {
        let mut image = test_image(vec![], vec![]);
        image.context = String::new();
        image.installable = Some(".#images.api".to_string());
        image.registry = Some("ghcr.io/acme".to_string());

        let result =
            NixBuildInvocation::new("api", &image, ".#images.api", Path::new("/workspace/app"));
        assert!(result.is_err());
    }

    #[test]
    fn test_build_one_image_rejects_both_sources() {
        let mut image = test_image(vec!["latest"], vec![]);
        image.installable = Some(".#images.api".to_string());
        // context is still "." from test_image -> both set

        let result = build_one_image(Path::new("/workspace/app"), "api", &image);
        assert!(result.is_err());
        let message = result
            .err()
            .map(|error| error.to_string())
            .unwrap_or_default();
        assert!(message.contains("both"));
    }

    #[test]
    fn test_build_one_image_rejects_neither_source() {
        let mut image = test_image(vec!["latest"], vec![]);
        image.context = String::new();
        image.installable = None;

        let result = build_one_image(Path::new("/workspace/app"), "api", &image);
        assert!(result.is_err());
        let message = result
            .err()
            .map(|error| error.to_string())
            .unwrap_or_default();
        assert!(message.contains("either"));
    }
}
