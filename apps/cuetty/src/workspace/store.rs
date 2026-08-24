//! Durable workspace metadata, deliberately separate from live terminal state.

use super::{Node, PaneId, SavedTree, Workspace, validate_tab};
use std::fmt;
use std::fs::{self, File, OpenOptions};
use std::io::Write;
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicU64, Ordering};

pub const CURRENT_SCHEMA_VERSION: u32 = 1;

const MAGIC: &[u8; 4] = b"CWS1";
const MAX_PAYLOAD: usize = 8 * 1024 * 1024;
const MAX_NODES: usize = 100_000;
const MAX_DEPTH: usize = 128;
const MAX_STRING: usize = 1024 * 1024;
static TEMP_COUNTER: AtomicU64 = AtomicU64::new(0);

#[derive(Clone, Debug, Eq, PartialEq)]
pub struct PaneMetadata {
    pub pane_id: PaneId,
    pub session_id: String,
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub struct SessionMetadata {
    pub session_id: String,
    pub command: String,
    pub working_directory: Option<String>,
}

#[derive(Clone, Debug, PartialEq)]
pub struct WorkspaceSnapshot {
    pub schema_version: u32,
    pub tree: SavedTree<PaneId>,
    pub panes: Vec<PaneMetadata>,
    pub sessions: Vec<SessionMetadata>,
}

impl WorkspaceSnapshot {
    pub fn new(
        tree: SavedTree<PaneId>,
        panes: Vec<PaneMetadata>,
        sessions: Vec<SessionMetadata>,
    ) -> Self {
        Self {
            schema_version: CURRENT_SCHEMA_VERSION,
            tree,
            panes,
            sessions,
        }
    }

    pub fn migrate(mut self) -> Result<Self, StoreError> {
        if self.schema_version > CURRENT_SCHEMA_VERSION {
            return Err(StoreError::UnsupportedSchema {
                version: self.schema_version,
            });
        }
        if self.schema_version == 0 {
            self.schema_version = CURRENT_SCHEMA_VERSION;
        }
        self.validate()?;
        Ok(self)
    }

    pub fn validate(&self) -> Result<(), StoreError> {
        if self.schema_version != CURRENT_SCHEMA_VERSION {
            return Err(StoreError::UnsupportedSchema {
                version: self.schema_version,
            });
        }
        if !validate_tab(&self.tree.tab) {
            return Err(StoreError::InvalidTree);
        }

        let mut ids = Vec::new();
        collect_panes(&self.tree.tab.root, &mut ids);
        for pane_id in &self.panes {
            if !ids.contains(&pane_id.pane_id) {
                return Err(StoreError::OrphanPaneMetadata {
                    pane_id: pane_id.pane_id,
                });
            }
        }
        for pane_id in &ids {
            let matches: Vec<_> = self
                .panes
                .iter()
                .filter(|pane| pane.pane_id == *pane_id)
                .collect();
            match matches.as_slice() {
                [] => return Err(StoreError::MissingPaneMetadata { pane_id: *pane_id }),
                [pane] => {
                    if self
                        .panes
                        .iter()
                        .filter(|other| other.session_id == pane.session_id)
                        .count()
                        != 1
                    {
                        return Err(StoreError::DuplicatePaneSession {
                            session_id: pane.session_id.clone(),
                        });
                    }
                    let sessions: Vec<_> = self
                        .sessions
                        .iter()
                        .filter(|session| session.session_id == pane.session_id)
                        .collect();
                    if sessions.is_empty() {
                        return Err(StoreError::MissingSessionMetadata {
                            session_id: pane.session_id.clone(),
                        });
                    }
                    if sessions.len() != 1 {
                        return Err(StoreError::DuplicateSessionMetadata {
                            session_id: pane.session_id.clone(),
                        });
                    }
                }
                _ => return Err(StoreError::DuplicatePaneMetadata { pane_id: *pane_id }),
            }
        }
        for session in &self.sessions {
            if !self
                .panes
                .iter()
                .any(|pane| pane.session_id == session.session_id)
            {
                return Err(StoreError::OrphanSessionMetadata {
                    session_id: session.session_id.clone(),
                });
            }
        }
        Ok(())
    }

