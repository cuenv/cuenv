//! Implementation of the `cuenv build` command.
//!
//! Evaluates the CUE configuration, discovers container image definitions,
//! and either lists available images or validates what would be built.
//! Actual execution backends (Dagger, Docker CLI) are future work.

use std::collections::HashMap;
use std::path::Path;

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
/// lists available images or validates the build configuration.
///
/// # Errors
///
/// Returns an error if CUE evaluation or deserialization fails.
pub fn execute_build(
    options: &BuildOptions,
    executor: &CommandExecutor,
) -> cuenv_core::Result<()> {
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

    // No image name specified → list all images
    if options.names.is_empty() && options.labels.is_empty() {
        emit_stdout!("Available images:\n");
        for (name, image) in &project.images {
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
        return Ok(());
    }

    let filtered = filter_images(&project.images, &options.names, &options.labels);

    if filtered.is_empty() {
        emit_stdout!("cuenv build: no images match the specified filters");
        return Ok(());
    }

    // Phase 1: validate and print what would be built.
    // Execution backends will be added in follow-up work.
    for (name, image) in &filtered {
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

    emit_stdout!("\ncuenv build: execution backends not yet implemented — schema validated successfully");
    Ok(())
}

/// Filter images by name and label.
fn filter_images(
    all_images: &HashMap<String, ContainerImage>,
    names: &[String],
    labels: &[String],
) -> HashMap<String, ContainerImage> {
    all_images
        .iter()
        .filter(|(name, image)| {
            let name_match = names.is_empty() || names.contains(name);
            let label_match =
                labels.is_empty() || labels.iter().any(|l| image.labels.contains(l));
            name_match && label_match
        })
        .map(|(name, image)| (name.clone(), image.clone()))
        .collect()
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
    fn test_filter_images_no_filters() {
        let mut images = HashMap::new();
        images.insert("api".to_string(), test_image(vec!["latest"], vec![]));
        images.insert("worker".to_string(), test_image(vec!["latest"], vec![]));

        let result = filter_images(&images, &[], &[]);
        assert_eq!(result.len(), 2);
    }

    #[test]
    fn test_filter_images_by_name() {
        let mut images = HashMap::new();
        images.insert("api".to_string(), test_image(vec!["latest"], vec![]));
        images.insert("worker".to_string(), test_image(vec!["latest"], vec![]));

        let result = filter_images(&images, &["api".to_string()], &[]);
        assert_eq!(result.len(), 1);
        assert!(result.contains_key("api"));
    }

    #[test]
    fn test_filter_images_by_label() {
        let mut images = HashMap::new();
        images.insert("api".to_string(), test_image(vec!["latest"], vec!["ci"]));
        images.insert("worker".to_string(), test_image(vec!["latest"], vec![]));

        let result = filter_images(&images, &[], &["ci".to_string()]);
        assert_eq!(result.len(), 1);
        assert!(result.contains_key("api"));
    }
}
