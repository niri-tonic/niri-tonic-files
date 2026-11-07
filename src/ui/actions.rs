use super::Ui;
use crate::{
    associations,
    config::Favorite,
    connector::{Connector, Context, DeleteMode},
    locations as local,
};
use gtk::{gdk, gio, glib, prelude::*};
use std::{
    path::{Path, PathBuf},
    rc::Rc,
};

const CLIPBOARD_MIME: &str = "application/x-niri-tonic-files+json";
#[derive(serde::Serialize, serde::Deserialize)]
#[serde(deny_unknown_fields)]
struct FileClipboard {
    version: u32,
    locations: Vec<String>,
    cut: bool,
}

const ACTIONS: &[&str] = &[
    "open",
    "open-with",
    "new-file",
    "new-folder",
    "rename",
    "copy",
    "cut",
    "paste",
    "move",
    "trash",
    "delete",
    "archive",
    "restore",
    "archives",
    "favorite",
    "unfavorite",
    "toggle-column",
    "quit",
    "back",
    "forward",
    "parent",
    "close-column",
    "refresh",
    "hidden",
    "animations",
    "select-all",
    "about",
];

impl Ui {
    pub(super) fn install_actions(self: &Rc<Self>) {
        for &name in ACTIONS {
            let action = gio::SimpleAction::new(name, None);
            action.connect_activate(glib::clone!(
                #[weak(rename_to = ui)]
                self,
                move |_, _| ui.perform(name)
            ));
            self.window.add_action(&action);
        }
        let goto = gio::SimpleAction::new("goto", Some(&String::static_variant_type()));
        goto.connect_activate(glib::clone!(
            #[weak(rename_to = ui)]
            self,
            move |_, parameter| {
                if let Some(uri) = parameter.and_then(|v| v.get::<String>()) {
                    if let Ok(path) = ui.parse_path(&uri) {
                        ui.navigate(path);
                    }
                }
            }
        ));
        self.window.add_action(&goto);
        let detach =
            gio::SimpleAction::new("detach-favorite", Some(&String::static_variant_type()));
        detach.connect_activate(glib::clone!(
            #[weak(rename_to=ui)]
            self,
            move |_, parameter| {
                if let Some(path) = parameter
                    .and_then(|v| v.get::<String>())
                    .and_then(|uri| ui.parse_path(&uri).ok())
                {
                    ui.change_favorites(vec![path], true);
                }
            }
        ));
        self.window.add_action(&detach);
        let keys = gtk::EventControllerKey::new();
        keys.set_propagation_phase(gtk::PropagationPhase::Capture);
        keys.connect_key_pressed(glib::clone!(
            #[weak(rename_to = ui)]
            self,
            #[upgrade_or]
            glib::Propagation::Proceed,
            move |_, key, _, modifiers| {
                let focus = gtk::prelude::RootExt::focus(&ui.window);
                let editing = focus.as_ref().is_some_and(|w| {
                    w.is::<gtk::Text>() || w.is::<gtk::Entry>() || w.is::<gtk::TextView>()
                });
                let action = super::shortcuts::command(key, modifiers);
                if action == Some("quit") {
                    ui.window.close();
                    return glib::Propagation::Stop;
                }
                if key == gdk::Key::Escape && ui.menu.borrow().is_some() {
                    ui.dismiss_menu();
                    return glib::Propagation::Stop;
                }
                if editing {
                    return glib::Propagation::Proceed;
                }
                if let Some(action) = action {
                    if let Some(action) = ui.window.lookup_action(action) {
                        action.activate(None);
                    }
                    return glib::Propagation::Stop;
                }
                let file_focus = focus.is_some_and(|w| {
                    w.is::<gtk::ListView>() || w.ancestor(gtk::ListView::static_type()).is_some()
                });
                if modifiers
                    .intersects(gdk::ModifierType::CONTROL_MASK | gdk::ModifierType::ALT_MASK)
                    || matches!(
                        key,
                        gdk::Key::Delete
                            | gdk::Key::BackSpace
                            | gdk::Key::F1
                            | gdk::Key::F2
                            | gdk::Key::F5
                            | gdk::Key::F10
                            | gdk::Key::Menu
                    )
                    || (file_focus
                        && matches!(key, gdk::Key::Return | gdk::Key::KP_Enter | gdk::Key::space))
                {
                    return glib::Propagation::Stop;
                }
                glib::Propagation::Proceed
            }
        ));
        self.window.add_controller(keys);
    }

