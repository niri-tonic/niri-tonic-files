//! Blocking, object-safe connector contract. Invoke only on worker threads.
//! IDs are opaque (local IDs are byte-preserving file URIs), not display names.
use serde::{Deserialize, Serialize};
use std::{
    io::Read,
    sync::{
        Arc,
        atomic::{AtomicBool, Ordering},
    },
};

pub const PROTOCOL_VERSION: u32 = 1;

#[derive(Clone, Debug, PartialEq, Eq, Hash, Serialize, Deserialize)]
pub struct Location {
    pub connector: String,
    pub id: String,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum Kind {
    File,
    Directory,
    Symlink,
    Other,
}

#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct Entry {
    pub location: Location,
    pub name: String,
    pub kind: Kind,
    /// True for a directory or a symlink whose target is a directory.
    pub navigable: bool,
    pub hidden: bool,
    pub size: u64,
    pub modified: Option<u64>,
    /// Provider version/identity token, when available, for transfer consistency.
    #[serde(default)]
    pub revision: Option<String>,
    /// Portable name for new copies when the path contains a provider identity
    /// alias. None preserves the original (possibly non-UTF-8 local) basename.
    #[serde(default)]
    pub transfer_name: Option<String>,
    pub read_only: bool,
}

#[derive(Clone, Copy, Debug, Serialize, Deserialize)]
pub struct Capabilities {
    pub read: bool,
    pub write: bool,
    pub create: bool,
    pub rename: bool,
    pub copy: bool,
    pub move_item: bool,
    pub trash: bool,
    pub delete: bool,
    pub archive: bool,
    pub watch: bool,
    pub pagination: bool,
    pub atomic_move: bool,
}
impl Capabilities {
    pub fn read_only(&self) -> bool {
        !self.write && !self.create && !self.delete
    }
}

#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct Page {
    pub entries: Vec<Entry>,
    pub next: Option<String>,
}

#[derive(Clone, Copy, Debug, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
pub enum ErrorCode {
    NotFound,
    PermissionDenied,
    AlreadyExists,
    Cancelled,
    Unsupported,
    InvalidInput,
    AuthenticationRequired,
    StaleCursor,
    Io,
}

#[derive(Clone, Debug, thiserror::Error, Serialize, Deserialize)]
#[error("{message}")]
pub struct Error {
    pub code: ErrorCode,
    pub message: String,
    pub retryable: bool,
}
pub type Result<T> = std::result::Result<T, Error>;
impl Error {
    pub fn new(code: ErrorCode, message: impl Into<String>) -> Self {
        Self {
            code,
            message: message.into(),
            retryable: false,
        }
    }
    pub fn io(path: &std::path::Path, e: std::io::Error) -> Self {
        let code = match e.kind() {
            std::io::ErrorKind::NotFound => ErrorCode::NotFound,
            std::io::ErrorKind::PermissionDenied => ErrorCode::PermissionDenied,
            std::io::ErrorKind::AlreadyExists => ErrorCode::AlreadyExists,
            _ => ErrorCode::Io,
        };
        Self::new(code, format!("{}: {e}", path.display()))
    }
}

#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct Progress {
    pub phase: String,
    pub bytes: u64,
    pub items: u64,
    pub cancellable: bool,
}

#[derive(Clone)]
pub struct Context {
    pub cancelled: Arc<AtomicBool>,
    report: Arc<dyn Fn(Progress) + Send + Sync>,
}
impl Default for Context {
    fn default() -> Self {
        Self::new(|_| {})
    }
}
impl Context {
    pub fn new(report: impl Fn(Progress) + Send + Sync + 'static) -> Self {
        Self {
            cancelled: Arc::new(AtomicBool::new(false)),
            report: Arc::new(report),
        }
    }
    pub fn cancel(&self) {
        self.cancelled.store(true, Ordering::Relaxed);
    }
    pub fn check(&self) -> Result<()> {
        if self.cancelled.load(Ordering::Relaxed) {
            Err(Error::new(
                ErrorCode::Cancelled,
                "Cancelled. Completed items have been kept.",
            ))
        } else {
            Ok(())
        }
    }
    pub fn progress(&self, phase: &str, bytes: u64, items: u64, cancellable: bool) {
        (self.report)(Progress {
            phase: phase.into(),
            bytes,
            items,
            cancellable,
        });
    }
}

#[derive(Clone, Debug, Serialize, Deserialize)]
#[serde(tag = "state", rename_all = "snake_case")]
pub enum Authentication {
    NotRequired,
    Required { method: String },
    Ready,
    Expired,
}

#[derive(Clone, Copy, Debug, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum DeleteMode {
    Trash,
    Permanent,
}

pub trait Connector: Send + Sync {
    fn id(&self) -> &str;
    fn capabilities(&self) -> Capabilities;
    fn authentication(&self) -> Authentication;
    /// References to Secret Service entries only; never plaintext credentials.
    fn authenticate(&self, _credential_ref: &str, _ctx: &Context) -> Result<()> {
        Err(Error::new(
            ErrorCode::Unsupported,
            "This connector does not support authentication",
        ))
    }
    fn list(&self, directory: &Location, cursor: Option<&str>, ctx: &Context) -> Result<Page>;
    fn stat(&self, location: &Location, ctx: &Context) -> Result<Entry>;
    /// Reads at most one MiB. Call repeatedly for larger streams.
    fn read(
        &self,
        location: &Location,
        offset: u64,
        limit: usize,
        ctx: &Context,
    ) -> Result<Vec<u8>>;
    /// Exclusive creation. Replacing existing data is deliberately not implicit.
    fn write_new(&self, location: &Location, input: &mut dyn Read, ctx: &Context) -> Result<()>;
    fn create_directory(&self, location: &Location, ctx: &Context) -> Result<()>;
    fn rename(&self, source: &Location, destination: &Location, ctx: &Context) -> Result<()>;
    fn copy(&self, source: &Location, destination: &Location, ctx: &Context) -> Result<()>;
    /// Exclusive copy, returning the final location. Local storage automatically
    /// chooses a numbered name on collision; other providers may reject it.
    fn copy_unique(
        &self,
        source: &Location,
        destination: &Location,
        ctx: &Context,
    ) -> Result<Location> {
        self.copy(source, destination, ctx)?;
        Ok(destination.clone())
    }
    fn move_item(&self, source: &Location, destination: &Location, ctx: &Context) -> Result<()>;
    fn delete(&self, location: &Location, mode: DeleteMode, ctx: &Context) -> Result<()>;
    fn archive(&self, location: &Location, ctx: &Context) -> Result<Location>;
    fn restore(&self, location: &Location, ctx: &Context) -> Result<Location>;
}

/// Negotiated before any external plugin requests. No Rust ABI crosses the boundary.
#[derive(Clone, Debug, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Handshake {
    pub protocol: u32,
    pub connector: String,
    pub capabilities: Capabilities,
}
impl Handshake {
    pub fn validate(&self) -> Result<()> {
        if self.protocol != PROTOCOL_VERSION
            || self.connector.is_empty()
            || self.connector == "local"
        {
            Err(Error::new(
                ErrorCode::Unsupported,
                "Incompatible plugin handshake (expected protocol 1 and a non-local connector ID)",
            ))
        } else {
            Ok(())
        }
    }
}
