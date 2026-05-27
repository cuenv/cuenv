//! Implementation of the `cuenv build` command.
//!
//! Evaluates the CUE configuration and discovers container image definitions.
//! Listing image definitions is implemented; executing image builds is not.

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

    Err(cuenv_core::Error::configuration(
        "cuenv build: image execution backends are not implemented yet; \
         omit image names and labels to list configured images",
    ))
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
}