    /// Restore only the pure layout model. A terminal session must be
    /// reconnected by the caller from `SessionMetadata`.
    pub fn restore_into(&self, workspace: &mut Workspace<PaneId>) -> Result<(), StoreError> {
        self.validate()?;
        workspace
            .restore_tree(self.tree.clone())
            .map_err(|_| StoreError::FailedRestore)
    }
}

fn collect_panes(node: &Node<PaneId>, ids: &mut Vec<PaneId>) {
    match node {
        Node::Pane(pane) => ids.push(pane.id),
        Node::Split { children, .. } => children.iter().for_each(|child| collect_panes(child, ids)),
    }
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub enum StoreError {
    UnsupportedSchema { version: u32 },
    CorruptSnapshot,
    IncompleteSnapshot,
    MissingPaneMetadata { pane_id: PaneId },
    MissingSessionMetadata { session_id: String },
    InvalidTree,
    DuplicatePaneMetadata { pane_id: PaneId },
    DuplicatePaneSession { session_id: String },
    DuplicateSessionMetadata { session_id: String },
    OrphanPaneMetadata { pane_id: PaneId },
    OrphanSessionMetadata { session_id: String },
    FailedRestore,
    Codec(String),
    Storage(String),
}
impl fmt::Display for StoreError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "{self:?}")
    }
}
impl std::error::Error for StoreError {}

pub trait SnapshotCodec {
    fn encode(&self, snapshot: &WorkspaceSnapshot) -> Result<Vec<u8>, StoreError>;
    fn decode(&self, payload: &[u8]) -> Result<WorkspaceSnapshot, StoreError>;
}

/// A small, deterministic, length-delimited binary snapshot format.
///
/// The format is intentionally private to this crate: `CWS1`, followed by
/// little-endian integers, recursively encoded layout nodes, and UTF-8
/// strings.  It contains metadata only; live Rio sessions are never encoded.
#[derive(Clone, Copy, Debug, Default)]
pub struct BinarySnapshotCodec;

impl SnapshotCodec for BinarySnapshotCodec {
    fn encode(&self, snapshot: &WorkspaceSnapshot) -> Result<Vec<u8>, StoreError> {
        snapshot.validate()?;
        let mut out = Vec::new();
        out.extend_from_slice(MAGIC);
        put_u32(&mut out, snapshot.schema_version);
        put_tree(&mut out, &snapshot.tree)?;
        put_u32_len(&mut out, snapshot.panes.len())?;
        for pane in &snapshot.panes {
            put_u64(&mut out, pane.pane_id.get());
            put_string(&mut out, &pane.session_id)?;
        }
        put_u32_len(&mut out, snapshot.sessions.len())?;
        for session in &snapshot.sessions {
            put_string(&mut out, &session.session_id)?;
            put_string(&mut out, &session.command)?;
            match &session.working_directory {
                Some(path) => {
                    out.push(1);
                    put_string(&mut out, path)?;
                }
                None => out.push(0),
            }
        }
        if out.len() > MAX_PAYLOAD {
            return Err(StoreError::Codec("snapshot too large".into()));
        }
        Ok(out)
    }