    pub(super) fn update_actions(&self) {
        let entries = self.selected_entries();
        let selection = !entries.is_empty();
        let folder = self.active_path();
        let writable = folder.as_ref().is_some_and(|path| {
            !self
                .connector
                .capabilities_at(&local::location(path))
                .read_only()
        }) && self.job.borrow().is_none()
            && self.rename.borrow().is_none();
        let sources_writable = entries
            .iter()
            .all(|entry| !self.connector.capabilities_at(&entry.location).read_only())
            && self.job.borrow().is_none()
            && self.rename.borrow().is_none();
        let nav = self.nav.borrow();
        let set = |name: &str, enabled: bool| {
            if let Some(action) = self
                .window
                .lookup_action(name)
                .and_downcast::<gio::SimpleAction>()
            {
                action.set_enabled(enabled);
            }
        };
        for name in ["open", "open-with", "copy"] {
            set(name, selection);
        }
        for name in ["cut", "move", "trash", "delete", "archive"] {
            let supported = entries.iter().all(|entry| {
                let caps = self.connector.capabilities_at(&entry.location);
                match name {
                    "cut" | "move" => caps.move_item,
                    "trash" => caps.trash,
                    "delete" => caps.delete,
                    "archive" => caps.archive,
                    _ => false,
                }
            });
            set(name, selection && sources_writable && supported);
        }
        set(
            "restore",
            selection
                && writable
                && folder
                    .as_ref()
                    .is_some_and(|p| p.file_name().is_some_and(|n| n == ".archive")),
        );
        set("rename", entries.len() == 1 && sources_writable);
        for name in ["new-file", "new-folder"] {
            set(name, folder.is_some() && writable);
        }
        set("paste", folder.is_some() && writable);
        set("archives", folder.is_some());
        set(
            "favorite",
            folder.is_some() && self.job.borrow().is_none() && !self.saving_favorites.get(),
        );
        set(
            "unfavorite",
            folder.is_some() && !self.saving_favorites.get(),
        );
        set("back", nav.can_back());
        set("forward", nav.can_forward());
        set(
            "parent",
            folder.as_ref().and_then(|p| local::parent(p)).is_some(),
        );
        set("close-column", folder.is_some());
        set("select-all", folder.is_some());
    }

    pub(super) fn context_menu(
        self: &Rc<Self>,
        anchor: &impl IsA<gtk::Widget>,
        x: f64,
        y: f64,
    ) -> gtk::Popover {
        let index = self.nav.borrow().snapshot().active;
        let list = self
            .columns
            .borrow()
            .get(index)
            .map(|column| column.list.clone());
        if let Some(list) = list {
            list.grab_focus();
        }
        self.update_actions();
        let menu = gio::Menu::new();
        let selection = !self.selected_entries().is_empty();
        let add_section = |items: &[(&str, &str)]| {
            let section = gio::Menu::new();
            for (label, action) in items {
                section.append(Some(label), Some(&format!("win.{action}")));
            }
            menu.append_section(None, &section);
        };
        if selection {
            add_section(&[("Open", "open"), ("Rename…", "rename")]);
            add_section(&[("Copy", "copy"), ("Cut", "cut"), ("Paste", "paste")]);
            add_section(&[
                ("Delete", "delete"),
                ("Archive", "archive"),
                ("Restore from Archive", "restore"),
            ]);
        } else {
            add_section(&[
                ("New File…", "new-file"),
                ("New Folder…", "new-folder"),
                ("Paste", "paste"),
            ]);
        }
        let favorite_paths = self.favorite_paths();
        let is_favorite = !favorite_paths.is_empty()
            && favorite_paths.iter().all(|p| {
                self.config
                    .borrow()
                    .favorites
                    .iter()
                    .any(|f| self.paths.expand(&f.path) == *p)
            });
        add_section(&[
            (
                if is_favorite {
                    "Remove from Favorites"
                } else {
                    "Add to Favorites"
                },
                if is_favorite {
                    "unfavorite"
                } else {
                    "favorite"
                },
            ),
            ("View Archives", "archives"),
        ]);
        self.show_menu(&menu, anchor, x, y, "File actions")
    }

