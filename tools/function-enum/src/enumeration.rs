//! Function enumeration tool for cuenv workspace
//!
//! Uses `syn` to parse Rust source files and enumerate all functions.

use clap::{Parser, ValueEnum};
use serde::Serialize;
use std::collections::{BTreeMap, HashSet};
use std::fs;
use std::path::{Path, PathBuf};
use syn::Visibility;
use syn::visit::Visit;
use walkdir::WalkDir;

use super::style_check;

#[derive(Parser)]
#[command(name = "cuenv-function-enum")]
#[command(about = "Enumerate all functions in cuenv workspace crates")]
struct Cli {
    /// Output format
    #[arg(short, long, default_value = "json")]
    format: OutputFormat,

    /// Path to workspace root (defaults to current directory)
    #[arg(short, long)]
    path: Option<PathBuf>,

    /// Only enumerate public functions
    #[arg(long, default_value = "true")]
    public_only: bool,

    /// Include methods on impl blocks
    #[arg(long, default_value = "true")]
    include_methods: bool,

    /// Check Rust source style instead of enumerating functions.
    #[arg(long)]
    check_style: bool,
}

#[derive(Clone, ValueEnum)]
enum OutputFormat {
    Json,
    Markdown,
    Csv,
}

#[derive(Debug, Serialize, Clone, PartialEq, Eq)]
struct FunctionInfo {
    crate_name: String,
    file_path: String,
    name: String,
    visibility: String,
    is_async: bool,
    is_unsafe: bool,
    is_const: bool,
    #[serde(skip_serializing_if = "Option::is_none")]
    impl_for: Option<String>,
}

#[derive(Debug, Serialize)]
struct EnumerationResult {
    workspace_path: String,
    crates: Vec<String>,
    functions: Vec<FunctionInfo>,
    total_count: usize,
}

#[derive(Debug)]
struct WorkspaceCrate {
    name: String,
    path: PathBuf,
}

#[derive(Clone, Copy)]
struct EnumerationOptions {
    public_only: bool,
    include_methods: bool,
}

struct FunctionVisitor {
    crate_name: String,
    file_path: String,
    functions: Vec<FunctionInfo>,
    options: EnumerationOptions,
    current_impl: Option<String>,
}

impl FunctionVisitor {
    fn new(crate_name: String, file_path: String, options: EnumerationOptions) -> Self {
        Self {
            crate_name,
            file_path,
            functions: Vec::new(),
            options,
            current_impl: None,
        }
    }

    fn visibility_string(vis: &Visibility) -> &'static str {
        match vis {
            Visibility::Public(_) => "pub",
            Visibility::Restricted(_) => "pub(restricted)",
            Visibility::Inherited => "private",
        }
    }

    fn is_public(vis: &Visibility) -> bool {
        matches!(vis, Visibility::Public(_))
    }
}

impl<'ast> Visit<'ast> for FunctionVisitor {
    fn visit_item_fn(&mut self, node: &'ast syn::ItemFn) {
        if self.options.public_only && !Self::is_public(&node.vis) {
            return;
        }

        self.functions.push(FunctionInfo {
            crate_name: self.crate_name.clone(),
            file_path: self.file_path.clone(),
            name: node.sig.ident.to_string(),
            visibility: Self::visibility_string(&node.vis).to_string(),
            is_async: node.sig.asyncness.is_some(),
            is_unsafe: node.sig.unsafety.is_some(),
            is_const: node.sig.constness.is_some(),
            impl_for: None,
        });

        syn::visit::visit_item_fn(self, node);
    }

    fn visit_item_impl(&mut self, node: &'ast syn::ItemImpl) {
        if !self.options.include_methods {
            return;
        }

        // Get the type being implemented
        let impl_type = if let syn::Type::Path(type_path) = &*node.self_ty {
            type_path
                .path
                .segments
                .iter()
                .map(|s| s.ident.to_string())
                .collect::<Vec<_>>()
                .join("::")
        } else {
            "<complex type>".to_string()
        };

        self.current_impl = Some(impl_type);
        syn::visit::visit_item_impl(self, node);
        self.current_impl = None;
    }

    fn visit_impl_item_fn(&mut self, node: &'ast syn::ImplItemFn) {
        if self.options.public_only && !Self::is_public(&node.vis) {
            syn::visit::visit_impl_item_fn(self, node);
            return;
        }

        self.functions.push(FunctionInfo {
            crate_name: self.crate_name.clone(),
            file_path: self.file_path.clone(),
            name: node.sig.ident.to_string(),
            visibility: Self::visibility_string(&node.vis).to_string(),
            is_async: node.sig.asyncness.is_some(),
            is_unsafe: node.sig.unsafety.is_some(),
            is_const: node.sig.constness.is_some(),
            impl_for: self.current_impl.clone(),
        });

        syn::visit::visit_impl_item_fn(self, node);
    }
}

