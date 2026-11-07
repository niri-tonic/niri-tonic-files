use crate::{
    config::{Sort, View},
    connector::*,
};
use gtk::{gio, prelude::*};
use rustix::fs::{CWD, RenameFlags, renameat_with};
use std::{
    ffi::OsString,
    fs::{self, File, OpenOptions},
    io::{Read, Seek, SeekFrom, Write},
    os::unix::{
        ffi::OsStrExt,
        fs::{MetadataExt, OpenOptionsExt, PermissionsExt, symlink},
    },
    path::{Path, PathBuf},
    time::UNIX_EPOCH,
};

#[derive(Clone, Default)]
pub struct Local {
    pub read_only: bool,
}

pub fn location(path: &Path) -> Location {
    Location {
        connector: "local".into(),
        id: gio::File::for_path(path).uri().to_string(),
    }
}
pub fn path(location: &Location) -> Result<PathBuf> {
    if location.connector != "local" || !location.id.starts_with("file:///") {
        return Err(Error::new(
            ErrorCode::InvalidInput,
            "Expected a local file URI",
        ));
    }
    gio::File::for_uri(&location.id)
        .path()
        .ok_or_else(|| Error::new(ErrorCode::InvalidInput, "Invalid local file URI"))
}
pub fn valid_name(name: &str) -> Result<()> {
    if name.is_empty() || name == "." || name == ".." || name.contains(['/', '\0']) {
        Err(Error::new(
            ErrorCode::InvalidInput,
            "Enter a single name, without '/' or NUL",
        ))
    } else {
        Ok(())
    }
}

pub fn sort(entries: &mut [Entry], view: &View) {
    entries.sort_by(|a, b| {
        let group = if view.folders_first {
            b.navigable.cmp(&a.navigable)
        } else {
            std::cmp::Ordering::Equal
        };
        group.then_with(|| {
            let order = match view.sort {
                Sort::Name => a.name.to_lowercase().cmp(&b.name.to_lowercase()),
                Sort::Modified => a.modified.cmp(&b.modified),
                Sort::Size => a.size.cmp(&b.size),
            }
            .then_with(|| a.location.id.cmp(&b.location.id));
            if view.descending {
                order.reverse()
            } else {
                order
            }
        })
    });
}

fn metadata(path: &Path) -> Result<Entry> {
    let meta = fs::symlink_metadata(path).map_err(|e| Error::io(path, e))?;
    let kind = if meta.is_symlink() {
        Kind::Symlink
    } else if meta.is_dir() {
        Kind::Directory
    } else if meta.is_file() {
        Kind::File
    } else {
        Kind::Other
    };
    let name = path.file_name().unwrap_or(path.as_os_str());
    Ok(Entry {
        location: location(path),
        name: name.to_string_lossy().into_owned(),
        kind,
        navigable: meta.is_dir()
            || (meta.is_symlink() && fs::metadata(path).is_ok_and(|m| m.is_dir())),
        hidden: name.as_bytes().first() == Some(&b'.'),
        size: meta.len(),
        modified: meta
            .modified()
            .ok()
            .and_then(|t| t.duration_since(UNIX_EPOCH).ok())
            .map(|d| d.as_secs()),
        read_only: meta.permissions().readonly(),
        transfer_name: None,
        revision: Some(format!(
            "{}:{}:{}:{}:{}:{}",
            meta.dev(),
            meta.ino(),
            meta.mtime(),
            meta.mtime_nsec(),
            meta.ctime(),
            meta.ctime_nsec()
        )),
    })
}

fn rename_exclusive(src: &Path, dest: &Path) -> Result<()> {
    renameat_with(CWD, src, CWD, dest, RenameFlags::NOREPLACE)
        .map_err(|e| Error::io(dest, e.into()))
}

fn ensure_absent(path: &Path) -> Result<()> {
    match fs::symlink_metadata(path) {
        Ok(_) => Err(Error::new(
            ErrorCode::AlreadyExists,
            format!("{} already exists. Nothing was replaced.", path.display()),
        )),
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => Ok(()),
        Err(e) => Err(Error::io(path, e)),
    }
}

