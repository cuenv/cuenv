//! Check Rust signature and loop conventions in first-party source.

use std::error::Error;
use std::path::Path;
use std::{fs, io};

use syn::visit::Visit;
use syn::{Expr, FnArg, Signature, Type};
use walkdir::{DirEntry, WalkDir};

#[derive(Default)]
struct StyleCheck {
    trait_implementation: bool,
    violations: Vec<String>,
}

impl StyleCheck {
    fn inspect(&mut self, signature: &Signature) {
        if is_foreign_abi(signature) {
            return;
        }
        let arguments: Vec<_> = signature
            .inputs
            .iter()
            .filter_map(|argument| match argument {
                FnArg::Receiver(_) => None,
                FnArg::Typed(argument) => Some(argument),
            })
            .collect();
        let location = signature.ident.span().start().line;
        if arguments.len() > 3 {
            self.violations.push(format!(
                "{location}: {} has {} non-receiver parameters",
                signature.ident,
                arguments.len()
            ));
        }
        if arguments.iter().any(|argument| is_boolean(&argument.ty)) {
            self.violations.push(format!(
                "{location}: {} has a boolean parameter",
                signature.ident
            ));
        }
    }
}

impl<'syntax> Visit<'syntax> for StyleCheck {
    fn visit_item_fn(&mut self, item: &'syntax syn::ItemFn) {
        self.inspect(&item.sig);
        syn::visit::visit_item_fn(self, item);
    }

    fn visit_item_impl(&mut self, item: &'syntax syn::ItemImpl) {
        let previous = self.trait_implementation;
        self.trait_implementation = item.trait_.is_some() && !is_boolean_conversion(item);
        syn::visit::visit_item_impl(self, item);
        self.trait_implementation = previous;
    }

    fn visit_impl_item_fn(&mut self, item: &'syntax syn::ImplItemFn) {
        if !self.trait_implementation {
            self.inspect(&item.sig);
        }
        syn::visit::visit_impl_item_fn(self, item);
    }

    fn visit_trait_item_fn(&mut self, item: &'syntax syn::TraitItemFn) {
        self.inspect(&item.sig);
        syn::visit::visit_trait_item_fn(self, item);
    }

    fn visit_expr_for_loop(&mut self, item: &'syntax syn::ExprForLoop) {
        use syn::spanned::Spanned;

        if iterator_adapters(&item.expr) >= 2 {
            self.violations.push(format!(
                "{}: bind the iterator chain before the for loop",
                item.span().start().line
            ));
        }
        syn::visit::visit_expr_for_loop(self, item);
    }
}

fn is_boolean_conversion(item: &syn::ItemImpl) -> bool {
    item.trait_.as_ref().is_some_and(|(_, path, _)| {
        path.segments.last().is_some_and(|segment| {
            segment.ident == "From"
                && matches!(
                    &segment.arguments,
                    syn::PathArguments::AngleBracketed(arguments)
                        if arguments.args.iter().any(|argument|
                            matches!(argument, syn::GenericArgument::Type(ty) if is_boolean(ty)))
                )
        })
    })
}

fn iterator_adapters(expression: &Expr) -> usize {
    match expression {
        Expr::MethodCall(call) => {
            let adapter = matches!(
                call.method.to_string().as_str(),
                "chain" | "cloned" | "copied" | "cycle" | "enumerate" | "filter"
                    | "filter_map" | "flat_map" | "flatten" | "fuse" | "inspect" | "map"
                    | "map_while" | "peekable" | "rev" | "scan" | "skip" | "skip_while"
                    | "step_by" | "take" | "take_while" | "zip"
            );
            usize::from(adapter) + iterator_adapters(&call.receiver)
        }
        Expr::Paren(group) => iterator_adapters(&group.expr),
        Expr::Group(group) => iterator_adapters(&group.expr),
        _ => 0,
    }
}

fn is_foreign_abi(signature: &Signature) -> bool {
    signature
        .abi
        .as_ref()
        .is_some_and(|abi| abi.name.as_ref().is_none_or(|name| name.value() != "Rust"))
}

fn is_boolean(ty: &Type) -> bool {
    match ty {
        Type::Reference(reference) => is_boolean(&reference.elem),
        Type::Paren(parenthesized) => is_boolean(&parenthesized.elem),
        Type::Group(group) => is_boolean(&group.elem),
        Type::Path(path) if path.qself.is_none() => {
            let segments: Vec<_> = path
                .path
                .segments
                .iter()
                .map(|segment| &segment.ident)
                .collect();
            matches!(segments.as_slice(), [name] if *name == "bool")
                || matches!(segments.as_slice(), [root, primitive, name]
                    if (*root == "std" || *root == "core") && *primitive == "primitive" && *name == "bool")
        }
        _ => false,
    }
}

fn is_source_entry(entry: &DirEntry) -> bool {
    let name = entry.file_name();
    name != "target" && name != ".git" && name != "node_modules" && name != "result"
}