pub fn run() -> Result<(), Box<dyn std::error::Error>> {
    let cli = Cli::parse();

    let workspace_path = match cli.path {
        Some(path) => path,
        None => std::env::current_dir()?,
    };
    if cli.check_style {
        return style_check::run(&workspace_path);
    }

    // Discover workspace crates
    let crates = discover_workspace_crates(&workspace_path)?;
    eprintln!("Found {} crates in workspace", crates.len());

    // Parse each crate's source files
    let mut all_functions = Vec::new();
    let options = EnumerationOptions {
        public_only: cli.public_only,
        include_methods: cli.include_methods,
    };
    for workspace_crate in &crates {
        eprintln!("Parsing {}...", workspace_crate.name);
        let functions = parse_crate_sources(&workspace_crate.name, &workspace_crate.path, options)?;
        eprintln!("  Found {} functions", functions.len());
        all_functions.extend(functions);
    }

    let result = EnumerationResult {
        workspace_path: workspace_path.display().to_string(),
        crates: crates
            .into_iter()
            .map(|workspace_crate| workspace_crate.name)
            .collect(),
        total_count: all_functions.len(),
        functions: all_functions,
    };

    // Output result
    match cli.format {
        OutputFormat::Json => {
            println!("{}", serde_json::to_string_pretty(&result)?);
        }
        OutputFormat::Markdown => {
            output_markdown(&result);
        }
        OutputFormat::Csv => {
            output_csv(&result);
        }
    }

    Ok(())
}

fn discover_workspace_crates(
    workspace_path: &Path,
) -> Result<Vec<WorkspaceCrate>, Box<dyn std::error::Error>> {
    let cargo_toml_path = workspace_path.join("Cargo.toml");
    let content = fs::read_to_string(&cargo_toml_path)?;
    let value: toml::Value = toml::from_str(&content)?;

    let members = value
        .get("workspace")
        .and_then(|workspace| workspace.get("members"))
        .and_then(toml::Value::as_array)
        .ok_or_else(|| std::io::Error::other("workspace.members must be an array"))?;

    let mut crates = Vec::new();
    let mut names = HashSet::new();
    for member in members {
        let member_str = member
            .as_str()
            .ok_or_else(|| std::io::Error::other("workspace member must be a path string"))?;
        if member_str.contains(['*', '?', '[']) {
            return Err(std::io::Error::other(format!(
                "workspace member glob is unsupported: {member_str}"
            ))
            .into());
        }

        let path = workspace_path.join(member_str);
        let manifest = path.join("Cargo.toml");
        let content = fs::read_to_string(&manifest).map_err(|error| {
            std::io::Error::new(
                error.kind(),
                format!("cannot read {}: {error}", manifest.display()),
            )
        })?;
        let member: toml::Value = toml::from_str(&content)?;
        let name = member
            .get("package")
            .and_then(|package| package.get("name"))
            .and_then(toml::Value::as_str)
            .ok_or_else(|| {
                std::io::Error::other(format!("{} has no package name", manifest.display()))
            })?
            .to_string();
        if !names.insert(name.clone()) {
            return Err(std::io::Error::other(format!("duplicate package name: {name}")).into());
        }
        crates.push(WorkspaceCrate { name, path });
    }
    Ok(crates)
}

fn parse_crate_sources(
    crate_name: &str,
    crate_path: &Path,
    options: EnumerationOptions,
) -> Result<Vec<FunctionInfo>, Box<dyn std::error::Error>> {
    let mut all_functions = Vec::new();
    let src_path = crate_path.join("src");

    let metadata = fs::metadata(&src_path).map_err(|error| {
        std::io::Error::new(
            error.kind(),
            format!("cannot inspect {}: {error}", src_path.display()),
        )
    })?;
    if !metadata.is_dir() {
        return Err(std::io::Error::other(format!(
            "source path is not a directory: {}",
            src_path.display()
        ))
        .into());
    }

    for entry in WalkDir::new(&src_path).sort_by_file_name() {
        let entry = entry?;
        if !entry.path().extension().is_some_and(|ext| ext == "rs") {
            continue;
        }
        let file_path = entry.path();
        let relative_path = file_path
            .strip_prefix(crate_path)
            .unwrap_or(file_path)
            .display()
            .to_string();

        let content = fs::read_to_string(file_path).map_err(|error| {
            std::io::Error::new(
                error.kind(),
                format!("cannot read {}: {error}", file_path.display()),
            )
        })?;
        let syntax = syn::parse_file(&content).map_err(|error| {
            std::io::Error::other(format!("cannot parse {}: {error}", file_path.display()))
        })?;
        let mut visitor = FunctionVisitor::new(crate_name.to_string(), relative_path, options);
        visitor.visit_file(&syntax);
        all_functions.extend(visitor.functions);
    }

    Ok(all_functions)
}