    pub(super) fn favorite_menu(
        self: &Rc<Self>,
        anchor: &impl IsA<gtk::Widget>,
        path: &Path,
        x: f64,
        y: f64,
    ) -> gtk::Popover {
        let menu = gio::Menu::new();
        let remove = gio::MenuItem::new(Some("Remove from Favorites"), None);
        remove.set_action_and_target_value(
            Some("win.detach-favorite"),
            Some(&self.display_path(path).to_variant()),
        );
        menu.append_item(&remove);
        if let Some(parent) = local::parent(path) {
            let parent_item = gio::MenuItem::new(Some("Open Parent Folder"), None);
            parent_item.set_action_and_target_value(
                Some("win.goto"),
                Some(&self.display_path(&parent).to_variant()),
            );
            menu.append_item(&parent_item);
        }
        self.show_menu(&menu, anchor, x, y, "Favorite actions")
    }

    fn show_menu(
        self: &Rc<Self>,
        menu: &gio::Menu,
        anchor: &impl IsA<gtk::Widget>,
        x: f64,
        y: f64,
        label: &str,
    ) -> gtk::Popover {
        self.dismiss_menu();
        let popover = popover_from_menu(
            menu.upcast_ref(),
            &self.window,
            (self.window.height() - 100).max(200),
        );
        popover.update_property(&[gtk::accessible::Property::Label(label)]);
        popover.set_has_arrow(false);
        popover.set_parent(anchor);
        popover.set_pointing_to(Some(&gdk::Rectangle::new(x as i32, y as i32, 1, 1)));
        self.menu.replace(Some(popover.clone()));
        popover.connect_closed(glib::clone!(
            #[weak(rename_to=ui)]
            self,
            move |popover| {
                let is_current = ui.menu.borrow().as_ref() == Some(popover);
                if is_current {
                    ui.menu.take();
                }
                if popover.parent().is_some() {
                    popover.unparent();
                }
            }
        ));
        popover.popup();
        popover
    }