fn check_source(path: &Path) -> Result<Vec<String>, Box<dyn Error>> {
    let content = fs::read_to_string(path).map_err(|error| {
        io::Error::new(
            error.kind(),
            format!("cannot read {}: {error}", path.display()),
        )
    })?;
    let syntax = syn::parse_file(&content)
        .map_err(|error| io::Error::other(format!("cannot parse {}: {error}", path.display())))?;
    let mut check = StyleCheck::default();
    check.visit_file(&syntax);
    Ok(check
        .violations
        .into_iter()
        .map(|violation| format!("{}:{violation}", path.display()))
        .collect())
}

fn check_workspace(root: &Path) -> Result<Vec<String>, Box<dyn Error>> {
    let canonical_root = root.canonicalize()?;
    let mut violations = Vec::new();
    for directory in ["crates", "apps", "tools"] {
        let source = canonical_root.join(directory);
        if !source.is_dir() {
            return Err(io::Error::other(format!(
                "Rust source directory is missing or unreadable: {}",
                source.display()
            ))
            .into());
        }
        for entry in WalkDir::new(source)
            .follow_links(true)
            .into_iter()
            .filter_entry(is_source_entry)
        {
            let entry = entry?;
            if entry.path_is_symlink() && !entry.path().canonicalize()?.starts_with(&canonical_root)
            {
                return Err(io::Error::other(format!(
                    "source link points outside the workspace: {}",
                    entry.path().display()
                ))
                .into());
            }
            if entry
                .path()
                .extension()
                .is_some_and(|extension| extension == "rs")
            {
                if !entry.file_type().is_file() {
                    return Err(io::Error::other(format!(
                        "Rust source is not a readable file: {}",
                        entry.path().display()
                    ))
                    .into());
                }
                violations.extend(check_source(entry.path())?);
            }
        }
    }
    violations.sort();
    Ok(violations)
}

/// Check first-party Rust source in the main, application, and tool workspaces.
pub fn run(root: &Path) -> Result<(), Box<dyn Error>> {
    let violations = check_workspace(root)?;
    if violations.is_empty() {
        println!("Rust source style rules: ok");
        return Ok(());
    }
    for violation in &violations {
        eprintln!("{violation}");
    }
    Err(io::Error::other(format!(
        "{} Rust source style violations",
        violations.len()
    ))
    .into())
}

#[cfg(test)]
mod tests {
    use std::fs;
    use std::path::{Path, PathBuf};
    use std::sync::atomic::{AtomicUsize, Ordering};

    use syn::visit::Visit;

    use super::StyleCheck;

    fn inspect(source: &str) -> Vec<String> {
        let syntax = syn::parse_file(source).unwrap();
        let mut check = StyleCheck::default();
        check.visit_file(&syntax);
        check.violations
    }

    #[test]
    fn requires_iterator_chains_to_be_bound_before_loops() {
        let violations = inspect(
            r#"
            fn example(values: &[u8], other: &[u8]) {
                for value in values { let _ = value; }
                for (index, value) in values.iter().enumerate() { let _ = (index, value); }
                for (index, pair) in values.iter().zip(other).enumerate() { let _ = (index, pair); }
                let filtered = values.iter().filter(|value| **value > 0).copied();
                for value in filtered { let _ = value; }
            }
            "#,
        );
        assert_eq!(violations.len(), 1, "{violations:?}");
        assert!(violations[0].contains("iterator chain"));
    }

    struct Workspace {
        root: PathBuf,
    }

    impl Workspace {
        fn new() -> Self {
            static NEXT: AtomicUsize = AtomicUsize::new(0);
            let root = std::env::temp_dir().join(format!(
                "cuenv-parameter-check-{}-{}",
                std::process::id(),
                NEXT.fetch_add(1, Ordering::Relaxed)
            ));
            for directory in ["crates", "apps", "tools"] {
                fs::create_dir_all(root.join(directory)).unwrap();
            }
            Self { root }
        }

        fn write(&self, relative: &str, content: &[u8]) {
            let path = self.root.join(relative);
            fs::create_dir_all(path.parent().unwrap()).unwrap();
            fs::write(path, content).unwrap();
        }
    }

    impl Drop for Workspace {
        fn drop(&mut self) {
            fs::remove_dir_all(&self.root).unwrap();
        }
    }

    #[test]
    fn checks_functions_trait_declarations_and_inherent_methods() {
        let violations = inspect(
            r#"
            fn bad(flag: bool, a: u8, b: u8, c: u8) {}
            fn fine(a: u8, b: u8, c: u8) {}
            struct Example;
            impl Example {
                fn bad(&self, flag: &bool) {}
                fn fine(self: Box<Self>, a: u8, b: u8, c: u8) {}
                fn too_many(&mut self, a: u8, b: u8, c: u8, d: u8) {}
            }
            trait Local { fn bad(&self, flag: bool); }
            "#,
        );
        assert_eq!(violations.len(), 5, "{violations:?}");
        assert!(
            violations
                .iter()
                .any(|line| line.contains("too_many has 4"))
        );
        assert!(!violations.iter().any(|line| line.contains("fine")));
    }