    fn decode(&self, payload: &[u8]) -> Result<WorkspaceSnapshot, StoreError> {
        if payload.len() > MAX_PAYLOAD {
            return Err(StoreError::Codec("snapshot too large".into()));
        }
        let mut r = Reader {
            bytes: payload,
            pos: 0,
            nodes: 0,
        };
        if r.take(4)? != MAGIC {
            return Err(StoreError::Codec("invalid magic".into()));
        }
        let schema_version = r.u32()?;
        let tree = read_tree(&mut r, 0)?;
        let pane_count = r.count()?;
        let mut panes = Vec::with_capacity(pane_count);
        for _ in 0..pane_count {
            panes.push(PaneMetadata {
                pane_id: PaneId::new(r.u64()?),
                session_id: r.string()?,
            });
        }
        let session_count = r.count()?;
        let mut sessions = Vec::with_capacity(session_count);
        for _ in 0..session_count {
            let session_id = r.string()?;
            let command = r.string()?;
            let working_directory = match r.byte()? {
                0 => None,
                1 => Some(r.string()?),
                _ => return Err(StoreError::Codec("unknown optional string tag".into())),
            };
            sessions.push(SessionMetadata {
                session_id,
                command,
                working_directory,
            });
        }
        if r.pos != payload.len() {
            return Err(StoreError::Codec("trailing bytes".into()));
        }
        WorkspaceSnapshot {
            schema_version,
            tree,
            panes,
            sessions,
        }
        .migrate()
    }
}

fn put_u32(out: &mut Vec<u8>, value: u32) {
    out.extend_from_slice(&value.to_le_bytes());
}
fn put_u64(out: &mut Vec<u8>, value: u64) {
    out.extend_from_slice(&value.to_le_bytes());
}
fn put_u32_len(out: &mut Vec<u8>, len: usize) -> Result<(), StoreError> {
    let len = u32::try_from(len).map_err(|_| StoreError::Codec("length overflow".into()))?;
    put_u32(out, len);
    Ok(())
}
fn put_string(out: &mut Vec<u8>, value: &str) -> Result<(), StoreError> {
    if value.len() > MAX_STRING {
        return Err(StoreError::Codec("string too large".into()));
    }
    put_u32_len(out, value.len())?;
    out.extend_from_slice(value.as_bytes());
    Ok(())
}
fn put_tree(out: &mut Vec<u8>, tree: &SavedTree<PaneId>) -> Result<(), StoreError> {
    put_u64(out, tree.tab.id.get());
    put_string(out, &tree.tab.title)?;
    put_node(out, &tree.tab.root, 0)?;
    put_u64(out, tree.tab.focused.get());
    Ok(())
}
fn put_node(out: &mut Vec<u8>, node: &Node<PaneId>, depth: usize) -> Result<(), StoreError> {
    if depth > MAX_DEPTH {
        return Err(StoreError::Codec("tree too deep".into()));
    }
    match node {
        Node::Pane(pane) => {
            out.push(0);
            put_u64(out, pane.id.get());
            put_u64(out, pane.content.get());
        }
        Node::Split {
            id,
            axis,
            weights,
            children,
        } => {
            out.push(1);
            put_u64(out, id.get());
            out.push(match axis {
                super::Axis::Horizontal => 0,
                super::Axis::Vertical => 1,
            });
            put_u32_len(out, weights.len())?;
            for weight in weights {
                if !weight.is_finite() || *weight <= 0.0 {
                    return Err(StoreError::Codec("invalid weight".into()));
                }
                out.extend_from_slice(&weight.to_le_bytes());
            }
            put_u32_len(out, children.len())?;
            for child in children {
                put_node(out, child, depth + 1)?;
            }
        }
    }
    Ok(())
}