    fn perform(self: &Rc<Self>, action: &str) {
        match action {
            "open" => self.open_selected(),
            "open-with" => self.open_with(),
            "new-file" | "new-folder" => self.new_item(action == "new-folder"),
            "rename" => self.rename_item(),
            "copy" | "cut" => self.copy_selection(action == "cut"),
            "paste" => self.paste(),
            "move" => self.move_dialog(),
            "trash" => self.delete_items(false),
            "delete" => self.delete_items(true),
            "archive" | "restore" => {
                let entries = self.selected_entries();
                let restore = action == "restore";
                self.run_job(
                    if restore { "Restoring" } else { "Archiving" },
                    true,
                    move |connector, ctx| {
                        for (i, entry) in entries.iter().enumerate() {
                            ctx.check().map_err(|e| e.to_string())?;
                            let result = if restore {
                                connector.restore(&entry.location, ctx)
                            } else {
                                connector.archive(&entry.location, ctx)
                            };
                            result
                                .map_err(|e| format!("{} completed. {e}", super::item_count(i)))?;
                            ctx.progress("Processing archive", 0, (i + 1) as u64, true);
                        }
                        Ok(format!(
                            "{} {}",
                            super::item_count(entries.len()),
                            if restore { "restored" } else { "archived" }
                        ))
                    },
                );
            }
            "archives" => {
                if let Some(path) = self.active_path() {
                    self.navigate(path.join(".archive"));
                }
            }
            "favorite" => {
                let paths = self.favorite_paths();
                let remove = !paths.is_empty()
                    && paths.iter().all(|path| {
                        self.config
                            .borrow()
                            .favorites
                            .iter()
                            .any(|f| self.paths.expand(&f.path) == *path)
                    });
                self.change_favorites(paths, remove);
            }
            "unfavorite" => self.change_favorites(self.favorite_paths(), true),
            "quit" => self.window.close(),
            "toggle-column" => {
                if self.nav.borrow().snapshot().columns.len() == 2 {
                    let index = self.nav.borrow().snapshot().active;
                    self.nav.borrow_mut().close(index);
                } else {
                    let path = self
                        .active_path()
                        .unwrap_or_else(|| self.paths.home.clone());
                    self.nav.borrow_mut().open(path);
                }
                self.rebuild(true);
            }
            "back" => {
                let changed = self.nav.borrow_mut().back();
                if changed {
                    self.rebuild(true);
                }
            }
            "forward" => {
                let changed = self.nav.borrow_mut().forward();
                if changed {
                    self.rebuild(true);
                }
            }
            "parent" => {
                let changed = self.nav.borrow_mut().parent();
                if changed {
                    self.rebuild(true);
                }
            }
            "close-column" => {
                let index = self.nav.borrow().snapshot().active;
                let changed = self.nav.borrow_mut().close(index);
                if changed {
                    self.rebuild(true);
                }
            }
            "refresh" => self.refresh_panes(),
            "hidden" => {
                let hidden = self.config.borrow().view.hidden;
                self.config.borrow_mut().view.hidden = !hidden;
                self.rebuild(false);
            }
            "animations" => {
                let enabled = self.config.borrow().navigation.animations;
                self.config.borrow_mut().navigation.animations = !enabled;
                self.message(
                    if enabled {
                        "Animations disabled for this session"
                    } else {
                        "Animations enabled for this session"
                    },
                    false,
                );
            }
            "select-all" => {
                let index = self.nav.borrow().snapshot().active;
                if let Some(column) = self.columns.borrow().get(index) {
                    column.selection.select_all();
                }
            }
            "about" => {
                gtk::AboutDialog::builder()
                    .transient_for(&self.window)
                    .modal(true)
                    .program_name("Niri Tonic Files")
                    .version(crate::APP_VERSION)
                    .comments(
                        "A quiet, column-based file explorer for Niri and Dank Material Shell.",
                    )
                    .license_type(gtk::License::Gpl30Only)
                    .website("https://github.com/tdesaules/niri-tonic-files")
                    .build()
                    .present();
            }
            _ => {}
        }
    }

    pub(super) fn open_selected(self: &Rc<Self>) {
        let entries = self.selected_entries();
        let mut files = vec![];
        let mut folders = vec![];
        for entry in entries {
            if let Ok(path) = local::path(&entry.location) {
                if entry.navigable {
                    folders.push(path);
                } else {
                    files.push(path);
                }
            }
        }
        // Open one directory in the column flow; multiple directories get windows.
        if folders.len() == 1 {
            self.navigate(folders.remove(0));
        } else if !folders.is_empty() {
            let executable = std::env::current_exe();
            match executable {
                Ok(executable) => {
                    if let Err(e) = std::process::Command::new(executable)
                        .args(&folders)
                        .spawn()
                        .map(|mut child| {
                            std::thread::spawn(move || {
                                let _ = child.wait();
                            })
                        })
                    {
                        self.message(&e.to_string(), true);
                    }
                }
                Err(e) => self.message(&e.to_string(), true),
            }
        }
        if !files.is_empty() {
            let associations = self.config.borrow().associations.clone();
            let storage = self.connector.clone();
            self.run_job("Preparing files", false, move |_, context| {
                let local_files = files
                    .iter()
                    .map(|path| {
                        storage
                            .materialize(&local::location(path), context)
                            .map_err(|e| e.to_string())
                    })
                    .collect::<Result<Vec<_>, _>>()?;
                associations::open(&associations, &local_files)?;
                Ok(format!("Opened {} files", local_files.len()))
            });
        }
    }

