use super::{Arch, Os, Platform};

#[test]
fn expand_template_substitutes_every_placeholder() {
    let platform = Platform::new(Os::Darwin, Arch::Arm64);
    assert_eq!(
        platform.expand_template(
            "https://example.com/{version}/tool-{os}-{arch}.tar.gz",
            "2.0.0"
        ),
        "https://example.com/2.0.0/tool-darwin-aarch64.tar.gz"
    );
}

#[test]
fn expand_template_uses_provider_arch_names() {
    let platform = Platform::new(Os::Linux, Arch::X86_64);
    assert_eq!(
        platform.expand_template("{os}-{arch}", "1.0.0"),
        "linux-x86_64"
    );
}

#[test]
fn expand_template_repeats_and_ignores_unknown_placeholders() {
    let platform = Platform::new(Os::Linux, Arch::Arm64);
    assert_eq!(
        platform.expand_template("v{version}/{name}-{version}", "1.2.3"),
        "v1.2.3/{name}-1.2.3"
    );
    assert_eq!(
        platform.expand_template("static.tar.gz", "1.0.0"),
        "static.tar.gz"
    );
}
