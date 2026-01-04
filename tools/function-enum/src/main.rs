//! Function enumeration tool for cuenv workspace
//!
//! Uses `syn` to parse Rust source files and enumerate all functions.

use clap::{Parser, ValueEnum};
use serde::Serialize;
use std::collections::HashMap;
use std::fs;
use std::path::{Path, PathBuf};
use syn::{visit::Visit, Visibility};
use walkdir::WalkDir;

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

struct FunctionVisitor {
    crate_name: String,
    file_path: String,
    functions: Vec<FunctionInfo>,
    public_only: bool,
    include_methods: bool,
    current_impl: Option<String>,
}

impl FunctionVisitor {
    fn new(crate_name: String, file_path: String, public_only: bool, include_methods: bool) -> Self {
        Self {
            crate_name,
            file_path,
            functions: Vec::new(),
            public_only,
            include_methods,
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
        if self.public_only && !Self::is_public(&node.vis) {
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
        if !self.include_methods {
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
        if self.public_only && !Self::is_public(&node.vis) {
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

fn main() -> Result<(), Box<dyn std::error::Error>> {
    let cli = Cli::parse();

    let workspace_path = cli.path.unwrap_or_else(|| std::env::current_dir().unwrap());

    // Discover workspace crates
    let crates = discover_workspace_crates(&workspace_path)?;
    eprintln!("Found {} crates in workspace", crates.len());

    // Parse each crate's source files
    let mut all_functions = Vec::new();
    for (crate_name, crate_path) in &crates {
        eprintln!("Parsing {}...", crate_name);
        let functions = parse_crate_sources(crate_name, crate_path, cli.public_only, cli.include_methods)?;
        eprintln!("  Found {} functions", functions.len());
        all_functions.extend(functions);
    }

    let result = EnumerationResult {
        workspace_path: workspace_path.display().to_string(),
        crates: crates.into_iter().map(|(name, _)| name).collect(),
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

fn discover_workspace_crates(workspace_path: &Path) -> Result<Vec<(String, PathBuf)>, Box<dyn std::error::Error>> {
    let cargo_toml_path = workspace_path.join("Cargo.toml");
    let content = fs::read_to_string(&cargo_toml_path)?;
    let value: toml::Value = toml::from_str(&content)?;

    let mut crates = Vec::new();

    if let Some(workspace) = value.get("workspace") {
        if let Some(members) = workspace.get("members") {
            if let Some(members_array) = members.as_array() {
                for member in members_array {
                    if let Some(member_str) = member.as_str() {
                        // Skip comments (lines starting with #)
                        if member_str.starts_with('#') {
                            continue;
                        }

                        let member_path = workspace_path.join(member_str);

                        // Extract crate name from path
                        let crate_name = Path::new(member_str)
                            .file_name()
                            .and_then(|n| n.to_str())
                            .unwrap_or(member_str)
                            .to_string();

                        if member_path.exists() {
                            crates.push((crate_name, member_path));
                        }
                    }
                }
            }
        }
    }

    Ok(crates)
}

fn parse_crate_sources(
    crate_name: &str,
    crate_path: &Path,
    public_only: bool,
    include_methods: bool,
) -> Result<Vec<FunctionInfo>, Box<dyn std::error::Error>> {
    let mut all_functions = Vec::new();
    let src_path = crate_path.join("src");

    if !src_path.exists() {
        return Ok(all_functions);
    }

    for entry in WalkDir::new(&src_path)
        .into_iter()
        .filter_map(|e| e.ok())
        .filter(|e| e.path().extension().is_some_and(|ext| ext == "rs"))
    {
        let file_path = entry.path();
        let relative_path = file_path
            .strip_prefix(crate_path)
            .unwrap_or(file_path)
            .display()
            .to_string();

        match fs::read_to_string(file_path) {
            Ok(content) => {
                match syn::parse_file(&content) {
                    Ok(syntax) => {
                        let mut visitor = FunctionVisitor::new(
                            crate_name.to_string(),
                            relative_path,
                            public_only,
                            include_methods,
                        );
                        visitor.visit_file(&syntax);
                        all_functions.extend(visitor.functions);
                    }
                    Err(e) => {
                        eprintln!("  Warning: Failed to parse {}: {}", file_path.display(), e);
                    }
                }
            }
            Err(e) => {
                eprintln!("  Warning: Failed to read {}: {}", file_path.display(), e);
            }
        }
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
    let mut by_crate: HashMap<&str, Vec<&FunctionInfo>> = HashMap::new();
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