    fn copy_selection(&self, cut: bool) {
        let paths = self.selected_paths();
        if paths.is_empty() {
            return;
        }
        let files: Vec<_> = paths
            .iter()
            .map(|p| {
                if local::is_remote(p) {
                    gio::File::for_uri(&self.display_path(p))
                } else {
                    gio::File::for_path(p)
                }
            })
            .collect();
        let filelist = gdk::FileList::from_array(&files);
        let uris = files
            .iter()
            .map(|f| f.uri().to_string())
            .collect::<Vec<_>>()
            .join("\n");
        let gnome = format!("{}\n{uris}", if cut { "cut" } else { "copy" });
        let payload = FileClipboard {
            version: 1,
            locations: paths.iter().map(|p| self.display_path(p)).collect(),
            cut,
        };
        let bytes = match serde_json::to_vec(&payload) {
            Ok(bytes) => bytes,
            Err(error) => {
                self.message(&format!("Cannot copy locations: {error}"), true);
                return;
            }
        };
        let provider = gdk::ContentProvider::new_union(&[
            gdk::ContentProvider::for_bytes(CLIPBOARD_MIME, &glib::Bytes::from_owned(bytes)),
            gdk::ContentProvider::for_value(&filelist.to_value()),
            gdk::ContentProvider::for_bytes(
                "x-special/gnome-copied-files",
                &glib::Bytes::from_owned(gnome.into_bytes()),
            ),
        ]);
        match self.window.clipboard().set_content(Some(&provider)) {
            Ok(()) => {
                self.message(
                    &format!(
                        "{} {}",
                        if cut { "Cut" } else { "Copied" },
                        super::item_count(paths.len())
                    ),
                    false,
                );
            }
            Err(e) => self.message(&format!("Clipboard unavailable: {e}"), true),
        }
    }

    fn paste(self: &Rc<Self>) {
        let Some(destination) = self.active_path() else {
            return;
        };
        let clipboard = self.window.clipboard();
        if clipboard.formats().contain_mime_type(CLIPBOARD_MIME) {
            let weak = Rc::downgrade(self);
            let config = self.config.borrow().clone();
            glib::spawn_future_local(async move {
                let result = async {
                    let (stream, _) = clipboard
                        .read_future(&[CLIPBOARD_MIME], glib::Priority::DEFAULT)
                        .await
                        .map_err(|e| e.to_string())?;
                    let mut bytes = Vec::new();
                    loop {
                        let chunk = stream
                            .read_bytes_future(65536, glib::Priority::DEFAULT)
                            .await
                            .map_err(|e| e.to_string())?;
                        if chunk.is_empty() {
                            break;
                        }
                        if bytes.len() + chunk.len() > 1024 * 1024 {
                            return Err("File clipboard is too large".to_string());
                        }
                        bytes.extend_from_slice(&chunk);
                    }
                    let payload: FileClipboard = serde_json::from_slice(&bytes)
                        .map_err(|e| format!("Invalid file clipboard: {e}"))?;
                    if payload.version != 1 || payload.locations.is_empty() {
                        return Err("Unsupported or empty file clipboard".to_string());
                    }
                    let paths = payload
                        .locations
                        .iter()
                        .map(|s| config.parse_path(s).map_err(|e| e.to_string()))
                        .collect::<Result<Vec<_>, _>>()?;
                    if paths.iter().any(|p| !p.is_absolute()) {
                        return Err("File clipboard paths must be absolute".into());
                    }
                    Ok((paths, payload.cut))
                }
                .await;
                let Some(ui) = weak.upgrade() else {
                    return;
                };
                match result {
                    Ok((paths, cut)) => ui.transfer(paths, destination, cut),
                    Err(error) => ui.message(&error, true),
                }
            });
            return;
        }
        let weak = Rc::downgrade(self);
        self.window.clipboard().read_value_async(
            gdk::FileList::static_type(),
            glib::Priority::DEFAULT,
            gio::Cancellable::NONE,
            move |result| {
                let Some(ui) = weak.upgrade() else {
                    return;
                };
                match result.and_then(|v| {
                    v.get::<gdk::FileList>().map_err(|e| {
                        glib::Error::new(gio::IOErrorEnum::InvalidData, &e.to_string())
                    })
                }) {
                    Ok(files) => {
                        let files = files.files();
                        let paths: Vec<_> = files
                            .iter()
                            .filter_map(|file| ui.parse_path(file.uri().as_str()).ok())
                            .filter(|path| path.is_absolute())
                            .collect();
                        if paths.len() != files.len() {
                            ui.message("The clipboard contains unsupported file locations", true);
                        } else {
                            ui.transfer(paths, destination, false);
                        }
                    }
                    Err(e) => {
                        ui.message(&format!("No supported files in the clipboard: {e}"), true)
                    }
                }
            },
        );
    }

