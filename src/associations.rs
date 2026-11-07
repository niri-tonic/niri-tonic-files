use crate::config::Associations;
use gtk::{gio, prelude::*};
use std::{
    collections::BTreeMap,
    ffi::OsString,
    path::{Path, PathBuf},
    process::{Command, Stdio},
};

pub fn matching<'a>(
    config: &'a Associations,
    path: &Path,
    mime: Option<&str>,
) -> Option<&'a Vec<String>> {
    path.extension()
        .and_then(|s| s.to_str())
        .and_then(|ext| config.extensions.get(&ext.to_lowercase()))
        .or_else(|| mime.and_then(|mime| config.mime_types.get(mime)))
        .or_else(|| {
            mime.and_then(|m| m.split_once('/'))
                .and_then(|(group, _)| config.mime_types.get(&format!("{group}/*")))
        })
}

pub fn argv(command: &[String], paths: &[PathBuf]) -> Result<(OsString, Vec<OsString>), String> {
    let (executable, args) = command
        .split_first()
        .filter(|(s, _)| !s.is_empty())
        .ok_or("No application command configured")?;
    let mut args: Vec<OsString> = args.iter().map(OsString::from).collect();
    for path in paths {
        // Absolute paths cannot be mistaken for command options.
        if !path.is_absolute() {
            return Err("Application launch requires absolute paths".into());
        }
        args.push(path.as_os_str().to_owned());
    }
    Ok((executable.into(), args))
}

pub fn launch_command(command: &[String], paths: &[PathBuf]) -> Result<(), String> {
    let (executable, args) = argv(command, paths)?;
    let mut child = Command::new(executable)
        .args(args)
        .stdin(Stdio::null())
        .spawn()
        .map_err(|e| format!("Cannot start {}: {e}", command[0]))?;
    std::thread::spawn(move || {
        let _ = child.wait();
    });
    Ok(())
}

/// GIO queries and launches run on a worker, never on GTK's main thread.
pub fn open(config: &Associations, paths: &[PathBuf]) -> Result<(), String> {
    let mut commands: BTreeMap<Vec<String>, Vec<PathBuf>> = BTreeMap::new();
    let mut defaults: BTreeMap<String, Vec<gio::File>> = BTreeMap::new();
    for path in paths {
        let file = gio::File::for_path(path);
        let info = file
            .query_info(
                "standard::content-type",
                gio::FileQueryInfoFlags::NONE,
                gio::Cancellable::NONE,
            )
            .map_err(|e| format!("{}: {e}", path.display()))?;
        let content_type = info
            .content_type()
            .unwrap_or_else(|| "application/octet-stream".into());
        let mime = gio::content_type_get_mime_type(&content_type);
        if let Some(command) = matching(config, path, mime.as_deref()) {
            commands
                .entry(command.clone())
                .or_default()
                .push(path.clone());
        } else {
            defaults
                .entry(content_type.to_string())
                .or_default()
                .push(file);
        }
    }
    for (command, paths) in commands {
        launch_command(&command, &paths)?;
    }
    for (mime, files) in defaults {
        let app = gio::AppInfo::default_for_type(&mime, false)
            .ok_or_else(|| format!("No default application for {mime}. Use Open With."))?;
        app.launch(&files, gio::AppLaunchContext::NONE)
            .map_err(|e| format!("Cannot open files: {e}"))?;
    }
    Ok(())
}
