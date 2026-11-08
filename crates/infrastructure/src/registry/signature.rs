//! Verifying a provider release's signed `SHA256SUMS`.
//!
//! A registry publishes, with each download, the release's `SHA256SUMS`, a
//! detached OpenPGP signature over it and the public keys that may have made
//! that signature. cuenv checks the signature with a key its trust policy
//! accepts, then requires `SHA256SUMS` to list the archive with the SHA-256
//! the registry reported (and the download produced). A release host or
//! mirror that serves another archive, or another `SHA256SUMS`, is refused.
//!
//! Trust policy, by provider:
//!
//! - `signingKeys` in the provider declaration: only those fingerprints.
//! - providers under `registry.terraform.io/hashicorp/`: only HashiCorp's
//!   release key, as Terraform requires for official providers.
//! - every other provider: any valid key the registry lists for it, which is
//!   Terraform's policy for partner and community providers. The registry
//!   vouches for itself here; pin `signingKeys` to anchor it.

use pgp::composed::{Deserializable, DetachedSignature, SignedPublicKey};
use pgp::types::KeyDetails;

use super::{DEFAULT_REGISTRY, ProviderSource};
use crate::error::{InfrastructureError, Result};

/// HashiCorp's release signing key, which signs every provider in the
/// `hashicorp` namespace of the public registry.
pub const HASHICORP_SIGNING_KEY: &str = "C874011F0AB405110D02105534365D9472D7468F";

/// Which keys may sign a release.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum SigningTrust {
    /// Only these fingerprints (uppercase hexadecimal).
    Keys(Vec<String>),
    /// Any valid key the registry lists for the release.
    Registry,
}

impl SigningTrust {
    /// The policy for `source`, given the fingerprints its declaration pins.
    ///
    /// # Errors
    ///
    /// Returns [`InfrastructureError::Configuration`] for a pinned value that
    /// is not a 40- or 64-digit hexadecimal fingerprint.
    pub fn for_source(source: &ProviderSource, pinned: &[String]) -> Result<Self> {
        if !pinned.is_empty() {
            return pinned
                .iter()
                .map(|fingerprint| normalize_fingerprint(fingerprint))
                .collect::<Result<Vec<_>>>()
                .map(Self::Keys);
        }
        if source.hostname == DEFAULT_REGISTRY && source.namespace == "hashicorp" {
            return Ok(Self::Keys(vec![HASHICORP_SIGNING_KEY.to_string()]));
        }
        Ok(Self::Registry)
    }

    fn accepts(&self, fingerprint: &str) -> bool {
        match self {
            Self::Keys(keys) => keys.iter().any(|key| key == fingerprint),
            Self::Registry => true,
        }
    }
}

/// A fingerprint as uppercase hexadecimal without spaces.
///
/// # Errors
///
/// Returns [`InfrastructureError::Configuration`] for anything that is not
/// 40 (v4) or 64 (v6) hexadecimal digits, spaces aside.
pub fn normalize_fingerprint(text: &str) -> Result<String> {
    let compact: String = text.chars().filter(|c| !c.is_whitespace()).collect();
    if matches!(compact.len(), 40 | 64) && compact.chars().all(|c| c.is_ascii_hexdigit()) {
        Ok(compact.to_ascii_uppercase())
    } else {
        Err(InfrastructureError::configuration(
            "a provider signing key is an OpenPGP fingerprint: 40 or 64 hexadecimal digits",
        ))
    }
}

/// What [`verify`] checks.
pub struct SignedSums<'input> {
    /// The release, for messages.
    pub release: &'input str,
    /// The `SHA256SUMS` document.
    pub sums: &'input [u8],
    /// Its detached signature, binary or armored.
    pub signature: &'input [u8],
    /// ASCII-armored public keys the registry lists.
    pub keys: &'input [String],
    /// Which of them may sign.
    pub trust: &'input SigningTrust,
}