fn parent(path: &Path) -> Result<&Path> {
    path.parent()
        .filter(|p| !p.as_os_str().is_empty())
        .ok_or_else(|| {
            Error::new(
                ErrorCode::InvalidInput,
                "The filesystem root cannot be changed",
            )
        })
}

fn prevent_recursion(src: &Path, dest: &Path) -> Result<()> {
    if fs::symlink_metadata(src)
        .map_err(|e| Error::io(src, e))?
        .is_dir()
    {
        let src = fs::canonicalize(src).map_err(|e| Error::io(src, e))?;
        let destination_parent = fs::canonicalize(parent(dest)?).map_err(|e| Error::io(dest, e))?;
        if destination_parent.starts_with(src) {
            return Err(Error::new(
                ErrorCode::InvalidInput,
                "A folder cannot be copied or moved inside itself",
            ));
        }
    }
    Ok(())
}

fn copy_stream(
    input: &mut dyn Read,
    output: &mut File,
    ctx: &Context,
    bytes: &mut u64,
) -> Result<()> {
    let mut buffer = vec![0u8; 1024 * 1024];
    loop {
        ctx.check()?;
        let n = input
            .read(&mut buffer)
            .map_err(|e| Error::new(ErrorCode::Io, format!("Reading source: {e}")))?;
        if n == 0 {
            break;
        }
        output
            .write_all(&buffer[..n])
            .map_err(|e| Error::new(ErrorCode::Io, format!("Writing destination: {e}")))?;
        *bytes += n as u64;
        ctx.progress("Copying", *bytes, 0, true);
    }
    output
        .sync_all()
        .map_err(|e| Error::new(ErrorCode::Io, e.to_string()))
}

fn copy_tree(src: &Path, dest: &Path, ctx: &Context, bytes: &mut u64) -> Result<()> {
    ctx.check()?;
    let meta = fs::symlink_metadata(src).map_err(|e| Error::io(src, e))?;
    if meta.is_symlink() {
        let target = fs::read_link(src).map_err(|e| Error::io(src, e))?;
        symlink(target, dest).map_err(|e| Error::io(dest, e))?;
    } else if meta.is_dir() {
        fs::create_dir(dest).map_err(|e| Error::io(dest, e))?;
        for child in fs::read_dir(src).map_err(|e| Error::io(src, e))? {
            let child = child.map_err(|e| Error::io(src, e))?;
            copy_tree(&child.path(), &dest.join(child.file_name()), ctx, bytes)?;
        }
        // Keep ordinary permission bits, never copy setuid/setgid bits.
        fs::set_permissions(
            dest,
            fs::Permissions::from_mode(meta.permissions().mode() & 0o777),
        )
        .map_err(|e| Error::io(dest, e))?;
    } else if meta.is_file() {
        // O_NOFOLLOW avoids copying an unexpected symlink after a concurrent replacement.
        let mut input = OpenOptions::new()
            .read(true)
            .custom_flags(
                (rustix::fs::OFlags::NOFOLLOW | rustix::fs::OFlags::NONBLOCK).bits() as i32,
            )
            .open(src)
            .map_err(|e| Error::io(src, e))?;
        if !input.metadata().map_err(|e| Error::io(src, e))?.is_file() {
            return Err(Error::new(
                ErrorCode::Io,
                "Source was replaced by a non-regular file",
            ));
        }
        let mut output = OpenOptions::new()
            .write(true)
            .create_new(true)
            .open(dest)
            .map_err(|e| Error::io(dest, e))?;
        copy_stream(&mut input, &mut output, ctx, bytes)?;
        fs::set_permissions(
            dest,
            fs::Permissions::from_mode(meta.permissions().mode() & 0o777),
        )
        .map_err(|e| Error::io(dest, e))?;
    } else {
        return Err(Error::new(
            ErrorCode::Unsupported,
            format!(
                "{} is a special file (device, socket or pipe)",
                src.display()
            ),
        ));
    }
    let after = fs::symlink_metadata(src).map_err(|e| Error::io(src, e))?;
    if fingerprint(&meta) != fingerprint(&after) {
        return Err(Error::new(
            ErrorCode::Io,
            format!(
                "{} changed during copying; the copy was not committed",
                src.display()
            ),
        ));
    }
    Ok(())
}