struct Reader<'a> {
    bytes: &'a [u8],
    pos: usize,
    nodes: usize,
}
impl<'a> Reader<'a> {
    fn take(&mut self, n: usize) -> Result<&'a [u8], StoreError> {
        let end = self
            .pos
            .checked_add(n)
            .ok_or_else(|| StoreError::Codec("length overflow".into()))?;
        if end > self.bytes.len() {
            return Err(StoreError::IncompleteSnapshot);
        }
        let result = &self.bytes[self.pos..end];
        self.pos = end;
        Ok(result)
    }
    fn byte(&mut self) -> Result<u8, StoreError> {
        Ok(self.take(1)?[0])
    }
    fn u32(&mut self) -> Result<u32, StoreError> {
        Ok(u32::from_le_bytes(self.take(4)?.try_into().unwrap()))
    }
    fn u64(&mut self) -> Result<u64, StoreError> {
        Ok(u64::from_le_bytes(self.take(8)?.try_into().unwrap()))
    }
    fn count(&mut self) -> Result<usize, StoreError> {
        let n = self.u32()? as usize;
        if n > MAX_NODES {
            return Err(StoreError::Codec("count too large".into()));
        }
        Ok(n)
    }
    fn string(&mut self) -> Result<String, StoreError> {
        let n = self.u32()? as usize;
        if n > MAX_STRING {
            return Err(StoreError::Codec("string too large".into()));
        }
        String::from_utf8(self.take(n)?.to_vec())
            .map_err(|_| StoreError::Codec("invalid utf-8".into()))
    }
}
fn read_tree(r: &mut Reader<'_>, depth: usize) -> Result<SavedTree<PaneId>, StoreError> {
    let id = super::TabId::new(r.u64()?);
    let title = r.string()?;
    let root = read_node(r, depth)?;
    let focused = PaneId::new(r.u64()?);
    Ok(SavedTree {
        tab: super::Tab {
            id,
            title,
            root,
            focused,
        },
    })
}
fn read_node(r: &mut Reader<'_>, depth: usize) -> Result<Node<PaneId>, StoreError> {
    if depth > MAX_DEPTH {
        return Err(StoreError::Codec("tree too deep".into()));
    }
    r.nodes += 1;
    if r.nodes > MAX_NODES {
        return Err(StoreError::Codec("node count too large".into()));
    }
    match r.byte()? {
        0 => {
            let id = PaneId::new(r.u64()?);
            let content = PaneId::new(r.u64()?);
            Ok(Node::Pane(super::Pane { id, content }))
        }
        1 => {
            let id = super::SplitId::new(r.u64()?);
            let axis = match r.byte()? {
                0 => super::Axis::Horizontal,
                1 => super::Axis::Vertical,
                _ => return Err(StoreError::Codec("unknown axis".into())),
            };
            let weight_count = r.count()?;
            let mut weights = Vec::with_capacity(weight_count);
            for _ in 0..weight_count {
                let bytes: [u8; 4] = r.take(4)?.try_into().unwrap();
                let weight = f32::from_le_bytes(bytes);
                if !weight.is_finite() || weight <= 0.0 {
                    return Err(StoreError::Codec("invalid weight".into()));
                }
                weights.push(weight);
            }
            let child_count = r.count()?;
            let mut children = Vec::with_capacity(child_count);
            for _ in 0..child_count {
                children.push(read_node(r, depth + 1)?);
            }
            Ok(Node::Split {
                id,
                axis,
                weights,
                children,
            })
        }
        _ => Err(StoreError::Codec("unknown node tag".into())),
    }
}

/// Atomic filesystem payload storage. The path is assumed to have been
/// authorized by the host capability layer; this type performs no path grant.
#[derive(Clone, Debug)]
pub struct FileSnapshotStorage {
    path: PathBuf,
}
impl FileSnapshotStorage {
    pub fn new(path: impl Into<PathBuf>) -> Self {
        Self { path: path.into() }
    }
    pub fn path(&self) -> &Path {
        &self.path
    }
}
impl SnapshotStorage for FileSnapshotStorage {
    fn read(&self) -> Result<Option<Vec<u8>>, StoreError> {
        match fs::read(&self.path) {
            Ok(bytes) => Ok(Some(bytes)),
            Err(error) if error.kind() == std::io::ErrorKind::NotFound => Ok(None),
            Err(error) => Err(StoreError::Storage(error.to_string())),
        }
    }
    fn replace(&mut self, payload: Vec<u8>) -> Result<(), StoreError> {
        if payload.len() > MAX_PAYLOAD {
            return Err(StoreError::Storage("payload too large".into()));
        }
        let parent = self.path.parent().unwrap_or_else(|| Path::new("."));
        let name = self
            .path
            .file_name()
            .ok_or_else(|| StoreError::Storage("path has no filename".into()))?
            .to_string_lossy();
        let suffix = TEMP_COUNTER.fetch_add(1, Ordering::Relaxed);
        let temp = parent.join(format!(".{name}.tmp-{}-{suffix}", std::process::id()));
        let result = (|| {
            let mut file = OpenOptions::new()
                .write(true)
                .create_new(true)
                .open(&temp)
                .map_err(|e| StoreError::Storage(e.to_string()))?;
            file.write_all(&payload)
                .and_then(|_| file.sync_all())
                .map_err(|e| StoreError::Storage(e.to_string()))?;
            fs::rename(&temp, &self.path).map_err(|e| StoreError::Storage(e.to_string()))?;
            if let Ok(dir) = File::open(parent) {
                let _ = dir.sync_all();
            }
            Ok(())
        })();
        if result.is_err() {
            let _ = fs::remove_file(&temp);
        }
        result
    }
}

