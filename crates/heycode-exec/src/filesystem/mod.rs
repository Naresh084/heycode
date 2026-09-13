//! Replaceable filesystem service and built-in local provider.

mod error;
mod local;
mod model;
mod mutations;
mod observation;
mod plugin;
mod policy;
mod service;
mod text_page;

pub(crate) use error::from_io;
pub use error::{FileSystemError, FileSystemErrorCode};
pub(crate) use local::LocalFileSystemBackend;
pub use model::{
    EditContext, EditFileOutput, EditFileSpec, FileEntryKind, FileMetadata, GlobOutput, GlobSpec,
    GrepContextLine, GrepFileMatch, GrepMatch, GrepOutput, GrepOutputMode, GrepSpec, PathRequest,
    ReadFileOutput, ReadFilePage, ReadFileSpec, ReadFileWindow, ResolvedPath, SearchReport,
    WriteFileSpec,
};
pub use mutations::{CheckedWriteOutput, CheckedWriteSpec, MultiEditOutput, MultiEditSpec};
pub use observation::ObservationLog;
pub(crate) use observation::Stamp;
pub use plugin::local_filesystem_plugin;
pub(crate) use policy::remove_current_components;
pub use policy::{FileSystemPolicy, FileSystemRoot, FileSystemRootAccess};
pub use service::{FileSystemBackend, FileSystemService};
