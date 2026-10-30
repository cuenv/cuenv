//! The two S3 XML documents the store reads: a `ListObjectsV2` result and
//! an error body, deserialised with `quick-xml`.
//!
//! `quick-xml` never resolves DTDs or external entities, and the caller
//! bounds the body before it gets here. Keys arrive URL-encoded
//! (`encoding-type=url`) and are decoded here.

use serde::Deserialize;

/// One page of a `ListObjectsV2` result.
#[derive(Debug, Default, Clone, PartialEq, Eq)]
pub struct ListPage {
    /// Object keys, decoded.
    pub keys: Vec<String>,
    /// The token for the next page, when the listing was truncated.
    pub next_continuation_token: Option<String>,
}

/// The `Code` and `Message` of an S3 error body.
#[derive(Debug, Default, Clone, PartialEq, Eq, Deserialize)]
#[serde(rename_all = "PascalCase")]
pub struct ErrorBody {
    /// The error code (`NoSuchKey`, `PreconditionFailed`, …).
    #[serde(default)]
    pub code: Option<String>,
    /// The message, never quoted without redaction.
    #[serde(default)]
    pub message: Option<String>,
    /// The region a `PermanentRedirect` names, when it does.
    #[serde(default)]
    pub region: Option<String>,
}

/// `<ListBucketResult>`, as far as the store reads it.
#[derive(Debug, Default, Deserialize)]
#[serde(rename_all = "PascalCase")]
struct ListBucketResult {
    #[serde(default)]
    contents: Vec<Contents>,
    #[serde(default)]
    is_truncated: bool,
    #[serde(default)]
    next_continuation_token: Option<String>,
}

/// One `<Contents>` entry.
#[derive(Debug, Deserialize)]
#[serde(rename_all = "PascalCase")]
struct Contents {
    key: String,
}

/// Decode a key from an `encoding-type=url` listing: `+` is a space and
/// `%XX` a byte. `None` for a malformed escape or bytes that are not UTF-8.
#[must_use]
pub fn percent_decode(text: &str) -> Option<String> {
    let well_formed = text.match_indices('%').all(|(index, _)| {
        text.get(index + 1..index + 3)
            .is_some_and(|hex| hex.bytes().all(|byte| byte.is_ascii_hexdigit()))
    });
    if !well_formed {
        return None;
    }
    let spaced = text.replace('+', " ");
    percent_encoding::percent_decode_str(&spaced)
        .decode_utf8()
        .ok()
        .map(std::borrow::Cow::into_owned)
}

/// Parse a `ListObjectsV2` result whose keys are URL-encoded. A key that
/// does not decode is dropped (it cannot be one of ours).
///
/// # Errors
///
/// Returns the parser's message when `document` is not a listing.
pub fn parse_list(document: &str) -> Result<ListPage, String> {
    let result: ListBucketResult =
        quick_xml::de::from_str(document).map_err(|error| error.to_string())?;
    let keys = result
        .contents
        .into_iter()
        .filter_map(|contents| percent_decode(&contents.key))
        .collect();
    let next_continuation_token = result
        .is_truncated
        .then_some(result.next_continuation_token)
        .flatten()
        .filter(|token| !token.is_empty());
    Ok(ListPage {
        keys,
        next_continuation_token,
    })
}

/// Parse an S3 error body. Missing fields, and a body that is not an S3
/// error document at all, give `None`s.
#[must_use]
pub fn parse_error(document: &str) -> ErrorBody {
    let body: ErrorBody = quick_xml::de::from_str(document).unwrap_or_default();
    let present = |field: Option<String>| field.filter(|text| !text.is_empty());
    ErrorBody {
        code: present(body.code),
        message: present(body.message),
        region: present(body.region),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn listings_decode_keys_and_the_continuation_token() {
        let page = parse_list(
            "<?xml version=\"1.0\" encoding=\"UTF-8\"?>\
             <ListBucketResult xmlns=\"http://s3.amazonaws.com/doc/2006-03-01/\">\
             <Name>b</Name><Prefix>_locks%2F</Prefix>\
             <Contents><Key>_locks%2Fa%2Fb.json</Key><ETag>&quot;x&quot;</ETag>\
             <Owner><ID>o</ID></Owner></Contents>\
             <Contents><Key>a+b%26c</Key></Contents>\
             <Contents><Key><![CDATA[c%2Fd]]></Key></Contents>\
             <Contents><Key>bad%zz</Key></Contents>\
             <IsTruncated>true</IsTruncated>\
             <NextContinuationToken>1ueGcxLPRx1Tr/XYExHnhbYLgveDs2J/wm36Hy4vbOwM=</NextContinuationToken>\
             </ListBucketResult>",
        )
        .unwrap();
        assert_eq!(page.keys, ["_locks/a/b.json", "a b&c", "c/d"]);
        assert_eq!(
            page.next_continuation_token.as_deref(),
            Some("1ueGcxLPRx1Tr/XYExHnhbYLgveDs2J/wm36Hy4vbOwM=")
        );
        let last = parse_list(
            "<ListBucketResult><IsTruncated>false</IsTruncated>\
             <NextContinuationToken>ignored</NextContinuationToken></ListBucketResult>",
        )
        .unwrap();
        assert!(last.keys.is_empty());
        assert_eq!(last.next_continuation_token, None);
    }

    #[test]
    fn a_listing_that_is_not_xml_is_an_error() {
        assert!(parse_list("<html><body>gateway timeout").is_err());
    }

    #[test]
    fn error_bodies_yield_code_message_and_region() {
        let error = parse_error(
            "<?xml version=\"1.0\" encoding=\"UTF-8\"?>\
             <Error><Code>PermanentRedirect</Code><Message>Use &lt;eu-west-2&gt; &#x41;</Message>\
             <Region>eu-west-2</Region><StringToSign>secret-ish</StringToSign></Error>",
        );
        assert_eq!(error.code.as_deref(), Some("PermanentRedirect"));
        assert_eq!(error.message.as_deref(), Some("Use <eu-west-2> A"));
        assert_eq!(error.region.as_deref(), Some("eu-west-2"));
        assert_eq!(parse_error("not xml"), ErrorBody::default());
        assert_eq!(
            parse_error("<Error><Code></Code></Error>"),
            ErrorBody::default()
        );
    }

    #[test]
    fn document_type_declarations_are_not_expanded() {
        let error = parse_error(
            "<?xml version=\"1.0\"?><!DOCTYPE Error [<!ENTITY x SYSTEM \"file:///etc/passwd\">]>\
             <Error><Code>&x;</Code></Error>",
        );
        assert!(!error.code.unwrap_or_default().contains("root:"));
    }

    #[test]
    fn percent_decoding_refuses_malformed_escapes() {
        assert_eq!(percent_decode("%"), None);
        assert_eq!(percent_decode("%4"), None);
        assert_eq!(percent_decode("%zz"), None);
        assert_eq!(percent_decode("%FF"), None);
        assert_eq!(percent_decode("%C3%A9"), Some("é".to_string()));
        assert_eq!(percent_decode("a%2Bb+c"), Some("a+b c".to_string()));
    }
}