pub trait SnapshotStorage {
    fn read(&self) -> Result<Option<Vec<u8>>, StoreError>;
    /// Replace the complete payload atomically; errors must preserve the old value.
    fn replace(&mut self, payload: Vec<u8>) -> Result<(), StoreError>;
}

pub trait WorkspaceStore {
    fn save(&mut self, snapshot: &WorkspaceSnapshot) -> Result<(), StoreError>;
    fn load(&self) -> Result<Option<WorkspaceSnapshot>, StoreError>;
}

pub struct WorkspacePersistence<C, S> {
    codec: C,
    storage: S,
}
impl<C, S> WorkspacePersistence<C, S> {
    pub fn new(codec: C, storage: S) -> Self {
        Self { codec, storage }
    }
    pub fn into_parts(self) -> (C, S) {
        (self.codec, self.storage)
    }
}
impl<C: SnapshotCodec, S: SnapshotStorage> WorkspaceStore for WorkspacePersistence<C, S> {
    fn save(&mut self, snapshot: &WorkspaceSnapshot) -> Result<(), StoreError> {
        snapshot.validate()?;
        let payload = self.codec.encode(snapshot)?;
        self.storage.replace(payload)
    }
    fn load(&self) -> Result<Option<WorkspaceSnapshot>, StoreError> {
        let Some(payload) = self.storage.read()? else {
            return Ok(None);
        };
        let snapshot = self
            .codec
            .decode(&payload)
            .map_err(|_| StoreError::CorruptSnapshot)?;
        snapshot.migrate().map(Some).map_err(|error| match error {
            StoreError::UnsupportedSchema { version } => StoreError::UnsupportedSchema { version },
            _ => StoreError::IncompleteSnapshot,
        })
    }
}

#[derive(Clone, Debug, Default)]
pub struct InMemoryWorkspaceStore {
    snapshot: Option<WorkspaceSnapshot>,
}
impl InMemoryWorkspaceStore {
    pub fn snapshot(&self) -> Option<&WorkspaceSnapshot> {
        self.snapshot.as_ref()
    }
}
impl WorkspaceStore for InMemoryWorkspaceStore {
    fn save(&mut self, snapshot: &WorkspaceSnapshot) -> Result<(), StoreError> {
        snapshot.validate()?;
        self.snapshot = Some(snapshot.clone());
        Ok(())
    }
    fn load(&self) -> Result<Option<WorkspaceSnapshot>, StoreError> {
        self.snapshot.clone().map(|s| s.migrate()).transpose()
    }
}

#[cfg(test)]
mod tests {
    use super::super::{Axis, Node, Pane, SplitId, Tab, TabId};
    use super::*;
    use std::fs;
    use std::time::{SystemTime, UNIX_EPOCH};

    fn snapshot() -> WorkspaceSnapshot {
        let pane = PaneId::new(1);
        WorkspaceSnapshot::new(
            SavedTree {
                tab: Tab {
                    id: TabId::new(1),
                    title: "main".into(),
                    root: Node::Pane(Pane {
                        id: pane,
                        content: pane,
                    }),
                    focused: pane,
                },
            },
            vec![PaneMetadata {
                pane_id: pane,
                session_id: "s1".into(),
            }],
            vec![SessionMetadata {
                session_id: "s1".into(),
                command: "shell".into(),
                working_directory: None,
            }],
        )
    }