fn output_markdown(result: &EnumerationResult) {
    println!("# Function Enumeration Report\n");
    println!("**Workspace:** {}\n", result.workspace_path);
    println!("**Total Functions:** {}\n", result.total_count);
    println!("**Crates:** {}\n", result.crates.join(", "));
    println!("---\n");

    // Group by crate
    let mut by_crate: BTreeMap<&str, Vec<&FunctionInfo>> = BTreeMap::new();
    for func in &result.functions {
        by_crate.entry(&func.crate_name).or_default().push(func);
    }

    for (crate_name, functions) in by_crate {
        println!("## {}\n", crate_name);
        println!("| Function | File | Visibility | Async | Unsafe | Impl For |");
        println!("|----------|------|------------|-------|--------|----------|");
        for func in functions {
            println!(
                "| `{}` | {} | {} | {} | {} | {} |",
                func.name,
                func.file_path,
                func.visibility,
                if func.is_async { "Yes" } else { "No" },
                if func.is_unsafe { "Yes" } else { "No" },
                func.impl_for.as_deref().unwrap_or("-"),
            );
        }
        println!();
    }
}

fn output_csv(result: &EnumerationResult) {
    println!("crate,file,name,visibility,async,unsafe,const,impl_for");
    for func in &result.functions {
        println!(
            "{},{},{},{},{},{},{},{}",
            func.crate_name,
            func.file_path,
            func.name,
            func.visibility,
            func.is_async,
            func.is_unsafe,
            func.is_const,
            func.impl_for.as_deref().unwrap_or("")
        );
    }
}

#[cfg(test)]
mod tests {
    use super::EnumerationOptions;
    use std::fs;
    use std::path::PathBuf;
    use std::time::{SystemTime, UNIX_EPOCH};

    fn temporary_directory() -> PathBuf {
        let suffix = SystemTime::now().duration_since(UNIX_EPOCH).unwrap().as_nanos();
        let directory = std::env::temp_dir().join(format!(
            "cuenv-enumeration-{}-{suffix}",
            std::process::id()
        ));
        fs::create_dir_all(&directory).unwrap();
        directory
    }

    #[test]
    fn enumeration_fails_for_missing_or_unparseable_source() {
        let directory = temporary_directory();
        let options = EnumerationOptions {
            public_only: false,
            include_methods: true,
        };
        let missing = super::parse_crate_sources("broken", &directory, options)
            .unwrap_err()
            .to_string();
        assert!(missing.contains("cannot inspect"), "{missing}");
        fs::create_dir(directory.join("src")).unwrap();
        let source = directory.join("src/broken.rs");
        fs::write(&source, "fn incomplete(").unwrap();
        let result = super::parse_crate_sources(
            "broken",
            &directory,
            options,
        );
        fs::remove_dir_all(&directory).unwrap();
        let error = result.unwrap_err().to_string();
        assert!(error.contains("cannot parse"), "{error}");
        assert!(error.contains("broken.rs"), "{error}");
    }

    #[test]
    fn workspace_inventory_uses_package_names_and_requires_each_manifest() {
        let directory = temporary_directory();
        fs::write(
            directory.join("Cargo.toml"),
            "[workspace]\nmembers = [\"crates/github\", \"crates/tools/github\"]\n",
        )
        .unwrap();
        for (path, name) in [
            ("crates/github", "cuenv-github"),
            ("crates/tools/github", "cuenv-tool-github"),
        ] {
            let member = directory.join(path);
            fs::create_dir_all(&member).unwrap();
            fs::write(member.join("Cargo.toml"), format!("[package]\nname = \"{name}\"\n"))
                .unwrap();
        }
        let crates = super::discover_workspace_crates(&directory).unwrap();
        let names: Vec<_> = crates.iter().map(|member| member.name.as_str()).collect();
        assert_eq!(names, ["cuenv-github", "cuenv-tool-github"]);

        fs::remove_file(directory.join("crates/tools/github/Cargo.toml")).unwrap();
        let error = super::discover_workspace_crates(&directory).unwrap_err().to_string();
        fs::remove_dir_all(&directory).unwrap();
        assert!(error.contains("cannot read"), "{error}");
        assert!(error.contains("Cargo.toml"), "{error}");
    }
}