    pub(super) fn transfer(self: &Rc<Self>, paths: Vec<PathBuf>, destination: PathBuf, cut: bool) {
        self.run_job(
            if cut { "Moving" } else { "Copying" },
            true,
            move |connector, ctx| {
                for (i, path) in paths.iter().enumerate() {
                    ctx.check().map_err(|e| e.to_string())?;
                    let source = local::location(path);
                    let entry = connector.stat(&source, ctx).map_err(|e| e.to_string())?;
                    let name = local::transfer_name(&entry).map_err(|e| e.to_string())?;
                    let target = local::location(&destination.join(name));
                    let result = if cut {
                        connector.move_item(&source, &target, ctx)
                    } else {
                        connector.copy_unique(&source, &target, ctx).map(|_| ())
                    };
                    result.map_err(|e| format!("{} completed. {e}", super::item_count(i)))?;
                    ctx.progress(
                        if cut { "Moving" } else { "Copying" },
                        0,
                        (i + 1) as u64,
                        true,
                    );
                }
                Ok(format!(
                    "{} {}",
                    super::item_count(paths.len()),
                    if cut { "moved" } else { "copied" }
                ))
            },
        );
    }

    fn delete_items(self: &Rc<Self>, permanent: bool) {
        let entries = self.selected_entries();
        if entries.is_empty() {
            return;
        }
        let execute = move |ui: &Rc<Self>| {
            ui.run_job(
                if permanent {
                    "Deleting permanently"
                } else {
                    "Moving to Trash"
                },
                true,
                move |connector, ctx| {
                    for (i, entry) in entries.iter().enumerate() {
                        connector
                            .delete(
                                &entry.location,
                                if permanent {
                                    DeleteMode::Permanent
                                } else {
                                    DeleteMode::Trash
                                },
                                ctx,
                            )
                            .map_err(|e| format!("{} completed. {e}", super::item_count(i)))?;
                        ctx.progress("Deleting", 0, (i + 1) as u64, true);
                    }
                    Ok(format!(
                        "{} {}",
                        super::item_count(entries.len()),
                        if permanent {
                            "permanently deleted"
                        } else {
                            "moved to Trash"
                        }
                    ))
                },
            );
        };
        execute(self);
    }

    fn favorite_paths(&self) -> Vec<PathBuf> {
        let mut paths: Vec<_> = self
            .selected_entries()
            .iter()
            .filter(|e| e.navigable)
            .filter_map(|e| local::path(&e.location).ok())
            .collect();
        if paths.is_empty() {
            paths.extend(self.active_path());
        }
        paths
    }

    pub(super) fn change_favorites(self: &Rc<Self>, paths: Vec<PathBuf>, remove: bool) {
        if self.saving_favorites.get() {
            return;
        }
        let mut favorites = self.config.borrow().favorites.clone();
        for path in paths {
            let existing = favorites
                .iter()
                .position(|f| self.paths.expand(&f.path) == path);
            if remove {
                if let Some(index) = existing {
                    favorites.remove(index);
                }
            } else if existing.is_none() {
                favorites.push(Favorite {
                    name: path
                        .file_name()
                        .unwrap_or(path.as_os_str())
                        .to_string_lossy()
                        .into_owned(),
                    path: if local::is_remote(&path) {
                        PathBuf::from(self.display_path(&path))
                    } else if path.to_str().is_some() {
                        path
                    } else {
                        PathBuf::from(gio::File::for_path(path).uri().as_str())
                    },
                });
            }
        }
        if favorites == self.config.borrow().favorites {
            return;
        }
        let config_path = self.paths.config.clone();
        self.saving_favorites.set(true);
        self.update_actions();
        let desired = favorites.clone();
        let weak = Rc::downgrade(self);
        let (send, recv) = async_channel::bounded(1);
        std::thread::spawn(move || {
            // Preserve unrelated settings. YAML comments are not retained on save.
            let result = (|| {
                let mut value: serde_yaml_ng::Value = match std::fs::read_to_string(&config_path) {
                    Ok(text) => {
                        crate::config::Config::parse(&text)?;
                        serde_yaml_ng::from_str(&text).map_err(|e| e.to_string())?
                    }
                    Err(e) if e.kind() == std::io::ErrorKind::NotFound => {
                        serde_yaml_ng::to_value(crate::config::Config::default())
                            .map_err(|e| e.to_string())?
                    }
                    Err(e) => return Err(e.to_string()),
                };
                value["favorites"] = serde_yaml_ng::to_value(desired).map_err(|e| e.to_string())?;
                let text = serde_yaml_ng::to_string(&value).map_err(|e| e.to_string())?;
                crate::config::atomic_write(&config_path, text.as_bytes())
            })();
            let _ = send.send_blocking(result);
        });
        glib::spawn_future_local(async move {
            let Ok(result) = recv.recv().await else {
                return;
            };
            let Some(ui) = weak.upgrade() else {
                return;
            };
            ui.saving_favorites.set(false);
            match result {
                Ok(()) => {
                    ui.config.borrow_mut().favorites = favorites;
                    ui.update_sidebar();
                    ui.message("Favorites saved", false);
                }
                Err(e) => ui.message(&format!("Cannot save favorites: {e}"), true),
            }
            ui.update_actions();
        });
    }