    fn temp_path(label: &str) -> std::path::PathBuf {
        let stamp = SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .unwrap()
            .as_nanos();
        std::env::temp_dir().join(format!("cuetty-{label}-{}-{stamp}", std::process::id()))
    }

    #[test]
    fn binary_codec_round_trips_and_is_deterministic() {
        let codec = BinarySnapshotCodec;
        let first = codec.encode(&snapshot()).unwrap();
        assert_eq!(first, codec.encode(&snapshot()).unwrap());
        assert_eq!(codec.decode(&first).unwrap(), snapshot());
    }

    #[test]
    fn binary_codec_rejects_truncated_unknown_trailing_and_invalid_utf8() {
        let codec = BinarySnapshotCodec;
        let payload = codec.encode(&snapshot()).unwrap();
        assert!(codec.decode(&payload[..payload.len() - 1]).is_err());
        assert!(codec.decode(b"XWS1").is_err());
        let mut trailing = payload.clone();
        trailing.push(0);
        assert!(codec.decode(&trailing).is_err());
        let mut invalid = payload;
        // The title is the first string after the tab id; replace its first byte.
        invalid[20] = 0xff;
        assert!(codec.decode(&invalid).is_err());
        let mut unknown_tag = codec.encode(&snapshot()).unwrap();
        // magic, schema, tab id, title length, and the four title bytes.
        unknown_tag[24] = 7;
        assert!(codec.decode(&unknown_tag).is_err());
    }

    #[test]
    fn binary_codec_rejects_oversized_payload() {
        let codec = BinarySnapshotCodec;
        assert!(codec.decode(&vec![0; MAX_PAYLOAD + 1]).is_err());
    }

    #[test]
    fn file_storage_reads_missing_and_atomically_replaces() {
        let directory = temp_path("storage");
        fs::create_dir_all(&directory).unwrap();
        let path = directory.join("snapshot.bin");
        let mut storage = FileSnapshotStorage::new(&path);
        assert_eq!(storage.read().unwrap(), None);
        storage.replace(vec![1, 2, 3]).unwrap();
        assert_eq!(storage.read().unwrap(), Some(vec![1, 2, 3]));
        storage.replace(vec![4, 5]).unwrap();
        assert_eq!(storage.read().unwrap(), Some(vec![4, 5]));
        fs::remove_dir_all(directory).unwrap();
    }

