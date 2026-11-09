use super::{ToolExtract, ToolProvider, ToolResolveRequest, ToolSource, UrlToolProvider};
use cuenv_tool_runtime::{Arch, Os, Platform};

#[test]
fn test_provider_name() {
    let provider = UrlToolProvider::new();
    assert_eq!(provider.name(), "url");
}

#[test]
fn test_provider_new_defers_client_initialization() {
    let provider = UrlToolProvider::new();
    assert!(provider.client.get().is_none());
}

#[test]
fn test_can_handle_url_source() {
    let provider = UrlToolProvider::new();
    let source = ToolSource::Url {
        url: "https://example.com/tool".to_string(),
        extract: vec![],
    };
    assert!(provider.can_handle(&source));
}

#[test]
fn test_cannot_handle_github_source() {
    let provider = UrlToolProvider::new();
    let source = ToolSource::GitHub {
        repo: "owner/repo".to_string(),
        tag: "v1.0.0".to_string(),
        asset: "tool.tar.gz".to_string(),
        extract: vec![],
    };
    assert!(!provider.can_handle(&source));
}

#[tokio::test]
async fn test_resolve_simple_url() {
    let provider = UrlToolProvider::new();
    let config = serde_json::json!({
        "type": "url",
        "url": "https://example.com/tool-{version}-{os}-{arch}.tar.gz"
    });
    let platform = Platform::new(Os::Linux, Arch::X86_64);
    let request = ToolResolveRequest {
        tool_name: "mytool",
        version: "1.0.0",
        platform: &platform,
        config: &config,
        token: None,
    };

    let resolved = provider.resolve(&request).await.unwrap();
    assert_eq!(resolved.name, "mytool");
    assert_eq!(resolved.version, "1.0.0");

    match &resolved.source {
        ToolSource::Url { url, extract } => {
            assert_eq!(url, "https://example.com/tool-1.0.0-linux-x86_64.tar.gz");
            assert!(extract.is_empty());
        }
        _ => panic!("Expected URL source"),
    }
}

#[tokio::test]
async fn test_resolve_url_with_path() {
    let provider = UrlToolProvider::new();
    let config = serde_json::json!({
        "type": "url",
        "url": "https://example.com/tool-{version}.tar.gz",
        "path": "tool-{version}/bin/tool"
    });
    let platform = Platform::new(Os::Linux, Arch::X86_64);
    let request = ToolResolveRequest {
        tool_name: "mytool",
        version: "2.0.0",
        platform: &platform,
        config: &config,
        token: None,
    };

    let resolved = provider.resolve(&request).await.unwrap();
    match &resolved.source {
        ToolSource::Url { url, extract } => {
            assert_eq!(url, "https://example.com/tool-2.0.0.tar.gz");
            assert_eq!(extract.len(), 1);
            match &extract[0] {
                ToolExtract::Bin { path, .. } => {
                    assert_eq!(path, "tool-2.0.0/bin/tool");
                }
                _ => panic!("Expected Bin extract"),
            }
        }
        _ => panic!("Expected URL source"),
    }
}

#[tokio::test]
async fn test_resolve_url_missing_url_field() {
    let provider = UrlToolProvider::new();
    let config = serde_json::json!({
        "type": "url"
    });
    let platform = Platform::new(Os::Linux, Arch::X86_64);
    let request = ToolResolveRequest {
        tool_name: "mytool",
        version: "1.0.0",
        platform: &platform,
        config: &config,
        token: None,
    };

    assert!(provider.resolve(&request).await.is_err());
}