type Fingerprint = (u64, u64, u64, i64, i64, i64, i64);
fn fingerprint(meta: &fs::Metadata) -> Fingerprint {
    (
        meta.dev(),
        meta.ino(),
        meta.len(),
        meta.mtime(),
        meta.mtime_nsec(),
        meta.ctime(),
        meta.ctime_nsec(),
    )
}
fn tree_fingerprint(
    path: &Path,
    ctx: &Context,
    result: &mut Vec<(PathBuf, Fingerprint)>,
) -> Result<()> {
    ctx.check()?;
    let meta = fs::symlink_metadata(path).map_err(|e| Error::io(path, e))?;
    result.push((path.to_path_buf(), fingerprint(&meta)));
    if meta.is_dir() {
        for child in fs::read_dir(path).map_err(|e| Error::io(path, e))? {
            tree_fingerprint(&child.map_err(|e| Error::io(path, e))?.path(), ctx, result)?;
        }
    }
    Ok(())
}

fn delete_tree(path: &Path, ctx: &Context, count: &mut u64) -> Result<()> {
    ctx.check()?;
    let meta = fs::symlink_metadata(path).map_err(|e| Error::io(path, e))?;
    if meta.is_dir() {
        for child in fs::read_dir(path).map_err(|e| Error::io(path, e))? {
            delete_tree(&child.map_err(|e| Error::io(path, e))?.path(), ctx, count)?;
        }
        fs::remove_dir(path).map_err(|e| Error::io(path, e))?;
    } else {
        fs::remove_file(path).map_err(|e| Error::io(path, e))?;
    }
    *count += 1;
    ctx.progress("Deleting", 0, *count, true);
    Ok(())
}

impl Local {
    fn writable(&self, ctx: &Context) -> Result<()> {
        ctx.check()?;
        if self.read_only {
            Err(Error::new(
                ErrorCode::PermissionDenied,
                "The local connector is read-only",
            ))
        } else {
            Ok(())
        }
    }

    fn copy_to(
        &self,
        source: &Location,
        destination: &Location,
        unique: bool,
        ctx: &Context,
    ) -> Result<Location> {
        self.writable(ctx)?;
        let (src, dest) = (path(source)?, path(destination)?);
        if !unique {
            ensure_absent(&dest)?;
        }
        prevent_recursion(&src, &dest)?;
        let is_directory = fs::symlink_metadata(&src)
            .map_err(|e| Error::io(&src, e))?
            .is_dir();
        let staging = tempfile::Builder::new()
            .prefix(".niri-tonic-copy-")
            .tempdir_in(parent(&dest)?)
            .map_err(|e| Error::io(&dest, e))?;
        let staged = staging.path().join("item");
        copy_tree(&src, &staged, ctx, &mut 0)?;
        let mut suffix = 0u64;
        loop {
            ctx.check()?;
            let candidate = numbered_destination(&dest, suffix, is_directory)?;
            match rename_exclusive(&staged, &candidate) {
                Ok(()) => return Ok(location(&candidate)),
                Err(error) if unique && error.code == ErrorCode::AlreadyExists => {
                    suffix = suffix.checked_add(1).ok_or_else(|| {
                        Error::new(ErrorCode::AlreadyExists, "No available copy name")
                    })?;
                }
                Err(error) => return Err(error),
            }
        }
    }
}

fn numbered_destination(destination: &Path, number: u64, is_directory: bool) -> Result<PathBuf> {
    if number == 0 {
        return Ok(destination.to_path_buf());
    }
    let name = destination
        .file_name()
        .ok_or_else(|| Error::new(ErrorCode::InvalidInput, "Cannot copy the filesystem root"))?;
    let leaf = Path::new(name);
    let mut numbered = OsString::from(if is_directory {
        name
    } else {
        leaf.file_stem().unwrap_or(name)
    });
    numbered.push(format!("-{number}"));
    if !is_directory && let Some(extension) = leaf.extension() {
        numbered.push(".");
        numbered.push(extension);
    }
    Ok(parent(destination)?.join(numbered))
}

