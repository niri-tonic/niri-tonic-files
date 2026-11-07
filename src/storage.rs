//! Instance routing and bounded, worker-only transfers. No registry lock is held
//! while calling a backend. Files are spooled before writes so even two accounts
//! sharing a backend process cannot deadlock on nested read/write requests.
use crate::{
    config::{Config, Paths},
    connector::*,
    local::Local,
    locations,
};
use std::{
    collections::{HashMap, HashSet},
    fs,
    io::{Seek, SeekFrom, Write},
    os::unix::fs::{DirBuilderExt, OpenOptionsExt, PermissionsExt},
    path::PathBuf,
    sync::Arc,
};

pub struct Registry {
    backends: HashMap<String, Arc<dyn Connector>>,
    read_only: HashSet<String>,
    cache: PathBuf,
}
#[derive(PartialEq, Eq)]
struct Fingerprint {
    kind: Kind,
    size: u64,
    modified: Option<u64>,
    revision: Option<String>,
    children: HashSet<Location>,
}
fn error(code: ErrorCode, message: impl Into<String>) -> Error {
    Error::new(code, message)
}
fn none() -> Capabilities {
    Capabilities {
        read: false,
        write: false,
        create: false,
        rename: false,
        copy: false,
        move_item: false,
        trash: false,
        delete: false,
        archive: false,
        watch: false,
        pagination: false,
        atomic_move: false,
    }
}
impl Registry {
    /// Injectable backends for offline tests; IDs must be unique instance IDs.
    pub fn new(
        cache: PathBuf,
        backends: Vec<Arc<dyn Connector>>,
    ) -> std::result::Result<Self, String> {
        let mut map = HashMap::new();
        for backend in backends {
            let id = backend.id().to_owned();
            if (id != "local" && !locations::valid_id(&id))
                || map.insert(id.clone(), backend).is_some()
            {
                return Err(format!("Invalid or duplicate connector ID '{id}'"));
            }
        }
        Ok(Self {
            backends: map,
            read_only: HashSet::new(),
            cache,
        })
    }
    pub fn from_config(config: &Config, paths: &Paths) -> std::result::Result<Self, String> {
        // Validate programmatically constructed configurations as well as YAML.
        Config::parse(&serde_yaml_ng::to_string(config).map_err(|e| e.to_string())?)?;
        let mut backends: Vec<Arc<dyn Connector>> = vec![Arc::new(Local {
            read_only: config.connectors.local.read_only,
        })];
        for instance in &config.connectors.instances {
            if instance.enabled {
                backends.push(Arc::new(crate::remote::RemoteConnector::new(
                    instance, paths,
                )?));
            }
        }
        let mut registry = Self::new(paths.cache.clone(), backends)?;
        registry.read_only.extend(
            config
                .connectors
                .instances
                .iter()
                .filter(|i| i.read_only)
                .map(|i| i.id.clone()),
        );
        Ok(registry)
    }
    fn backend(&self, location: &Location) -> Result<&dyn Connector> {
        locations::path(location)?;
        self.backends
            .get(&location.connector)
            .map(AsRef::as_ref)
            .ok_or_else(|| {
                error(
                    ErrorCode::Unsupported,
                    format!("Unknown or disabled connector '{}'", location.connector),
                )
            })
    }
    pub fn capabilities_at(&self, location: &Location) -> Capabilities {
        let Ok(backend) = self.backend(location) else {
            return none();
        };
        let mut caps = backend.capabilities();
        caps.copy = caps.read;
        if self.read_only.contains(&location.connector) {
            caps.write = false;
            caps.create = false;
            caps.rename = false;
            caps.move_item = false;
            caps.trash = false;
            caps.delete = false;
            caps.archive = false;
        }
        caps
    }
    fn require(
        &self,
        location: &Location,
        allowed: impl FnOnce(Capabilities) -> bool,
    ) -> Result<&dyn Connector> {
        let backend = self.backend(location)?;
        if !allowed(self.capabilities_at(location)) {
            return Err(error(
                ErrorCode::PermissionDenied,
                format!(
                    "Operation is unavailable for connector '{}'",
                    location.connector
                ),
            ));
        }
        Ok(backend)
    }
    fn private_cache(&self, id: &str) -> Result<PathBuf> {
        let dir = self.cache.join("materialized").join(id);
        fs::DirBuilder::new()
            .recursive(true)
            .mode(0o700)
            .create(&dir)
            .map_err(|e| Error::io(&dir, e))?;
        fs::set_permissions(&dir, fs::Permissions::from_mode(0o700))
            .map_err(|e| Error::io(&dir, e))?;
        Ok(dir)
    }
    /// Each download gets a private, exclusive directory, isolated by account ID.
    pub fn materialize(&self, location: &Location, ctx: &Context) -> Result<PathBuf> {
        ctx.check()?;
        self.require(location, |c| c.read)?;
        if location.connector == "local" {
            return crate::local::path(location);
        }
        if self.stat(location, ctx)?.kind != Kind::File {
            return Err(error(
                ErrorCode::Unsupported,
                "Only regular remote files can be opened",
            ));
        }
        let source = locations::path(location)?;
        let name = source
            .file_name()
            .ok_or_else(|| error(ErrorCode::InvalidInput, "File has no name"))?;
        let dir = tempfile::Builder::new()
            .prefix("download-")
            .tempdir_in(self.private_cache(&location.connector)?)
            .map_err(|e| error(ErrorCode::Io, e.to_string()))?;
        fs::set_permissions(dir.path(), fs::Permissions::from_mode(0o700))
            .map_err(|e| Error::io(dir.path(), e))?;
        let output = dir.path().join(name);
        let mut file = fs::OpenOptions::new()
            .write(true)
            .create_new(true)
            .mode(0o600)
            .open(&output)
            .map_err(|e| Error::io(&output, e))?;
        self.stream(location, &mut file, ctx)?;
        file.sync_all().map_err(|e| Error::io(&output, e))?;
        let _ = dir.keep();
        Ok(output)
    }
    fn stream(&self, source: &Location, output: &mut dyn Write, ctx: &Context) -> Result<()> {
        let before = self.stat(source, ctx)?;
        if before.kind != Kind::File {
            return Err(error(
                ErrorCode::Unsupported,
                "Only regular files can be streamed",
            ));
        }
        let mut offset = 0;
        loop {
            ctx.check()?;
            let chunk = self.read(source, offset, 1024 * 1024, ctx)?;
            if chunk.is_empty() {
                break;
            }
            if chunk.len() > 1024 * 1024 {
                return Err(error(ErrorCode::Io, "Backend exceeded bounded read limit"));
            }
            output
                .write_all(&chunk)
                .map_err(|e| error(ErrorCode::Io, e.to_string()))?;
            offset += chunk.len() as u64;
            ctx.progress("Copying", offset, 0, true);
        }
        let after = self.stat(source, ctx)?;
        if offset != before.size
            || after.size != before.size
            || after.modified != before.modified
            || after.revision != before.revision
            || after.kind != Kind::File
        {
            return Err(error(
                ErrorCode::Io,
                "Source changed while copying; source has not been removed",
            ));
        }
        Ok(())
    }
    fn check_transfer(&self, source: &Location, destination: &Location) -> Result<()> {
        self.require(source, |c| c.read)?;
        self.require(destination, |c| c.write && c.create)?;
        let src = locations::path(source)?;
        let dst = locations::path(destination)?;
        if source.connector == destination.connector && dst != src && dst.starts_with(&src) {
            return Err(error(
                ErrorCode::InvalidInput,
                "Cannot copy or move an item into itself",
            ));
        }
        if destination.connector != "local" && src.file_name().is_some_and(|n| n.to_str().is_none())
        {
            return Err(error(
                ErrorCode::InvalidInput,
                "Remote transfers require UTF-8 filenames",
            ));
        }
        Ok(())
    }
    fn tree(
        &self,
        source: &Location,
        destination: &Location,
        ctx: &Context,
        created: &mut bool,
        depth: usize,
    ) -> Result<()> {
        ctx.check()?;
        if depth > 256 {
            return Err(error(
                ErrorCode::InvalidInput,
                "Directory nesting exceeds transfer limit",
            ));
        }
        match self.stat(source, ctx)?.kind {
            Kind::File => {
                // Anonymous temporary files are private, automatically removed,
                // and keep memory bounded independent of the source file size.
                let mut spool =
                    tempfile::tempfile().map_err(|e| error(ErrorCode::Io, e.to_string()))?;
                self.stream(source, &mut spool, ctx)?;
                spool
                    .seek(SeekFrom::Start(0))
                    .map_err(|e| error(ErrorCode::Io, e.to_string()))?;
                ctx.check()?;
                self.write_new(destination, &mut spool, ctx).map_err(|mut e| {
                    if e.code != ErrorCode::AlreadyExists {
                        e.message.push_str("; destination write failed and may have left partial contents. Source has not been removed.");
                    }
                    e
                })?;
                *created = true;
            }
            Kind::Directory => {
                self.create_directory(destination, ctx)?;
                *created = true;
                let mut cursor = None;
                let mut cursors = HashSet::new();
                let mut children = HashSet::new();
                loop {
                    let page = self.list(source, cursor.as_deref(), ctx)?;
                    for entry in page.entries {
                        let child = locations::path(&entry.location)?;
                        if entry.location.connector != source.connector
                            || child.parent() != Some(locations::path(source)?.as_path())
                            || !children.insert(entry.location.clone())
                        {
                            return Err(error(
                                ErrorCode::StaleCursor,
                                "Backend returned duplicate or invalid directory children",
                            ));
                        }
                        let name = locations::transfer_name(&entry)?;
                        if destination.connector != "local" && name.to_str().is_none() {
                            return Err(error(
                                ErrorCode::InvalidInput,
                                "Remote transfers require UTF-8 filenames",
                            ));
                        }
                        let target =
                            locations::location(&locations::path(destination)?.join(&name));
                        self.tree(&entry.location, &target, ctx, created, depth + 1)?;
                    }
                    match page.next {
                        None => break,
                        Some(next) if !next.is_empty() && cursors.insert(next.clone()) => {
                            cursor = Some(next)
                        }
                        _ => {
                            return Err(error(
                                ErrorCode::StaleCursor,
                                "Backend repeated an invalid or stale pagination cursor",
                            ));
                        }
                    }
                }
            }
            Kind::Symlink | Kind::Other => {
                return Err(error(
                    ErrorCode::Unsupported,
                    "Cross-connector symlinks and special files are unsupported; links are never followed",
                ));
            }
        }
        Ok(())
    }
    fn transfer(&self, source: &Location, destination: &Location, ctx: &Context) -> Result<()> {
        self.check_transfer(source, destination)?;
        let mut created = false;
        self.tree(source, destination, ctx, &mut created, 0).map_err(|mut e| {
            if created { e.message.push_str("; partial destination contents have been kept. Source has not been removed."); }
            e
        })
    }
    // A best-effort metadata consistency check, not a transaction against
    // concurrent/adversarial writers: providers expose no conditional delete.
    fn snapshot(&self, source: &Location, ctx: &Context) -> Result<HashMap<Location, Fingerprint>> {
        let mut result = HashMap::new();
        let mut pending = vec![(source.clone(), 0)];
        while let Some((location, depth)) = pending.pop() {
            ctx.check()?;
            if depth > 256 {
                return Err(error(
                    ErrorCode::InvalidInput,
                    "Directory nesting exceeds transfer limit",
                ));
            }
            let entry = self.stat(&location, ctx)?;
            let mut children = HashSet::new();
            if entry.kind == Kind::Directory {
                let parent = locations::path(&location)?;
                let mut cursor = None;
                let mut cursors = HashSet::new();
                loop {
                    let page = self.list(&location, cursor.as_deref(), ctx)?;
                    for child in page.entries {
                        if child.location.connector != location.connector
                            || locations::path(&child.location)?.parent() != Some(parent.as_path())
                            || !children.insert(child.location.clone())
                        {
                            return Err(error(
                                ErrorCode::StaleCursor,
                                "Backend returned duplicate or invalid directory children",
                            ));
                        }
                        pending.push((child.location, depth + 1));
                    }
                    match page.next {
                        None => break,
                        Some(next) if !next.is_empty() && cursors.insert(next.clone()) => {
                            cursor = Some(next)
                        }
                        _ => {
                            return Err(error(
                                ErrorCode::StaleCursor,
                                "Backend repeated an invalid or stale pagination cursor",
                            ));
                        }
                    }
                }
            }
            result.insert(
                location,
                Fingerprint {
                    kind: entry.kind,
                    size: entry.size,
                    modified: entry.modified,
                    revision: entry.revision,
                    children,
                },
            );
        }
        Ok(result)
    }
}