    pub(super) fn run_job(
        self: &Rc<Self>,
        label: &str,
        refresh: bool,
        work: impl FnOnce(&dyn Connector, &Context) -> Result<String, String> + Send + 'static,
    ) {
        self.run_job_after(label, refresh, work, |_, _| {});
    }

    pub(super) fn run_job_after(
        self: &Rc<Self>,
        label: &str,
        refresh: bool,
        work: impl FnOnce(&dyn Connector, &Context) -> Result<String, String> + Send + 'static,
        complete: impl FnOnce(&Rc<Self>, bool) + 'static,
    ) {
        if self.job.borrow().is_some() {
            self.message("Wait for the current operation or cancel it first", true);
            return;
        }
        enum Event {
            Progress(crate::connector::Progress),
            Done(Result<String, String>),
        }
        let (send, recv) = async_channel::bounded(32);
        let progress_send = send.clone();
        let ctx = Context::new(move |p| {
            let _ = progress_send.try_send(Event::Progress(p));
        });
        self.job.replace(Some(ctx.clone()));
        self.progress.set_visible(true);
        self.cancel.set_visible(true);
        self.cancel.set_sensitive(true);
        self.message(label, false);
        self.update_actions();
        let weak = Rc::downgrade(self);
        glib::timeout_add_local(std::time::Duration::from_millis(120), move || {
            let Some(ui) = weak.upgrade() else {
                return glib::ControlFlow::Break;
            };
            if ui.job.borrow().is_none() {
                return glib::ControlFlow::Break;
            }
            ui.progress.pulse();
            glib::ControlFlow::Continue
        });
        let connector = self.connector.clone();
        std::thread::spawn(move || {
            let result = work(connector.as_ref(), &ctx);
            let _ = send.send_blocking(Event::Done(result));
        });
        let weak = Rc::downgrade(self);
        glib::spawn_future_local(async move {
            let mut complete = Some(complete);
            while let Ok(event) = recv.recv().await {
                let Some(ui) = weak.upgrade() else {
                    break;
                };
                match event {
                    Event::Progress(p) => {
                        ui.progress.pulse();
                        ui.cancel.set_sensitive(p.cancellable);
                        ui.message(
                            &format!(
                                "{} · {} · {:.1} MiB",
                                p.phase,
                                super::item_count(p.items as usize),
                                p.bytes as f64 / (1024.0 * 1024.0)
                            ),
                            false,
                        );
                    }
                    Event::Done(result) => {
                        let success = result.is_ok();
                        ui.job.replace(None);
                        ui.progress.set_visible(false);
                        ui.cancel.set_visible(false);
                        match result {
                            Ok(message) => ui.message(&message, false),
                            Err(e) => ui.message(&e, true),
                        }
                        if let Some(complete) = complete.take() {
                            complete(&ui, success);
                        }
                        if refresh {
                            // Keep the widgets under the pointer alive. Replacing
                            // a whole pane here invalidates hover/gesture state
                            // after paste; only its listing needs refreshing.
                            ui.refresh_panes();
                        } else {
                            ui.update_actions();
                        }
                        break;
                    }
                }
            }
        });
    }

