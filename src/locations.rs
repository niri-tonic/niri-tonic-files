//! UI paths are virtual for remote instances; never pass them to filesystem I/O.
use crate::{
    connector::{Error, ErrorCode, Location, Result},
    local,
};
pub use local::{sort, valid_name};
use std::{
    os::unix::ffi::OsStringExt,
    path::{Component, Path, PathBuf},
};

const PREFIX: &str = "/.niri-tonic-connectors";
pub fn valid_id(id: &str) -> bool {
    !id.is_empty()
        && id != "local"
        && id
            .bytes()
            .all(|b| b.is_ascii_alphanumeric() || b == b'-' || b == b'_')
}
pub fn root(id: &str) -> PathBuf {
    Path::new(PREFIX).join(id)
}
pub fn is_remote(path: &Path) -> bool {
    path.starts_with(PREFIX)
}
fn invalid() -> Error {
    Error::new(ErrorCode::InvalidInput, "Invalid connector path or URI")
}
fn split(path: &Path) -> Result<(&str, PathBuf)> {
    let rest = path.strip_prefix(PREFIX).map_err(|_| invalid())?;
    let mut components = rest.components();
    let id = components
        .next()
        .and_then(|c| c.as_os_str().to_str())
        .filter(|id| valid_id(id))
        .ok_or_else(invalid)?;
    let mut remote = PathBuf::from("/");
    for c in components {
        match c {
            Component::Normal(name) if name.to_str().is_some() => remote.push(name),
            _ => return Err(invalid()),
        }
    }
    Ok((id, remote))
}
pub fn location(path: &Path) -> Location {
    if is_remote(path) {
        match split(path) {
            Ok((id, remote)) => Location {
                connector: id.into(),
                id: local::location(&remote).id,
            },
            Err(_) => Location {
                connector: String::new(),
                id: String::new(),
            },
        }
    } else {
        local::location(path)
    }
}
pub fn path(location: &Location) -> Result<PathBuf> {
    let encoded = location
        .id
        .strip_prefix("file://")
        .filter(|s| s.starts_with('/'))
        .ok_or_else(invalid)?;
    let mut bytes = Vec::new();
    let mut input = encoded.bytes();
    while let Some(b) = input.next() {
        let b = if b == b'%' {
            let hi = input
                .next()
                .and_then(|b| (b as char).to_digit(16))
                .ok_or_else(invalid)?;
            let lo = input
                .next()
                .and_then(|b| (b as char).to_digit(16))
                .ok_or_else(invalid)?;
            (hi * 16 + lo) as u8
        } else {
            b
        };
        if b == 0 {
            return Err(invalid());
        }
        bytes.push(b);
    }
    let decoded = PathBuf::from(std::ffi::OsString::from_vec(bytes));
    if decoded
        .components()
        .any(|c| matches!(c, Component::ParentDir))
    {
        return Err(invalid());
    }
    if location.connector == "local" {
        if is_remote(&decoded) {
            return Err(invalid());
        }
        return Ok(decoded);
    }
    if !valid_id(&location.connector) || decoded.to_str().is_none() {
        return Err(invalid());
    }
    Ok(root(&location.connector).join(decoded.strip_prefix("/").map_err(|_| invalid())?))
}
pub fn display(path: &Path) -> String {
    display_with_scheme(path, "filen")
}
pub fn display_with_scheme(path: &Path, scheme: &str) -> String {
    if let Ok((id, remote)) = split(path) {
        format!("{scheme}://{id}{}", &local::location(&remote).id[7..])
    } else {
        path.to_str()
            .map(str::to_owned)
            .unwrap_or_else(|| local::location(path).id)
    }
}
pub fn is_remote_uri(text: &str) -> bool {
    text.starts_with("filen://") || text.starts_with("drive://")
}
pub fn transfer_name(entry: &crate::connector::Entry) -> Result<std::ffi::OsString> {
    if let Some(name) = &entry.transfer_name {
        valid_name(name)?;
        if name.chars().any(|c| c.is_control() || c == '\\') {
            return Err(invalid());
        }
        return Ok(name.into());
    }
    path(&entry.location)?
        .file_name()
        .map(std::ffi::OsStr::to_os_string)
        .ok_or_else(invalid)
}
pub fn parse(text: &str) -> Result<PathBuf> {
    if let Some(rest) = text
        .strip_prefix("filen://")
        .or_else(|| text.strip_prefix("drive://"))
    {
        let (id, tail) = rest.split_once('/').unwrap_or((rest, ""));
        if !valid_id(id) {
            return Err(invalid());
        }
        path(&Location {
            connector: id.into(),
            id: format!("file:///{tail}"),
        })
    } else if text.starts_with("file:") {
        path(&Location {
            connector: "local".into(),
            id: text.into(),
        })
    } else {
        let p = PathBuf::from(text);
        if is_remote(&p) {
            split(&p)?;
        }
        Ok(p)
    }
}
pub fn parent(path: &Path) -> Option<PathBuf> {
    if is_remote(path) {
        let (id, _) = split(path).ok()?;
        if path == root(id) {
            return None;
        }
    }
    path.parent().map(Path::to_path_buf)
}