impl Connector for Registry {
    fn id(&self) -> &str {
        "registry"
    }
    fn capabilities(&self) -> Capabilities {
        none()
    }
    fn authentication(&self) -> Authentication {
        Authentication::NotRequired
    }
    fn list(&self, directory: &Location, cursor: Option<&str>, ctx: &Context) -> Result<Page> {
        ctx.check()?;
        let mut page = self
            .require(directory, |c| c.read)?
            .list(directory, cursor, ctx)?;
        let read_only = self.capabilities_at(directory).read_only();
        for entry in &mut page.entries {
            if entry.location.connector != directory.connector {
                return Err(error(
                    ErrorCode::InvalidInput,
                    "Backend returned an entry from another connector",
                ));
            }
            locations::path(&entry.location)?;
            entry.read_only |= read_only;
        }
        Ok(page)
    }
    fn stat(&self, location: &Location, ctx: &Context) -> Result<Entry> {
        ctx.check()?;
        let mut entry = self.require(location, |c| c.read)?.stat(location, ctx)?;
        if entry.location.connector != location.connector
            || locations::path(&entry.location)? != locations::path(location)?
        {
            return Err(error(
                ErrorCode::InvalidInput,
                "Backend returned a mismatched item",
            ));
        }
        entry.read_only |= self.capabilities_at(location).read_only();
        Ok(entry)
    }
    fn read(
        &self,
        location: &Location,
        offset: u64,
        limit: usize,
        ctx: &Context,
    ) -> Result<Vec<u8>> {
        ctx.check()?;
        self.require(location, |c| c.read)?
            .read(location, offset, limit.min(1024 * 1024), ctx)
    }
    fn write_new(
        &self,
        location: &Location,
        input: &mut dyn std::io::Read,
        ctx: &Context,
    ) -> Result<()> {
        ctx.check()?;
        self.require(location, |c| c.write && c.create)?
            .write_new(location, input, ctx)
    }
    fn create_directory(&self, location: &Location, ctx: &Context) -> Result<()> {
        ctx.check()?;
        self.require(location, |c| c.create)?
            .create_directory(location, ctx)
    }
    fn rename(&self, source: &Location, destination: &Location, ctx: &Context) -> Result<()> {
        ctx.check()?;
        self.require(destination, |c| c.create && c.write)?;
        let backend = self.require(source, |c| c.rename)?;
        if source.connector != destination.connector {
            return self.move_item(source, destination, ctx);
        }
        backend.rename(source, destination, ctx)
    }
    fn copy(&self, source: &Location, destination: &Location, ctx: &Context) -> Result<()> {
        ctx.check()?;
        self.check_transfer(source, destination)?;
        if source.connector == destination.connector {
            return self.backend(source)?.copy(source, destination, ctx);
        }
        self.transfer(source, destination, ctx)
    }
    fn copy_unique(
        &self,
        source: &Location,
        destination: &Location,
        ctx: &Context,
    ) -> Result<Location> {
        ctx.check()?;
        self.check_transfer(source, destination)?;
        if source.connector == "local" && destination.connector == "local" {
            return self.backend(source)?.copy_unique(source, destination, ctx);
        }
        let path = locations::path(destination)?;
        let parent = locations::parent(&path)
            .ok_or_else(|| error(ErrorCode::InvalidInput, "Cannot replace connector root"))?;
        let directory = self.stat(source, ctx)?.kind == Kind::Directory;
        let stem = if directory {
            path.file_name()
        } else {
            path.file_stem()
        }
        .unwrap_or_default();
        let extension = if directory { None } else { path.extension() };
        for index in 0..10000 {
            ctx.check()?;
            let candidate = if index == 0 {
                destination.clone()
            } else {
                let mut name = stem.to_os_string();
                name.push(format!("-{index}"));
                if let Some(ext) = extension {
                    name.push(".");
                    name.push(ext);
                }
                locations::location(&parent.join(name))
            };
            match self.stat(&candidate, ctx) {
                Ok(_) => continue,
                Err(e) if e.code == ErrorCode::NotFound => {}
                Err(e) => return Err(e),
            }
            if source.connector == destination.connector {
                // Preserve provider-native types (for example Google Docs).
                // A native recursive copy may partially commit: do not replay it
                // under another name after an ambiguous backend failure.
                self.backend(source)?.copy(source, &candidate, ctx)?;
                return Ok(candidate);
            }
            let mut created = false;
            match self.tree(source, &candidate, ctx, &mut created, 0) {
                Ok(()) => return Ok(candidate),
                Err(e) if e.code == ErrorCode::AlreadyExists && !created => continue,
                Err(mut e) => {
                    if created {
                        e.message.push_str("; partial destination contents have been kept. Source has not been removed.");
                    }
                    return Err(e);
                }
            }
        }
        Err(error(
            ErrorCode::AlreadyExists,
            "No available numbered destination name",
        ))
    }
    fn move_item(&self, source: &Location, destination: &Location, ctx: &Context) -> Result<()> {
        ctx.check()?;
        self.check_transfer(source, destination)?;
        let backend = self.require(source, |c| c.move_item)?;
        if source.connector == destination.connector {
            return backend.move_item(source, destination, ctx);
        }
        self.require(source, |c| c.delete)?;
        let before = self.snapshot(source, ctx)?;
        self.transfer(source, destination, ctx)?;
        let verification = self.snapshot(source, ctx).and_then(|after| {
            if before != after {
                return Err(error(ErrorCode::Io, "Source tree changed during copying"));
            }
            ctx.check()
        });
        verification.map_err(|mut e| {
            e.message.push_str(
                "; copy completed and destination has been kept. Source has not been removed.",
            );
            e
        })?;
        self.delete(source, DeleteMode::Permanent, ctx)
            .map_err(|mut e| {
                e.message.push_str(
                "; copy completed, but source removal did not complete. Destination has been kept.",
            );
                e
            })
    }
    fn delete(&self, location: &Location, mode: DeleteMode, ctx: &Context) -> Result<()> {
        ctx.check()?;
        self.require(location, |c| match mode {
            DeleteMode::Trash => c.trash,
            DeleteMode::Permanent => c.delete,
        })?
        .delete(location, mode, ctx)
    }
    fn archive(&self, location: &Location, ctx: &Context) -> Result<Location> {
        ctx.check()?;
        self.require(location, |c| c.archive && c.write)?
            .archive(location, ctx)
    }
    fn restore(&self, location: &Location, ctx: &Context) -> Result<Location> {
        ctx.check()?;
        self.require(location, |c| c.write && c.create)?
            .restore(location, ctx)
    }
}