    #[test]
    fn finds_direct_bool_types_without_guessing_aliases_or_container_semantics() {
        let violations = inspect(
            r#"
            fn direct(_: bool) {}
            fn borrowed(_: &&bool) {}
            fn parens(_: (bool)) {}
            fn qualified(_: std::primitive::bool) {}
            fn core_qualified(_: ::core::primitive::bool) {}
            fn nested(_: Option<bool>) {}
            fn data(_: Vec<bool>) {}
            fn aliased(_: Flag) {}
            "#,
        );
        assert_eq!(violations.len(), 5, "{violations:?}");
    }

    #[test]
    fn rejects_optional_boolean_conversions_but_allows_framework_callbacks() {
        let violations = inspect(
            r#"
            impl From<bool> for Example { fn from(value: bool) -> Self { todo!() } }
            impl External for Example { fn callback(&self, value: bool) {} }
            "#,
        );
        assert_eq!(violations.len(), 1, "{violations:?}");
        assert!(violations[0].contains("from has a boolean parameter"));
    }

    #[test]
    fn foreign_signatures_are_exempt_but_rust_abi_and_nested_functions_are_checked() {
        let violations = inspect(
            r#"
            impl External for Example {
                fn callback(&self, flag: bool, a: u8, b: u8, c: u8) {
                    fn nested(flag: bool) {}
                }
            }
            extern "C" fn callback(flag: bool, a: u8, b: u8, c: u8) {
                fn nested_foreign(flag: bool) {}
            }
            extern "Rust" fn rust_abi(flag: bool) {}
            extern "C" { fn foreign(flag: bool, a: u8, b: u8, c: u8); }
            "#,
        );
        assert_eq!(violations.len(), 3, "{violations:?}");
        for name in ["nested", "nested_foreign", "rust_abi"] {
            assert!(violations.iter().any(|line| line.contains(name)));
        }
    }

    #[test]
    fn checks_every_source_root_and_ignores_build_outputs() {
        let workspace = Workspace::new();
        workspace.write("crates/example/src/lib.rs", b"fn crate_bad(_: bool) {}\n");
        workspace.write("apps/other/src/main.rs", b"fn app_bad(_: bool) {}\n");
        workspace.write("tools/new/src/main.rs", b"fn tool_bad(_: bool) {}\n");
        workspace.write("apps/other/target/broken.rs", b"not valid rust");
        workspace.write("tools/new/node_modules/pkg/broken.rs", b"not valid rust");
        workspace.write("crates/example/result/broken.rs", b"not valid rust");
        let violations = super::check_workspace(&workspace.root).unwrap();
        assert_eq!(violations.len(), 3);
        for name in ["crate_bad", "app_bad", "tool_bad"] {
            assert!(
                violations
                    .iter()
                    .any(|line| line.contains(name) && line.contains(":1:"))
            );
        }
        assert!(super::run(&workspace.root).is_err());
    }

    #[test]
    fn fails_for_unparseable_unreadable_and_missing_sources() {
        let workspace = Workspace::new();
        workspace.write("crates/bad.rs", b"fn incomplete(");
        assert!(
            super::check_workspace(&workspace.root)
                .unwrap_err()
                .to_string()
                .contains("cannot parse")
        );
        workspace.write("crates/bad.rs", &[0xff, 0xfe]);
        assert!(
            super::check_workspace(&workspace.root)
                .unwrap_err()
                .to_string()
                .contains("cannot read")
        );
        fs::remove_dir_all(workspace.root.join("apps")).unwrap();
        fs::remove_file(workspace.root.join("crates/bad.rs")).unwrap();
        assert!(
            super::check_workspace(&workspace.root)
                .unwrap_err()
                .to_string()
                .contains("missing or unreadable")
        );
    }

    #[cfg(unix)]
    #[test]
    fn follows_internal_source_links_and_rejects_links_outside_the_workspace() {
        let workspace = Workspace::new();
        workspace.write("crates/original.txt", b"fn linked(_: bool) {}\n");
        std::os::unix::fs::symlink(
            Path::new("original.txt"),
            workspace.root.join("crates/link.rs"),
        )
        .unwrap();
        let violations = super::check_workspace(&workspace.root).unwrap();
        assert!(
            violations
                .iter()
                .any(|line| line.contains("link.rs:1: linked"))
        );
        std::os::unix::fs::symlink(std::env::temp_dir(), workspace.root.join("tools/external"))
            .unwrap();
        assert!(
            super::check_workspace(&workspace.root)
                .unwrap_err()
                .to_string()
                .contains("outside the workspace")
        );
        fs::remove_file(workspace.root.join("tools/external")).unwrap();
        std::os::unix::fs::symlink("missing.rs", workspace.root.join("tools/dangling.rs")).unwrap();
        assert!(super::check_workspace(&workspace.root).is_err());
    }
}