    #[test]
    fn file_storage_failed_replace_preserves_previous_payload() {
        let directory = temp_path("failed");
        fs::create_dir_all(&directory).unwrap();
        let path = directory.join("snapshot.bin");
        let mut storage = FileSnapshotStorage::new(&path);
        storage.replace(vec![9, 8, 7]).unwrap();
        assert!(storage.replace(vec![0; MAX_PAYLOAD + 1]).is_err());
        assert_eq!(storage.read().unwrap(), Some(vec![9, 8, 7]));
        fs::remove_dir_all(directory).unwrap();
    }
    #[derive(Default)]
    struct Codec;
    impl SnapshotCodec for Codec {
        fn encode(&self, s: &WorkspaceSnapshot) -> Result<Vec<u8>, StoreError> {
            Ok(vec![s.schema_version as u8])
        }
        fn decode(&self, p: &[u8]) -> Result<WorkspaceSnapshot, StoreError> {
            if p.len() != 1 || p[0] == 255 {
                return Err(StoreError::CorruptSnapshot);
            }
            let mut s = snapshot();
            s.schema_version = p[0] as u32;
            Ok(s)
        }
    }
    #[derive(Default)]
    struct Storage {
        value: Option<Vec<u8>>,
        fail: bool,
    }
    impl SnapshotStorage for Storage {
        fn read(&self) -> Result<Option<Vec<u8>>, StoreError> {
            Ok(self.value.clone())
        }
        fn replace(&mut self, p: Vec<u8>) -> Result<(), StoreError> {
            if self.fail {
                return Err(StoreError::Storage("replace failed".into()));
            }
            self.value = Some(p);
            Ok(())
        }
    }
    #[test]
    fn save_load_round_trip() {
        let mut s = WorkspacePersistence::new(Codec, Storage::default());
        let expected = snapshot();
        s.save(&expected).unwrap();
        assert_eq!(s.load().unwrap(), Some(expected));
    }
    #[test]
    fn unsupported_version_is_rejected() {
        let s = WorkspacePersistence::new(
            Codec,
            Storage {
                value: Some(vec![9]),
                fail: false,
            },
        );
        assert!(matches!(
            s.load(),
            Err(StoreError::UnsupportedSchema { version: 9 })
        ));
    }
    #[test]
    fn corrupt_payload_is_rejected() {
        let s = WorkspacePersistence::new(
            Codec,
            Storage {
                value: Some(vec![255, 1]),
                fail: false,
            },
        );
        assert_eq!(s.load(), Err(StoreError::CorruptSnapshot));
    }
    #[test]
    fn failed_replace_keeps_previous_value() {
        let mut s = Storage {
            value: Some(vec![1]),
            fail: true,
        };
        assert!(s.replace(vec![2]).is_err());
        assert_eq!(s.value, Some(vec![1]));
    }
    #[test]
    fn migration_is_deterministic_and_current_is_noop() {
        let value = snapshot();
        assert_eq!(value.clone().migrate().unwrap(), value);
        let mut old = value.clone();
        old.schema_version = 0;
        assert_eq!(old.migrate().unwrap(), value);
    }
    #[test]
    fn missing_metadata_is_explicit() {
        let mut s = snapshot();
        s.panes.clear();
        assert!(matches!(
            s.validate(),
            Err(StoreError::MissingPaneMetadata { .. })
        ));
        let mut s = snapshot();
        s.sessions.clear();
        assert!(matches!(
            s.validate(),
            Err(StoreError::MissingSessionMetadata { .. })
        ));
    }
    #[test]
    fn duplicate_and_orphan_snapshot_metadata_are_rejected() {
        let mut s = snapshot();
        s.panes.push(s.panes[0].clone());
        assert!(matches!(
            s.validate(),
            Err(StoreError::DuplicatePaneMetadata { .. })
        ));

        let mut s = snapshot();
        s.panes.push(PaneMetadata {
            pane_id: PaneId::new(2),
            session_id: "s2".into(),
        });
        assert!(matches!(
            s.validate(),
            Err(StoreError::OrphanPaneMetadata { .. })
        ));

        let mut s = snapshot();
        s.sessions.push(SessionMetadata {
            session_id: "s2".into(),
            command: "shell".into(),
            working_directory: None,
        });
        assert!(matches!(
            s.validate(),
            Err(StoreError::OrphanSessionMetadata { .. })
        ));

        let mut s = snapshot();
        s.sessions.push(s.sessions[0].clone());
        assert!(matches!(
            s.validate(),
            Err(StoreError::DuplicateSessionMetadata { .. })
        ));
    }
    #[test]
    fn invalid_tree_and_ambiguous_pane_session_are_rejected() {
        let mut s = snapshot();
        s.tree.tab.focused = PaneId::new(2);
        assert_eq!(s.validate(), Err(StoreError::InvalidTree));

        let mut s = snapshot();
        s.tree.tab.root = Node::Split {
            id: SplitId::new(1),
            axis: Axis::Horizontal,
            weights: vec![0.5, 0.5],
            children: vec![
                Node::Pane(Pane {
                    id: PaneId::new(1),
                    content: PaneId::new(1),
                }),
                Node::Pane(Pane {
                    id: PaneId::new(2),
                    content: PaneId::new(2),
                }),
            ],
        };
        s.panes.push(PaneMetadata {
            pane_id: PaneId::new(2),
            session_id: "s1".into(),
        });
        assert!(matches!(
            s.validate(),
            Err(StoreError::DuplicatePaneSession { .. })
        ));
    }
}