    pub(super) fn expand_input(&self, text: &str) -> PathBuf {
        let path = self.paths.expand(Path::new(text));
        if path.is_absolute() {
            path
        } else {
            self.active_path()
                .unwrap_or_else(|| self.paths.home.clone())
                .join(path)
        }
    }
}

/// Native buttons provide stable AT-SPI names even on GTK releases whose
/// internal GtkModelButton accessibility exposes unnamed menu items.
fn popover_from_menu(
    menu: &gio::MenuModel,
    window: &gtk::ApplicationWindow,
    max_height: i32,
) -> gtk::Popover {
    fn content(
        menu: &gio::MenuModel,
        window: &gtk::ApplicationWindow,
        popover: &gtk::Popover,
    ) -> gtk::Box {
        let rows = gtk::Box::new(gtk::Orientation::Vertical, 2);
        for i in 0..menu.n_items() {
            if let Some(section) = menu.item_link(i, "section") {
                rows.append(&content(&section, window, popover));
                continue;
            }
            let label = menu
                .item_attribute_value(i, "label", None)
                .and_then(|v| v.get::<String>())
                .unwrap_or_default();
            if let Some(submenu) = menu.item_link(i, "submenu") {
                let expander = gtk::Expander::new(Some(&label));
                expander.set_child(Some(&content(&submenu, window, popover)));
                rows.append(&expander);
                continue;
            }
            let button = gtk::Button::with_label(&label);
            if let Some(label) = button.child().and_downcast::<gtk::Label>() {
                label.set_xalign(0.0);
            }
            let action = menu
                .item_attribute_value(i, "action", None)
                .and_then(|v| v.get::<String>())
                .and_then(|name| {
                    name.strip_prefix("win.")
                        .and_then(|name| window.lookup_action(name))
                });
            let target = menu.item_attribute_value(i, "target", None);
            button.set_sensitive(action.as_ref().is_some_and(|a| a.is_enabled()));
            if let Some(action) = action.as_ref() {
                action
                    .bind_property("enabled", &button, "sensitive")
                    .sync_create()
                    .build();
            }
            button.connect_clicked(glib::clone!(
                #[weak]
                popover,
                move |_| {
                    let Some(action) = action.clone().filter(|a| a.is_enabled()) else {
                        return;
                    };
                    let target = target.clone();
                    // Closing unparents the popup. Keep the resolved action and
                    // dispatch after dismissal, rather than resolving win.* on
                    // a button that has already lost its window ancestry.
                    popover.popdown();
                    glib::idle_add_local_once(move || action.activate(target.as_ref()));
                }
            ));
            rows.append(&button);
        }
        rows
    }
    let popover = gtk::Popover::new();
    // Avoid a compositor-level pointer grab: on Niri, removing a favorite's
    // anchor while dismissing a grabbed popup can leave pointer motion confined.
    // The window handles outside clicks, focus loss and Escape explicitly.
    popover.set_autohide(false);
    let rows = content(menu, window, &popover);
    let scroll = gtk::ScrolledWindow::builder()
        .hscrollbar_policy(gtk::PolicyType::Never)
        .propagate_natural_height(true)
        .max_content_height(max_height)
        .min_content_width(230)
        .child(&rows)
        .build();
    popover.set_child(Some(&scroll));
    let keys = gtk::EventControllerKey::new();
    keys.connect_key_pressed(glib::clone!(
        #[weak]
        popover,
        #[upgrade_or]
        glib::Propagation::Proceed,
        move |_, key, _, _| {
            match key {
                gdk::Key::Down => {
                    popover.child_focus(gtk::DirectionType::TabForward);
                    glib::Propagation::Stop
                }
                gdk::Key::Up => {
                    popover.child_focus(gtk::DirectionType::TabBackward);
                    glib::Propagation::Stop
                }
                _ => glib::Propagation::Proceed,
            }
        }
    ));
    popover.add_controller(keys);
    popover
}