impl Connector for Local {
    fn id(&self) -> &str {
        "local"
    }
    fn capabilities(&self) -> Capabilities {
        let w = !self.read_only;
        Capabilities {
            read: true,
            write: w,
            create: w,
            rename: w,
            copy: w,
            move_item: w,
            trash: w,
            delete: w,
            archive: w,
            watch: true,
            pagination: false,
            // Atomicity depends on the source and destination filesystem.
            atomic_move: false,
        }
    }
    fn authentication(&self) -> Authentication {
        Authentication::NotRequired
    }
    fn list(&self, directory: &Location, cursor: Option<&str>, ctx: &Context) -> Result<Page> {
        ctx.check()?;
        if cursor.is_some() {
            return Err(Error::new(
                ErrorCode::StaleCursor,
                "Local listings do not accept cursors",
            ));
        }
        let path = path(directory)?;
        let mut entries = vec![];
        for child in fs::read_dir(&path).map_err(|e| Error::io(&path, e))? {
            ctx.check()?;
            let child = child.map_err(|e| Error::io(&path, e))?;
            match metadata(&child.path()) {
                Ok(entry) => entries.push(entry),
                Err(e) if e.code == ErrorCode::NotFound => {} // concurrent removal
                Err(e) => return Err(e),
            }
        }
        Ok(Page {
            entries,
            next: None,
        })
    }
    fn stat(&self, loc: &Location, ctx: &Context) -> Result<Entry> {
        ctx.check()?;
        metadata(&path(loc)?)
    }
    fn read(&self, loc: &Location, offset: u64, limit: usize, ctx: &Context) -> Result<Vec<u8>> {
        ctx.check()?;
        let path = path(loc)?;
        let mut file = OpenOptions::new()
            .read(true)
            .custom_flags(rustix::fs::OFlags::NONBLOCK.bits() as i32)
            .open(&path)
            .map_err(|e| Error::io(&path, e))?;
        if !file.metadata().map_err(|e| Error::io(&path, e))?.is_file() {
            return Err(Error::new(
                ErrorCode::Unsupported,
                "Only regular files can be read",
            ));
        }
        file.seek(SeekFrom::Start(offset))
            .map_err(|e| Error::io(&path, e))?;
        let mut bytes = Vec::new();
        file.take(limit.min(1024 * 1024) as u64)
            .read_to_end(&mut bytes)
            .map_err(|e| Error::io(&path, e))?;
        ctx.check()?;
        Ok(bytes)
    }
    fn write_new(&self, loc: &Location, input: &mut dyn Read, ctx: &Context) -> Result<()> {
        self.writable(ctx)?;
        let dest = path(loc)?;
        ensure_absent(&dest)?;
        let mut staging =
            tempfile::NamedTempFile::new_in(parent(&dest)?).map_err(|e| Error::io(&dest, e))?;
        copy_stream(input, staging.as_file_mut(), ctx, &mut 0)?;
        ctx.check()?;
        rename_exclusive(staging.path(), &dest)
    }
    fn create_directory(&self, loc: &Location, ctx: &Context) -> Result<()> {
        self.writable(ctx)?;
        let path = path(loc)?;
        fs::create_dir(&path).map_err(|e| Error::io(&path, e))
    }
    fn rename(&self, src: &Location, dest: &Location, ctx: &Context) -> Result<()> {
        self.writable(ctx)?;
        let (src, dest) = (path(src)?, path(dest)?);
        parent(&src)?;
        prevent_recursion(&src, &dest)?;
        rename_exclusive(&src, &dest)
    }
    fn copy(&self, src: &Location, dest: &Location, ctx: &Context) -> Result<()> {
        self.copy_to(src, dest, false, ctx).map(|_| ())
    }
    fn copy_unique(&self, src: &Location, dest: &Location, ctx: &Context) -> Result<Location> {
        self.copy_to(src, dest, true, ctx)
    }
    fn move_item(&self, src: &Location, dest: &Location, ctx: &Context) -> Result<()> {
        self.writable(ctx)?;
        let (source, destination) = (path(src)?, path(dest)?);
        parent(&source)?;
        ensure_absent(&destination)?;
        prevent_recursion(&source, &destination)?;
        match renameat_with(CWD, &source, CWD, &destination, RenameFlags::NOREPLACE) {
            Ok(()) => Ok(()),
            Err(rustix::io::Errno::XDEV) => {
                // Cross-device moves retain the original until the copy commits.
                let mut before = Vec::new();
                tree_fingerprint(&source, ctx, &mut before)?;
                self.copy(src, dest, ctx)?;
                let mut after = Vec::new();
                let validation = tree_fingerprint(&source, ctx, &mut after);
                before.sort_unstable_by(|a, b| a.0.cmp(&b.0));
                after.sort_unstable_by(|a, b| a.0.cmp(&b.0));
                if validation.is_err() || before != after {
                    return Err(Error::new(
                        ErrorCode::Io,
                        format!(
                            "Copy completed at {}, but the source changed or the operation was cancelled. The source has been kept.",
                            destination.display()
                        ),
                    ));
                }
                ctx.progress("Removing copied source", 0, 0, false);
                let cleanup = Context::new(|_| {});
                delete_tree(&source, &cleanup, &mut 0).map_err(|e| {
                    Error::new(
                        e.code,
                        format!(
                            "Copy completed at {} but source cleanup failed: {e}",
                            destination.display()
                        ),
                    )
                })
            }
            Err(e) => Err(Error::io(&destination, e.into())),
        }
    }
    fn delete(&self, loc: &Location, mode: DeleteMode, ctx: &Context) -> Result<()> {
        self.writable(ctx)?;
        let path = path(loc)?;
        parent(&path)?;
        match mode {
            DeleteMode::Permanent => delete_tree(&path, ctx, &mut 0),
            DeleteMode::Trash => {
                ctx.progress("Moving to Trash", 0, 0, false);
                gio::File::for_path(&path).trash(gio::Cancellable::NONE).map_err(|e| Error::new(ErrorCode::Io, format!("Cannot move {} to Trash: {e}. No permanent deletion was attempted.",path.display())))
            }
        }
    }
    fn archive(&self, loc: &Location, ctx: &Context) -> Result<Location> {
        self.writable(ctx)?;
        let source = path(loc)?;
        let parent = parent(&source)?;
        if source.file_name().is_some_and(|s| s == ".archive")
            || parent.file_name().is_some_and(|s| s == ".archive")
        {
            return Err(Error::new(
                ErrorCode::InvalidInput,
                "This item is already an archive or inside one",
            ));
        }
        let archive = parent.join(".archive");
        match fs::create_dir(&archive) {
            Ok(()) => {}
            Err(e) if e.kind() == std::io::ErrorKind::AlreadyExists => {
                if !fs::symlink_metadata(&archive)
                    .map_err(|e| Error::io(&archive, e))?
                    .is_dir()
                {
                    return Err(Error::new(
                        ErrorCode::InvalidInput,
                        ".archive must be a real directory, not a symlink or file",
                    ));
                }
            }
            Err(e) => return Err(Error::io(&archive, e)),
        }
        let dest = location(
            &archive.join(
                source
                    .file_name()
                    .ok_or_else(|| Error::new(ErrorCode::InvalidInput, "Cannot archive root"))?,
            ),
        );
        self.rename(loc, &dest, ctx)?;
        Ok(dest)
    }
    fn restore(&self, loc: &Location, ctx: &Context) -> Result<Location> {
        self.writable(ctx)?;
        let source = path(loc)?;
        let archive = parent(&source)?;
        if archive.file_name().is_none_or(|n| n != ".archive")
            || !fs::symlink_metadata(archive)
                .map_err(|e| Error::io(archive, e))?
                .is_dir()
        {
            return Err(Error::new(
                ErrorCode::InvalidInput,
                "Select an item directly inside a real .archive directory",
            ));
        }
        let dest = location(
            &parent(archive)?.join(
                source
                    .file_name()
                    .ok_or_else(|| Error::new(ErrorCode::InvalidInput, "Cannot restore root"))?,
            ),
        );
        self.rename(loc, &dest, ctx)?;
        Ok(dest)
    }
}
