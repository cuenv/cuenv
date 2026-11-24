//! Fingerprints of what each generator last wrote, kept in
//! `.cuenv/autosync.json` so that an unchanged project costs a few `stat`
//! calls instead of a render.

use serde::{Deserialize, Serialize};
use sha2::{Digest, Sha256};
use std::collections::BTreeMap;
use std::fs;
use std::io::Write;
use std::path::{Path, PathBuf};
use std::time::UNIX_EPOCH;

const STORE_DIR: &str = ".cuenv";
const STORE_FILE: &str = "autosync.json";
const LOCK_FILE: &str = "autosync.lock";

/// The size and modification time of a file, or its absence.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub enum FileStamp {
    Missing,
    Present { len: u64, modified_ns: u128 },
}

impl FileStamp {
    pub fn of(path: &Path) -> Self {
        fs::metadata(path).map_or(Self::Missing, |metadata| Self::Present {
            len: metadata.len(),
            modified_ns: metadata
                .modified()
                .ok()
                .and_then(|time| time.duration_since(UNIX_EPOCH).ok())
                .map_or(0, |duration| duration.as_nanos()),
        })
    }
}

/// Stamps of a set of files, keyed by path.
pub type Stamps = BTreeMap<String, FileStamp>;

pub fn stamps<'path>(paths: impl IntoIterator<Item = &'path Path>) -> Stamps {
    paths
        .into_iter()
        .map(|path| (path.display().to_string(), FileStamp::of(path)))
        .collect()
}

/// What codegen of one project was last synced from and wrote.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct CodegenPrint {
    /// Digest of the evaluated codegen and formatter configuration.
    pub config: String,
    /// The generated files after the sync, formatting included.
    pub outputs: Stamps,
}

/// The `.rules.cue` files one rules sync read: one directory's, or every
/// file a workspace sync found.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct RulesPrint {
    pub repo_root: String,
    pub files: Stamps,
}

#[derive(Debug, Default, Serialize, Deserialize)]
pub struct Fingerprints {
    #[serde(default)]
    pub codegen: BTreeMap<String, CodegenPrint>,
    /// Keyed by the synced directory, or `*` for a workspace sync.
    #[serde(default)]
    pub rules: BTreeMap<String, RulesPrint>,
}

/// SHA-256 of a value's JSON with object keys sorted, as hex, so that a
/// map serialized in hash order digests the same on every run.
pub fn digest(value: &impl Serialize) -> String {
    let canonical = serde_json::to_value(value).map_or(serde_json::Value::Null, sorted);
    let bytes = serde_json::to_vec(&canonical).unwrap_or_default();
    Sha256::digest(&bytes)
        .iter()
        .fold(String::with_capacity(64), |mut hex, byte| {
            use std::fmt::Write as _;
            let _ = write!(hex, "{byte:02x}");
            hex
        })
}

fn sorted(value: serde_json::Value) -> serde_json::Value {
    match value {
        serde_json::Value::Object(map) => {
            let entries: BTreeMap<String, serde_json::Value> = map
                .into_iter()
                .map(|(key, value)| (key, sorted(value)))
                .collect();
            serde_json::Value::Object(entries.into_iter().collect())
        }
        serde_json::Value::Array(items) => {
            serde_json::Value::Array(items.into_iter().map(sorted).collect())
        }
        other => other,
    }
}

/// The fingerprint file of a directory, held under an exclusive lock for as
/// long as the value lives so that two cuenv processes never sync at once.
pub struct Store {
    path: PathBuf,
    pub prints: Fingerprints,
    _lock: Option<fs::File>,
}

impl Store {
    /// Open the store of `root`, waiting for any other cuenv syncing there.
    /// `None` when `.cuenv` cannot be created, as in a read-only checkout:
    /// syncing is then left to `cuenv sync`.
    pub fn open(root: &Path) -> Option<Self> {
        let dir = root.join(STORE_DIR);
        fs::create_dir_all(&dir).ok()?;
        let lock = lock(&dir.join(LOCK_FILE));
        let path = dir.join(STORE_FILE);
        let prints = fs::read(&path)
            .ok()
            .and_then(|bytes| serde_json::from_slice(&bytes).ok())
            .unwrap_or_default();
        Some(Self {
            path,
            prints,
            _lock: lock,
        })
    }

    /// Open the store of `root` only if one was written there before.
    pub fn open_existing(root: &Path) -> Option<Self> {
        root.join(STORE_DIR)
            .join(STORE_FILE)
            .is_file()
            .then(|| Self::open(root))
            .flatten()
    }

    /// Write the fingerprints atomically. A failure only costs a re-check
    /// on the next command, so it is logged, not raised.
    pub fn save(&self) {
        let result = (|| -> std::io::Result<()> {
            let dir = self.path.parent().unwrap_or_else(|| Path::new("."));
            let mut file = tempfile::NamedTempFile::new_in(dir)?;
            file.write_all(&serde_json::to_vec_pretty(&self.prints)?)?;
            file.persist(&self.path).map_err(|error| error.error)?;
            Ok(())
        })();
        if let Err(error) = result {
            tracing::debug!(path = %self.path.display(), %error, "autosync fingerprints not saved");
        }
    }
}

#[cfg(unix)]
fn lock(path: &Path) -> Option<fs::File> {
    let file = fs::OpenOptions::new()
        .create(true)
        .truncate(false)
        .write(true)
        .open(path)
        .ok()?;
    rustix::fs::flock(&file, rustix::fs::FlockOperation::LockExclusive).ok()?;
    Some(file)
}

#[cfg(not(unix))]
fn lock(_path: &Path) -> Option<fs::File> {
    None
}