/// Verify the signature over `SHA256SUMS`, returning the fingerprint of the
/// key that made it.
///
/// # Errors
///
/// Returns [`InfrastructureError::Install`] when no accepted, valid key made
/// the signature.
pub fn verify(input: &SignedSums<'_>) -> Result<String> {
    let SignedSums {
        release,
        sums,
        signature,
        keys,
        trust,
    } = *input;
    let signature = parse_signature(signature).map_err(|error| {
        InfrastructureError::install(format!(
            "{release}: the registry's SHA256SUMS signature cannot be read: {error}"
        ))
    })?;
    let mut offered = Vec::new();
    for armor in keys {
        let Ok((key, _)) = SignedPublicKey::from_string(armor) else {
            continue;
        };
        let fingerprint = format!("{:X}", key.fingerprint());
        offered.push(fingerprint.clone());
        if !trust.accepts(&fingerprint) || key.verify_bindings().is_err() {
            continue;
        }
        let primary = signature.verify(&key.primary_key, sums).is_ok();
        let subkey = || {
            key.public_subkeys.iter().any(|subkey| {
                subkey.verify_bindings(&key.primary_key).is_ok()
                    && signature.verify(&subkey.key, sums).is_ok()
            })
        };
        if primary || subkey() {
            return Ok(fingerprint);
        }
    }
    let accepted = match trust {
        SigningTrust::Keys(keys) => format!("a key with fingerprint {}", keys.join(" or ")),
        SigningTrust::Registry => "a key the registry lists".to_string(),
    };
    Err(InfrastructureError::install(format!(
        "{release}: SHA256SUMS is not signed by {accepted} (the registry offered {}); refusing \
         the release",
        if offered.is_empty() {
            "no readable key".to_string()
        } else {
            offered.join(", ")
        }
    )))
}

fn parse_signature(bytes: &[u8]) -> pgp::errors::Result<DetachedSignature> {
    if bytes.starts_with(b"-----BEGIN") {
        DetachedSignature::from_armor_single(bytes).map(|(signature, _)| signature)
    } else {
        DetachedSignature::from_bytes(bytes)
    }
}

