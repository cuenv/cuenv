//! The two S3 XML documents the store reads: a `ListObjectsV2` result and
//! an error body.
//!
//! Both are flat and small, so they are read with a minimal scanner instead
//! of an XML library: element text is taken between an opening and a
//! closing tag, and only the five predefined entities are decoded. Nothing
//! else is interpreted (no DTD, no external entities), and the caller bounds
//! the body before it gets here. Keys arrive URL-encoded (`encoding-type=url`)
//! and are decoded here.

/// One page of a `ListObjectsV2` result.
#[derive(Debug, Default, Clone, PartialEq, Eq)]
pub struct ListPage {
    /// Object keys, decoded.
    pub keys: Vec<String>,
    /// The token for the next page, when the listing was truncated.
    pub next_continuation_token: Option<String>,
}

/// The `Code` and `Message` of an S3 error body.
#[derive(Debug, Default, Clone, PartialEq, Eq)]
pub struct ErrorBody {
    /// The error code (`NoSuchKey`, `PreconditionFailed`, …).
    pub code: Option<String>,
    /// The message, never quoted without redaction.
    pub message: Option<String>,
    /// The region a `PermanentRedirect` names, when it does.
    pub region: Option<String>,
}

/// Every text of `<tag>…</tag>` in `document`, in order.
fn texts<'document>(document: &'document str, tag: &str) -> Vec<&'document str> {
    let open = format!("<{tag}>");
    let close = format!("</{tag}>");
    let mut found = Vec::new();
    let mut rest = document;
    while let Some(start) = rest.find(&open) {
        let after = &rest[start + open.len()..];
        let Some(end) = after.find(&close) else {
            break;
        };
        found.push(&after[..end]);
        rest = &after[end + close.len()..];
    }
    found
}

/// Decode the five predefined XML entities; anything else is left as is.
fn unescape(text: &str) -> String {
    text.replace("&lt;", "<")
        .replace("&gt;", ">")
        .replace("&quot;", "\"")
        .replace("&apos;", "'")
        .replace("&amp;", "&")
}

/// Decode `%XX` escapes (as `encoding-type=url` produces); `None` for a
/// malformed escape or bytes that are not UTF-8.
#[must_use]
pub fn percent_decode(text: &str) -> Option<String> {
    let bytes = text.as_bytes();
    let mut decoded = Vec::with_capacity(bytes.len());
    let mut index = 0;
    while index < bytes.len() {
        match bytes[index] {
            b'%' => {
                let hex = text.get(index + 1..index + 3)?;
                decoded.push(u8::from_str_radix(hex, 16).ok()?);
                index += 3;
            }
            b'+' => {
                // S3 encodes spaces as `+` in URL-encoded listings.
                decoded.push(b' ');
                index += 1;
            }
            byte => {
                decoded.push(byte);
                index += 1;
            }
        }
    }
    String::from_utf8(decoded).ok()
}

/// Parse a `ListObjectsV2` result whose keys are URL-encoded. A key that
/// does not decode is dropped (it cannot be one of ours).
#[must_use]
pub fn parse_list(document: &str) -> ListPage {
    let keys = texts(document, "Contents")
        .into_iter()
        .filter_map(|contents| texts(contents, "Key").first().copied())
        .filter_map(|key| percent_decode(&unescape(key)))
        .collect();
    let truncated = texts(document, "IsTruncated")
        .first()
        .is_some_and(|value| value.trim() == "true");
    let next_continuation_token = truncated
        .then(|| texts(document, "NextContinuationToken").first().copied())
        .flatten()
        .map(unescape);
    ListPage {
        keys,
        next_continuation_token,
    }
}

/// Parse an S3 error body. Missing fields are `None`.
#[must_use]
pub fn parse_error(document: &str) -> ErrorBody {
    let field = |tag: &str| {
        texts(document, tag)
            .first()
            .map(|text| unescape(text))
            .filter(|text| !text.is_empty())
    };
    ErrorBody {
        code: field("Code"),
        message: field("Message"),
        region: field("Region"),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn listings_decode_keys_and_the_continuation_token() {
        let page = parse_list(
            "<?xml version=\"1.0\" encoding=\"UTF-8\"?>\
             <ListBucketResult><Name>b</Name><Prefix>_locks%2F</Prefix>\
             <Contents><Key>_locks%2Fa%2Fb.json</Key><ETag>&quot;x&quot;</ETag></Contents>\
             <Contents><Key>a+b%26c</Key></Contents>\
             <Contents><Key>bad%zz</Key></Contents>\
             <IsTruncated>true</IsTruncated>\
             <NextContinuationToken>1ueGcxLPRx1Tr/XYExHnhbYLgveDs2J/wm36Hy4vbOwM=</NextContinuationToken>\
             </ListBucketResult>",
        );
        assert_eq!(page.keys, ["_locks/a/b.json", "a b&c"]);
        assert_eq!(
            page.next_continuation_token.as_deref(),
            Some("1ueGcxLPRx1Tr/XYExHnhbYLgveDs2J/wm36Hy4vbOwM=")
        );
        let last = parse_list(
            "<ListBucketResult><IsTruncated>false</IsTruncated>\
             <NextContinuationToken>ignored</NextContinuationToken></ListBucketResult>",
        );
        assert!(last.keys.is_empty());
        assert_eq!(last.next_continuation_token, None);
    }

    #[test]
    fn error_bodies_yield_code_message_and_region() {
        let error = parse_error(
            "<Error><Code>PermanentRedirect</Code><Message>Use &lt;eu-west-2&gt;</Message>\
             <Region>eu-west-2</Region><StringToSign>secret-ish</StringToSign></Error>",
        );
        assert_eq!(error.code.as_deref(), Some("PermanentRedirect"));
        assert_eq!(error.message.as_deref(), Some("Use <eu-west-2>"));
        assert_eq!(error.region.as_deref(), Some("eu-west-2"));
        assert_eq!(parse_error("not xml"), ErrorBody::default());
    }

    #[test]
    fn entities_beyond_the_predefined_five_are_left_alone() {
        assert_eq!(
            unescape("&amp;lt; &#x41; &unknown;"),
            "&lt; &#x41; &unknown;"
        );
        assert_eq!(percent_decode("%"), None);
        assert_eq!(percent_decode("%C3%A9"), Some("é".to_string()));
    }
}
