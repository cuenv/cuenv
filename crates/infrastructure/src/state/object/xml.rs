//! The two S3 XML documents the store reads: a `ListObjectsV2` result and
//! an error body, deserialised with `quick-xml`.
//!
//! `quick-xml` never resolves DTDs or external entities, and the caller
//! bounds the body before it gets here. Keys arrive URL-encoded
//! (`encoding-type=url`) and are decoded here.

use quick_xml::events::Event;
use serde::Deserialize;

/// One page of a `ListObjectsV2` result.
#[derive(Default, Clone, PartialEq, Eq)]
pub struct ListPage {
    /// Object keys, decoded.
    pub keys: Vec<String>,
    /// The token for the next page, when the listing was truncated.
    pub next_continuation_token: Option<String>,
}

/// The `Code` and `Message` of an S3 error body.
#[derive(Default, Clone, PartialEq, Eq, Deserialize)]
#[serde(rename_all = "PascalCase")]
pub struct ErrorBody {
    /// The error code (`NoSuchKey`, `PreconditionFailed`, …).
    #[serde(default)]
    pub code: Option<String>,
    /// The provider-supplied message; it must not be included in diagnostics.
    #[serde(default)]
    pub message: Option<String>,
    /// The region a `PermanentRedirect` names, when it does.
    #[serde(default)]
    pub region: Option<String>,
}

impl std::fmt::Debug for ErrorBody {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        formatter.write_str("ErrorBody(<redacted>)")
    }
}

/// `<ListBucketResult>`, as far as the store reads it.
#[derive(Default, Deserialize)]
#[serde(rename_all = "PascalCase")]
struct ListBucketResult {
    #[serde(default)]
    contents: Vec<Contents>,
    is_truncated: bool,
    #[serde(default)]
    encoding_type: Option<String>,
    #[serde(default)]
    next_continuation_token: Option<String>,
}

/// One `<Contents>` entry.
#[derive(Deserialize)]
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

/// Parse a complete `ListObjectsV2` page whose keys are URL-encoded.
///
/// # Errors
///
/// Returns an error when the page, key encoding, or pagination is invalid.
pub fn parse_list(document: &str) -> Result<ListPage, String> {
    if !is_list_document(document) {
        return Err("invalid ListObjectsV2 document".to_string());
    }
    let result: ListBucketResult =
        quick_xml::de::from_str(document).map_err(|_| "invalid ListObjectsV2 document")?;
    if result.encoding_type.as_deref() != Some("url") {
        return Err("the listing did not confirm URL-encoded keys".to_string());
    }
    let keys = result
        .contents
        .into_iter()
        .map(|contents| {
            percent_decode(&contents.key).ok_or_else(|| "invalid listing key encoding".to_string())
        })
        .collect::<Result<Vec<_>, _>>()?;
    let next_continuation_token = if result.is_truncated {
        Some(
            result
                .next_continuation_token
                .filter(|token| !token.is_empty())
                .ok_or_else(|| "truncated listing has no continuation token".to_string())?,
        )
    } else if result.next_continuation_token.is_some() {
        return Err("complete listing has a continuation token".to_string());
    } else {
        None
    };
    Ok(ListPage {
        keys,
        next_continuation_token,
    })
}

fn is_list_document(document: &str) -> bool {
    let mut reader = quick_xml::Reader::from_str(document);
    let mut seen_root = false;
    let mut depth = 0_usize;
    loop {
        match reader.read_event() {
            Ok(Event::Start(element)) => {
                if depth == 0 {
                    if seen_root || element.local_name().as_ref() != "ListBucketResult" {
                        return false;
                    }
                    seen_root = true;
                }
                depth += 1;
            }
            Ok(Event::Empty(element)) if depth == 0 => {
                if seen_root || element.local_name().as_ref() != "ListBucketResult" {
                    return false;
                }
                seen_root = true;
            }
            Ok(Event::End(_)) => {
                let Some(remaining) = depth.checked_sub(1) else {
                    return false;
                };
                depth = remaining;
            }
            Ok(Event::Text(text)) if depth == 0 => {
                if !text.as_ref().bytes().all(|byte| byte.is_ascii_whitespace()) {
                    return false;
                }
            }
            Ok(Event::CData(_) | Event::GeneralRef(_)) if depth == 0 => return false,
            Ok(Event::DocType(_)) | Err(_) => return false,
            Ok(Event::Decl(_)) if seen_root => return false,
            Ok(Event::Eof) => return seen_root && depth == 0,
            _ => {}
        }
    }
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
             <EncodingType>url</EncodingType>\
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
            "<ListBucketResult><EncodingType>url</EncodingType>\
             <IsTruncated>false</IsTruncated></ListBucketResult>",
        )
        .unwrap();
        assert!(last.keys.is_empty());
        assert_eq!(last.next_continuation_token, None);
    }

    #[test]
    fn incomplete_or_inconsistent_listings_are_refused() {
        for document in [
            "<Error><EncodingType>url</EncodingType><IsTruncated>false</IsTruncated></Error>",
            "<ListBucketResult><EncodingType>url</EncodingType></ListBucketResult>",
            "<ListBucketResult><IsTruncated>false</IsTruncated></ListBucketResult>",
            "<ListBucketResult><EncodingType>raw</EncodingType><IsTruncated>false</IsTruncated></ListBucketResult>",
            "<ListBucketResult><EncodingType>url</EncodingType><IsTruncated>true</IsTruncated></ListBucketResult>",
            "<ListBucketResult><EncodingType>url</EncodingType><IsTruncated>true</IsTruncated><NextContinuationToken/></ListBucketResult>",
            "<ListBucketResult><EncodingType>url</EncodingType><IsTruncated>false</IsTruncated><NextContinuationToken>unexpected</NextContinuationToken></ListBucketResult>",
            "<ListBucketResult><EncodingType>url</EncodingType><IsTruncated>false</IsTruncated><Contents><Key>bad%zz</Key></Contents></ListBucketResult>",
            "<ListBucketResult><EncodingType>url</EncodingType><IsTruncated>false</IsTruncated><Contents><Key>%FF</Key></Contents></ListBucketResult>",
            "<ListBucketResult><EncodingType>url</EncodingType><IsTruncated>false</IsTruncated></ListBucketResult><extra/>",
            "<ListBucketResult><EncodingType>url</EncodingType><IsTruncated>false</IsTruncated>",
        ] {
            assert!(parse_list(document).is_err(), "accepted {document}");
        }
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
        assert_eq!(format!("{error:?}"), "ErrorBody(<redacted>)");
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