/// Require `sums` to list `filename` with the hex SHA-256 `sha256`.
///
/// # Errors
///
/// Returns [`InfrastructureError::Install`] when it does not.
pub fn require_listed(sums: &[u8], filename: &str, sha256: &str) -> Result<()> {
    let listed = String::from_utf8_lossy(sums).lines().any(|line| {
        let mut fields = line.split_whitespace();
        matches!(
            (fields.next(), fields.next(), fields.next()),
            (Some(hash), Some(name), None)
                if name.trim_start_matches('*') == filename && hash.eq_ignore_ascii_case(sha256)
        )
    });
    if listed {
        Ok(())
    } else {
        Err(InfrastructureError::install(format!(
            "the signed SHA256SUMS does not list {filename} with SHA-256 {sha256}; refusing the \
             release"
        )))
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn source(text: &str) -> ProviderSource {
        ProviderSource::parse(text).unwrap()
    }

    #[test]
    fn hashicorp_providers_trust_only_hashicorps_key_unless_pinned() {
        assert_eq!(
            SigningTrust::for_source(&source("hashicorp/random"), &[]).unwrap(),
            SigningTrust::Keys(vec![HASHICORP_SIGNING_KEY.to_string()])
        );
        assert_eq!(
            SigningTrust::for_source(&source("cloudflare/cloudflare"), &[]).unwrap(),
            SigningTrust::Registry
        );
        let pinned = "c874 011f 0ab4 0511 0d02  1055 3436 5d94 72d7 468f".to_string();
        assert_eq!(
            SigningTrust::for_source(&source("cloudflare/cloudflare"), &[pinned]).unwrap(),
            SigningTrust::Keys(vec![HASHICORP_SIGNING_KEY.to_string()])
        );
        assert!(
            SigningTrust::for_source(
                &source("cloudflare/cloudflare"),
                &["34365D9472D7468F".into()]
            )
            .is_err(),
            "a short key ID is not a fingerprint"
        );
    }

    #[test]
    fn sums_must_list_the_exact_archive() {
        let sums = b"aa11  terraform-provider-x_1.0.0_linux_amd64.zip\nbb22  terraform-provider-x_1.0.0_darwin_arm64.zip\n";
        assert!(require_listed(sums, "terraform-provider-x_1.0.0_linux_amd64.zip", "AA11").is_ok());
        assert!(
            require_listed(sums, "terraform-provider-x_1.0.0_linux_amd64.zip", "bb22").is_err()
        );
        assert!(
            require_listed(sums, "terraform-provider-x_1.0.0_linux_arm64.zip", "aa11").is_err()
        );
    }

    #[test]
    fn an_unsigned_or_garbled_signature_is_refused() {
        let error = verify(&SignedSums {
            release: "x 1.0.0",
            sums: b"aa11  x.zip\n",
            signature: b"not a signature",
            keys: &[],
            trust: &SigningTrust::Registry,
        })
        .unwrap_err()
        .to_string();
        assert!(error.contains("signature cannot be read"), "{error}");
    }

    const HASHICORP_KEY: &str = include_str!("testdata/hashicorp.asc");
    const SUMS: &[u8] = include_bytes!("testdata/random_3.9.1_SHA256SUMS");
    const SUMS_SIGNATURE: &[u8] = include_bytes!("testdata/random_3.9.1_SHA256SUMS.sig");

    #[test]
    fn a_real_hashicorp_release_verifies_under_the_official_policy() {
        let keys = [HASHICORP_KEY.to_string()];
        let trust = SigningTrust::for_source(&source("hashicorp/random"), &[]).unwrap();
        let fingerprint = verify(&SignedSums {
            release: "hashicorp/random 3.9.1",
            sums: SUMS,
            signature: SUMS_SIGNATURE,
            keys: &keys,
            trust: &trust,
        })
        .unwrap();
        assert_eq!(fingerprint, HASHICORP_SIGNING_KEY);
        require_listed(
            SUMS,
            "terraform-provider-random_3.9.1_linux_amd64.zip",
            "60b4f8a8ef18f52bf8e19215229dae408bee732825964092db7c989fd2de4097",
        )
        .unwrap();
    }

    #[test]
    fn a_changed_sums_file_or_an_unaccepted_key_is_refused() {
        let keys = [HASHICORP_KEY.to_string()];
        let mut tampered = SUMS.to_vec();
        tampered[0] = if tampered[0] == b'0' { b'1' } else { b'0' };
        let official = SigningTrust::Keys(vec![HASHICORP_SIGNING_KEY.to_string()]);
        let error = verify(&SignedSums {
            release: "hashicorp/random 3.9.1",
            sums: &tampered,
            signature: SUMS_SIGNATURE,
            keys: &keys,
            trust: &official,
        })
        .unwrap_err()
        .to_string();
        assert!(error.contains("is not signed by"), "{error}");

        let other = SigningTrust::Keys(vec!["0".repeat(40)]);
        let error = verify(&SignedSums {
            release: "hashicorp/random 3.9.1",
            sums: SUMS,
            signature: SUMS_SIGNATURE,
            keys: &keys,
            trust: &other,
        })
        .unwrap_err()
        .to_string();
        assert!(
            error.contains(HASHICORP_SIGNING_KEY),
            "the offered key is named: {error}"
        );

        // The registry cannot vouch for a hashicorp/ provider with no key.
        let error = verify(&SignedSums {
            release: "hashicorp/random 3.9.1",
            sums: SUMS,
            signature: SUMS_SIGNATURE,
            keys: &[],
            trust: &official,
        })
        .unwrap_err()
        .to_string();
        assert!(error.contains("no readable key"), "{error}");
    }
}
