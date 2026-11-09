//! Pure workspace layout model.
//!
//! This module intentionally has no GPUI, Rio, or terminal dependencies.  It is
//! the small state machine that a view can project onto those systems.

mod projection;
mod store;

pub use projection::{
    LayoutRect, MinSizePolicy, PaneLayout, ProjectionError, ProjectionOptions, project_node,
};
pub use store::{
    BinarySnapshotCodec, CURRENT_SCHEMA_VERSION, FileSnapshotStorage, InMemoryWorkspaceStore,
    PaneMetadata, SessionMetadata, SnapshotCodec, SnapshotStorage, StoreError,
    WorkspacePersistence, WorkspaceSnapshot, WorkspaceStore,
};

mod workspace_model;

pub use workspace_model::{
    Axis, Command, CommandResult, Error, Node, Pane, PaneContent, PaneId, SavedTree, SplitId, Tab,
    TabId, Workspace,
};
