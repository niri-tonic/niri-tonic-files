use super::Ui;
use crate::{
    connector::{Connector, Context, Entry, Kind, Location},
    locations as local,
};
use gtk::{gdk, gio, glib, prelude::*};
use notify::Watcher;
use std::{
    cell::RefCell,
    collections::HashMap,
    path::PathBuf,
    rc::Rc,
    sync::{
        Arc,
        atomic::{AtomicBool, Ordering},
    },
    time::{Duration, Instant},
};

#[derive(Clone)]
pub(super) struct RenameDraft {
    pub column: usize,
    pub location: Location,
    pub original: String,
    pub text: String,
}

pub(super) struct Column {
    pub path: PathBuf,
    pub generation: u64,
    pub slot: gtk::Stack,
    pub root: gtk::Box,
    pub model: gio::ListStore,
    pub selection: gtk::MultiSelection,
    pub list: gtk::ListView,
    pub hint: gtk::Label,
    pub context: Context,
    pub anchor: RefCell<Option<Location>>,
    pub editors: Rc<RefCell<HashMap<Location, glib::WeakRef<gtk::Stack>>>>,
    pub refresh: Arc<AtomicBool>,
}

impl Ui {
    pub(super) fn rebuild(self: &Rc<Self>, animate: bool) {
        self.dismiss_menu();
        self.cancel_rename();
        self.generation.set(self.generation.get() + 1);
        let generation = self.generation.get();
        let snapshot = self.nav.borrow().snapshot().clone();
        let old_columns = self.columns.take();
        if old_columns.is_empty() {
            while let Some(child) = self.panes.first_child() {
                self.panes.remove(&child);
            }
        }
        let mut old_columns = old_columns.into_iter().map(Some).collect::<Vec<_>>();
        let config = self.config.borrow();
        let animations = animate
            && config.navigation.animations
            && gtk::Settings::default().is_none_or(|s| s.is_gtk_enable_animations());
        let duration = config.navigation.animation_ms;
        drop(config);
        let mut columns = Vec::new();
        let mut load = Vec::new();
        for (index, path) in snapshot.columns.iter().enumerate() {
            let old = old_columns.get_mut(index).and_then(Option::take);
            if animate && old.as_ref().is_some_and(|c| c.path == *path) {
                // Keep the other pane's selection, scroll position and live watch.
                columns.push(old.expect("existing pane"));
                continue;
            }
            let slot = if let Some(old) = old {
                old.context.cancel();
                old.slot
            } else {
                let slot = gtk::Stack::new();
                slot.set_hexpand(true);
                slot.set_vexpand(true);
                slot.set_hhomogeneous(false);
                slot.set_overflow(gtk::Overflow::Hidden);
                self.panes.append(&slot);
                slot
            };
            let previous = slot.visible_child();
            slot.set_transition_duration(duration);
            slot.set_transition_type(if animations {
                if index == 0 {
                    gtk::StackTransitionType::SlideRight
                } else {
                    gtk::StackTransitionType::SlideLeft
                }
            } else {
                gtk::StackTransitionType::None
            });
            let column = self.make_column(index, path.clone(), generation, slot.clone());
            slot.add_named(&column.root, Some(&generation.to_string()));
            slot.set_visible_child(&column.root);
            if let Some(previous) = previous {
                glib::timeout_add_local_once(
                    Duration::from_millis(duration as u64 + 50),
                    move || {
                        if previous.parent().is_some() {
                            slot.remove(&previous);
                        }
                    },
                );
            }
            columns.push(column);
            load.push((index, path.clone()));
        }
        for old in old_columns.into_iter().flatten() {
            old.context.cancel();
            self.panes.remove(&old.slot);
        }
        if snapshot.columns.is_empty() && self.panes.first_child().is_none() {
            let empty = gtk::Label::new(Some("Choose Home or a favorite to start exploring"));
            empty.add_css_class("muted");
            self.panes.append(&empty);
        }
        self.columns.replace(columns);
        self.activate_column(snapshot.active);
        if let Some(column) = self.columns.borrow().get(snapshot.active) {
            column.list.grab_focus();
        }
        for (index, path) in load {
            self.watch_column(index, path, generation);
        }
    }

    fn make_column(
        self: &Rc<Self>,
        index: usize,
        path: PathBuf,
        generation: u64,
        slot: gtk::Stack,
    ) -> Column {
        let root = gtk::Box::new(gtk::Orientation::Vertical, 0);
        root.add_css_class("column");
        root.set_hexpand(true);
        root.set_vexpand(true);
        root.set_overflow(gtk::Overflow::Hidden);
        let body = gtk::Box::new(gtk::Orientation::Vertical, 0);
        let header = gtk::Box::new(gtk::Orientation::Horizontal, 4);
        header.add_css_class("column-header");
        let title = gtk::Label::new(Some(&self.folder_title(&path)));
        title.add_css_class("column-title");
        title.set_hexpand(true);
        title.set_xalign(0.0);
        title.set_ellipsize(gtk::pango::EllipsizeMode::Middle);
        title.set_tooltip_text(Some(&self.display_path(&path)));
        header.append(&title);
        let close = gtk::Button::from_icon_name("window-close-symbolic");
        close.add_css_class("close-control");
        close.set_tooltip_text(Some("Close column"));
        close.update_property(&[gtk::accessible::Property::Label("Close column")]);
        close.connect_clicked(glib::clone!(
            #[weak(rename_to = ui)]
            self,
            move |_| {
                if ui.nav.borrow_mut().close(index) {
                    ui.rebuild(true);
                }
            }
        ));
        header.append(&close);
        body.append(&header);
        let model = gio::ListStore::new::<glib::BoxedAnyObject>();
        let selection = gtk::MultiSelection::new(Some(model.clone()));
        let editors: Rc<RefCell<HashMap<Location, glib::WeakRef<gtk::Stack>>>> = Rc::default();
        let factory = gtk::SignalListItemFactory::new();
        let metadata_width = gtk::SizeGroup::new(gtk::SizeGroupMode::Horizontal);
        let realized: Rc<RefCell<Vec<glib::WeakRef<gtk::ListItem>>>> = Rc::default();
        factory.connect_setup(glib::clone!(
            #[strong]
            metadata_width,
            #[strong]
            realized,
            #[weak(rename_to = ui)]
            self,
            move |_, object| {
                let item = object.downcast_ref::<gtk::ListItem>().expect("ListItem");
                realized.borrow_mut().push(item.downgrade());
                // Selection and activation are handled below so Control-click
                // never navigates and Shift alone never selects a range.
                item.set_selectable(false);
                item.set_activatable(false);
                item.set_focusable(true);
                let row = gtk::Box::new(gtk::Orientation::Horizontal, 10);
                row.add_css_class("file-row");
                let icon = gtk::Image::new();
                icon.add_css_class("file-icon");
                icon.set_pixel_size(20);
                let name = gtk::Label::new(None);
                name.set_hexpand(true);
                name.set_xalign(0.0);
                name.set_ellipsize(gtk::pango::EllipsizeMode::Middle);
                let names = gtk::Stack::new();
                names.set_hexpand(true);
                names.set_hhomogeneous(false);
                names.add_named(&name, Some("label"));
                let edit = gtk::Entry::new();
                edit.add_css_class("rename-entry");
                edit.update_property(&[gtk::accessible::Property::Label("Rename item")]);
                names.add_named(&edit, Some("edit"));
                names.set_visible_child_name("label");
                edit.connect_activate(glib::clone!(
                    #[weak]
                    ui,
                    move |entry| ui.commit_rename(entry.text().to_string())
                ));
                edit.connect_changed(glib::clone!(
                    #[weak]
                    ui,
                    #[weak]
                    item,
                    move |entry| {
                        let Some(current) = entry_of(&item) else {
                            return;
                        };
                        if let Some(draft) = ui.rename.borrow_mut().as_mut()
                            && draft.location == current.location
                        {
                            draft.text = entry.text().to_string();
                        }
                    }
                ));
                let edit_keys = gtk::EventControllerKey::new();
                edit_keys.connect_key_pressed(glib::clone!(
                    #[weak]
                    ui,
                    #[upgrade_or]
                    glib::Propagation::Proceed,
                    move |_, key, _, modifiers| {
                        if key == gdk::Key::Escape
                            && !modifiers.contains(gdk::ModifierType::CONTROL_MASK)
                        {
                            ui.cancel_rename();
                            return glib::Propagation::Stop;
                        }
                        glib::Propagation::Proceed
                    }
                ));
                edit.add_controller(edit_keys);
                let detail = gtk::Label::new(None);
                detail.add_css_class("muted");
                detail.add_css_class("file-details");
                detail.set_xalign(1.0);
                detail.set_single_line_mode(true);
                detail.set_valign(gtk::Align::Fill);
                metadata_width.add_widget(&detail);
                row.append(&icon);
                row.append(&names);
                row.append(&detail);
                item.set_child(Some(&row));
                let click = gtk::GestureClick::new();
                click.set_button(3);
                click.connect_pressed(glib::clone!(
                    #[weak]
                    ui,
                    #[weak]
                    item,
                    #[weak]
                    row,
                    move |gesture, _, x, y| {
                        ui.activate_column(index);
                        if let Some(column) = ui.columns.borrow().get(index)
                            && !column.selection.is_selected(item.position())
                        {
                            column.selection.select_item(item.position(), true);
                        }
                        gesture.set_state(gtk::EventSequenceState::Claimed);
                        ui.context_menu(&row, x, y);
                    }
                ));
                row.add_controller(click);
                let left = gtk::GestureClick::new();
                left.set_button(1);
                left.connect_released(glib::clone!(
                    #[weak]
                    ui,
                    #[weak]
                    item,
                    #[weak]
                    names,
                    move |gesture, presses, _, _| {
                        if names.visible_child_name().as_deref() == Some("edit") {
                            return;
                        }
                        gesture.set_state(gtk::EventSequenceState::Claimed);
                        ui.row_clicked(
                            index,
                            item.position(),
                            presses,
                            gesture.current_event_state(),
                        );
                    }
                ));
                row.add_controller(left);
                let source = gtk::DragSource::builder()
                    .actions(gdk::DragAction::COPY)
                    .build();
                source.connect_prepare(glib::clone!(
                    #[weak]
                    ui,
                    #[weak]
                    item,
                    #[upgrade_or]
                    None,
                    move |_, _, _| {
                        ui.activate_column(index);
                        if let Some(column) = ui.columns.borrow().get(index)
                            && !column.selection.is_selected(item.position())
                        {
                            column.selection.select_item(item.position(), true);
                        }
                        let files: Vec<_> = ui
                            .selected_paths()
                            .iter()
                            .map(|p| {
                                if local::is_remote(p) {
                                    gio::File::for_uri(&ui.display_path(p))
                                } else {
                                    gio::File::for_path(p)
                                }
                            })
                            .collect();
                        Some(gdk::ContentProvider::for_value(
                            &gdk::FileList::from_array(&files).to_value(),
                        ))
                    }
                ));
                row.add_controller(source);
                let drop =
                    gtk::DropTarget::new(gdk::FileList::static_type(), gdk::DragAction::COPY);
                drop.connect_drop(glib::clone!(
                    #[weak]
                    ui,
                    #[weak]
                    item,
                    #[upgrade_or]
                    false,
                    move |_, value, _, _| {
                        let Some(entry) = entry_of(&item) else {
                            return false;
                        };
                        if !entry.navigable {
                            return false;
                        }
                        let Ok(dest) = local::path(&entry.location) else {
                            return false;
                        };
                        ui.accept_drop(value, dest)
                    }
                ));
                row.add_controller(drop);
            }
        ));
        factory.connect_bind(glib::clone!(
            #[strong]
            selection,
            #[weak(rename_to=ui)]
            self,
            #[strong]
            editors,
            move |_, object| {
                let item = object.downcast_ref::<gtk::ListItem>().expect("ListItem");
                let pending_item = item.downgrade();
                let pending_selection = selection.downgrade();
                glib::idle_add_local_once(move || {
                    if let (Some(item), Some(selection)) =
                        (pending_item.upgrade(), pending_selection.upgrade())
                    {
                        style_selection(&item, &selection);
                    }
                });
                let Some(entry) = entry_of(item) else {
                    return;
                };
                item.set_accessible_label(&entry.name);
                item.set_accessible_description(match entry.kind {
                    Kind::Directory => "Folder",
                    Kind::File => "File",
                    Kind::Symlink => "Symbolic link",
                    Kind::Other => "Special file",
                });
                let row = item.child().and_downcast::<gtk::Box>().expect("row");
                let icon = row
                    .first_child()
                    .and_downcast::<gtk::Image>()
                    .expect("icon");
                let names = icon
                    .next_sibling()
                    .and_downcast::<gtk::Stack>()
                    .expect("names");
                let name = names
                    .child_by_name("label")
                    .and_downcast::<gtk::Label>()
                    .expect("name");
                let edit = names
                    .child_by_name("edit")
                    .and_downcast::<gtk::Entry>()
                    .expect("edit");
                let detail = names
                    .next_sibling()
                    .and_downcast::<gtk::Label>()
                    .expect("detail");
                icon.set_icon_name(Some(match entry.kind {
                    Kind::Directory => "folder-symbolic",
                    Kind::Symlink => "emblem-symbolic-link-symbolic",
                    Kind::File => "text-x-generic-symbolic",
                    Kind::Other => "dialog-warning-symbolic",
                }));
                name.set_text(&entry.name);
                editors
                    .borrow_mut()
                    .insert(entry.location.clone(), names.downgrade());
                let draft = ui.rename.borrow().clone();
                if let Some(draft) =
                    draft.filter(|d| d.location == entry.location && d.column == index)
                {
                    edit.set_text(&draft.text);
                    names.set_visible_child_name("edit");
                    glib::idle_add_local_once(move || {
                        edit.grab_focus();
                    });
                } else {
                    names.set_visible_child_name("label");
                }
                name.set_tooltip_text(Some(&entry.name));
                let modified = entry
                    .modified
                    .and_then(|value| i64::try_from(value).ok())
                    .and_then(|value| glib::DateTime::from_unix_local(value).ok())
                    .and_then(|value| value.format("%Y-%m-%d %H:%M").ok())
                    .map(|value| value.to_string())
                    .unwrap_or_else(|| "—".into());
                detail.set_text(&format!("{modified}  ·  {}", glib::format_size(entry.size)));
                detail.set_tooltip_text(Some(if entry.navigable {
                    "Last modified · directory entry size (not recursive)"
                } else {
                    "Last modified · size"
                }));
                row.set_tooltip_text(Some(&format!(
                    "{}\n{} bytes{}",
                    entry.name,
                    entry.size,
                    if entry.kind == Kind::Symlink {
                        " · symbolic link"
                    } else {
                        ""
                    }
                )));
            }
        ));
        factory.connect_unbind(glib::clone!(
            #[strong]
            editors,
            move |_, object| {
                if let Some(item) = object.downcast_ref::<gtk::ListItem>()
                    && let Some(entry) = entry_of(item)
                {
                    let names = item
                        .child()
                        .and_then(|row| row.first_child())
                        .and_then(|icon| icon.next_sibling())
                        .and_downcast::<gtk::Stack>();
                    let mut editors = editors.borrow_mut();
                    // GTK may bind a replacement row before unbinding the
                    // old row for the same location during a model splice.
                    if editors
                        .get(&entry.location)
                        .and_then(glib::WeakRef::upgrade)
                        == names
                    {
                        editors.remove(&entry.location);
                    }
                }
            }
        ));
        let list = gtk::ListView::new(Some(selection.clone()), Some(factory));
        list.set_single_click_activate(false);
        list.update_property(&[gtk::accessible::Property::Label(&format!(
            "Files in {}",
            self.display_path(&path)
        ))]);
        selection.connect_selection_changed(glib::clone!(
            #[strong]
            realized,
            #[weak(rename_to = ui)]
            self,
            move |selection, _, _| {
                let pending = realized.clone();
                let selection = selection.downgrade();
                glib::idle_add_local_once(move || {
                    if let Some(selection) = selection.upgrade() {
                        pending.borrow_mut().retain(|weak| {
                            if let Some(item) = weak.upgrade() {
                                style_selection(&item, &selection);
                                true
                            } else {
                                false
                            }
                        });
                    }
                });
                ui.update_actions();
            }
        ));
        let focus = gtk::EventControllerFocus::new();
        focus.connect_enter(glib::clone!(
            #[weak(rename_to = ui)]
            self,
            move |_| {
                ui.activate_column(index);
            }
        ));
        list.add_controller(focus);
        let left = gtk::GestureClick::new();
        left.set_button(1);
        left.set_propagation_phase(gtk::PropagationPhase::Capture);
        left.connect_pressed(glib::clone!(
            #[weak(rename_to = ui)]
            self,
            move |_, _, _, _| {
                ui.activate_column(index);
            }
        ));
        root.add_controller(left);
        let scroll = gtk::ScrolledWindow::builder()
            .hscrollbar_policy(gtk::PolicyType::Never)
            .vexpand(true)
            .child(&list)
            .build();
        body.append(&scroll);
        let hint = gtk::Label::new(Some("Loading…"));
        hint.add_css_class("status");
        hint.set_xalign(0.0);
        body.append(&hint);
        // The metadata background spans the header, list and empty space.
        let band = gtk::Box::new(gtk::Orientation::Vertical, 0);
        band.add_css_class("metadata-band");
        band.set_halign(gtk::Align::End);
        band.set_can_target(false);
        metadata_width.add_widget(&band);
        let content = gtk::Overlay::new();
        content.set_vexpand(true);
        content.set_child(Some(&band));
        content.add_overlay(&body);
        content.set_measure_overlay(&body, true);
        content.set_overflow(gtk::Overflow::Hidden);
        root.append(&content);
        let blank = gtk::GestureClick::new();
        blank.set_button(3);
        blank.connect_pressed(glib::clone!(
            #[weak(rename_to = ui)]
            self,
            #[weak]
            root,
            move |gesture, _, x, y| {
                if gesture.is_recognized() {
                    ui.activate_column(index);
                    if let Some(column) = ui.columns.borrow().get(index) {
                        column.selection.unselect_all();
                    }
                    ui.context_menu(&root, x, y);
                }
            }
        ));
        root.add_controller(blank);
        let drop = gtk::DropTarget::new(gdk::FileList::static_type(), gdk::DragAction::COPY);
        drop.connect_drop(glib::clone!(
            #[weak(rename_to = ui)]
            self,
            #[strong]
            path,
            #[upgrade_or]
            false,
            move |_, value, _, _| ui.accept_drop(value, path.clone())
        ));
        root.add_controller(drop);
        Column {
            path,
            generation,
            slot,
            root,
            model,
            selection,
            list,
            hint,
            context: Context::default(),
            anchor: RefCell::new(None),
            editors,
            refresh: Arc::new(AtomicBool::new(false)),
        }
    }

    fn watch_column(self: &Rc<Self>, index: usize, path: PathBuf, generation: u64) {
        let context = self.columns.borrow()[index].context.clone();
        let refresh = self.columns.borrow()[index].refresh.clone();
        let connector = self.connector.clone();
        let view = self.config.borrow().view.clone();
        let (send, recv) = async_channel::bounded(2);
        std::thread::spawn(move || {
            let (changed, events) = std::sync::mpsc::sync_channel(1);
            let _keepalive = changed.clone();
            let remote = local::is_remote(&path);
            let watcher = if remote {
                None
            } else {
                notify::recommended_watcher(move |event: notify::Result<notify::Event>| {
                    if !event
                        .as_ref()
                        .is_ok_and(|e| matches!(e.kind, notify::EventKind::Access(_)))
                    {
                        let _ = changed.try_send(());
                    }
                })
                .ok()
            };
            let mut watcher = watcher;
            let watch_ok = watcher
                .as_mut()
                .is_some_and(|w| w.watch(&path, notify::RecursiveMode::NonRecursive).is_ok());
            loop {
                if context.check().is_err() {
                    break;
                }
                let result =
                    connector
                        .list(&local::location(&path), None, &context)
                        .map(|mut page| {
                            page.entries.retain(|e| view.hidden || !e.hidden);
                            local::sort(&mut page.entries, &view);
                            page.entries
                        });
                let authentication_failed = result.as_ref().err().is_some_and(|error| {
                    matches!(
                        error.code,
                        crate::connector::ErrorCode::AuthenticationRequired
                            | crate::connector::ErrorCode::Cancelled
                    )
                });
                if send.send_blocking((result, watch_ok)).is_err() {
                    break;
                }
                let checked = Instant::now();
                // Coalesce change bursts and explicit refreshes after inline editing.
                loop {
                    if context.check().is_err() {
                        return;
                    }
                    if refresh.swap(false, Ordering::Relaxed) {
                        break;
                    }
                    if !watch_ok
                        && !authentication_failed
                        && checked.elapsed() >= Duration::from_secs(15)
                    {
                        break;
                    }
                    match events.recv_timeout(Duration::from_millis(180)) {
                        Ok(()) => {
                            while events.try_recv().is_ok() {}
                            break;
                        }
                        Err(std::sync::mpsc::RecvTimeoutError::Timeout) => {}
                        Err(_) => return,
                    }
                }
            }
        });
        let weak = Rc::downgrade(self);
        glib::spawn_future_local(async move {
            while let Ok((result, _watch_ok)) = recv.recv().await {
                let Some(ui) = weak.upgrade() else {
                    break;
                };
                if ui.closing.get() {
                    break;
                }
                let columns = ui.columns.borrow();
                let Some(column) = columns.get(index) else {
                    break;
                };
                if column.generation != generation {
                    break;
                }
                // Do not replace the editor while its text is being changed.
                // Completing/cancelling the edit requests a fresh listing.
                if ui
                    .rename
                    .borrow()
                    .as_ref()
                    .is_some_and(|d| d.column == index)
                {
                    continue;
                }
                match result {
                    Ok(entries) => {
                        let selected: Vec<_> = (0..column.model.n_items())
                            .filter(|i| column.selection.is_selected(*i))
                            .filter_map(|i| model_entry(&column.model, i).map(|e| e.location))
                            .collect();
                        let objects: Vec<_> =
                            entries.into_iter().map(glib::BoxedAnyObject::new).collect();
                        column.model.splice(0, column.model.n_items(), &objects);
                        column.selection.unselect_all();
                        for i in 0..column.model.n_items() {
                            if model_entry(&column.model, i)
                                .is_some_and(|e| selected.contains(&e.location))
                            {
                                column.selection.select_item(i, false);
                            }
                        }
                        column.hint.set_text("");
                        column.hint.remove_css_class("error");
                        column.hint.set_visible(false);
                    }
                    Err(e) => {
                        column.hint.set_visible(true);
                        column.model.remove_all();
                        column.hint.set_text(&e.to_string());
                        column.hint.set_wrap(true);
                        column.hint.add_css_class("error");
                    }
                }
            }
        });
    }

    pub(super) fn row_clicked(
        self: &Rc<Self>,
        index: usize,
        position: u32,
        presses: i32,
        modifiers: gdk::ModifierType,
    ) {
        let entry = self
            .columns
            .borrow()
            .get(index)
            .and_then(|c| model_entry(&c.model, position));
        let Some(entry) = entry else {
            return;
        };
        let control = modifiers.contains(gdk::ModifierType::CONTROL_MASK);
        let range = control && modifiers.contains(gdk::ModifierType::SHIFT_MASK);
        if entry.navigable && presses > 1 && !control {
            return;
        }
        self.cancel_rename();
        self.activate_column(index);
        {
            let columns = self.columns.borrow();
            let column = &columns[index];
            column.list.grab_focus();
            if range {
                let anchor = column
                    .anchor
                    .borrow()
                    .as_ref()
                    .and_then(|anchor| {
                        (0..column.model.n_items()).find(|i| {
                            model_entry(&column.model, *i).is_some_and(|e| e.location == *anchor)
                        })
                    })
                    .unwrap_or(position);
                column.selection.select_range(
                    anchor.min(position),
                    anchor.abs_diff(position) + 1,
                    false,
                );
            } else if control {
                if column.selection.is_selected(position) {
                    column.selection.unselect_item(position);
                } else {
                    column.selection.select_item(position, false);
                }
                column.anchor.replace(Some(entry.location.clone()));
            } else {
                column.selection.select_item(position, true);
                column.anchor.replace(Some(entry.location.clone()));
            }
        }
        if control {
            return;
        }
        if entry.navigable && presses == 1 {
            if let Ok(path) = local::path(&entry.location) {
                self.navigate(path);
            }
        } else if !entry.navigable && presses == 2 {
            self.open_selected();
        }
    }

    pub(super) fn selected_entries(&self) -> Vec<Entry> {
        let index = self.nav.borrow().snapshot().active;
        self.columns
            .borrow()
            .get(index)
            .map(|column| {
                (0..column.model.n_items())
                    .filter(|i| column.selection.is_selected(*i))
                    .filter_map(|i| model_entry(&column.model, i))
                    .collect()
            })
            .unwrap_or_default()
    }
    pub(super) fn selected_paths(&self) -> Vec<PathBuf> {
        self.selected_entries()
            .iter()
            .filter_map(|e| local::path(&e.location).ok())
            .collect()
    }
    fn accept_drop(self: &Rc<Self>, value: &glib::Value, dest: PathBuf) -> bool {
        if self.job.borrow().is_some()
            || self
                .connector
                .capabilities_at(&local::location(&dest))
                .read_only()
        {
            self.message("Cannot accept a drop while busy or in read-only mode", true);
            return false;
        }
        let Ok(files) = value.get::<gdk::FileList>() else {
            return false;
        };
        let files = files.files();
        let paths: Vec<_> = files
            .iter()
            .filter_map(|file| self.parse_path(file.uri().as_str()).ok())
            .filter(|path| path.is_absolute())
            .collect();
        if paths.is_empty() || paths.len() != files.len() {
            self.message(
                "Only local files and configured connector locations can be dropped here",
                true,
            );
            return false;
        }
        self.transfer(paths, dest, false);
        true
    }
}

fn style_selection(item: &gtk::ListItem, selection: &gtk::MultiSelection) {
    let Some(row) = item.child().and_then(|child| child.parent()) else {
        return;
    };
    let position = item.position();
    let selected = position != gtk::INVALID_LIST_POSITION && selection.is_selected(position);
    let first = selected && (position == 0 || !selection.is_selected(position - 1));
    let last =
        selected && (position + 1 >= selection.n_items() || !selection.is_selected(position + 1));
    for (class, enabled) in [("selection-start", first), ("selection-end", last)] {
        if enabled {
            row.add_css_class(class);
        } else {
            row.remove_css_class(class);
        }
    }
}

fn entry_of(item: &gtk::ListItem) -> Option<Entry> {
    item.item()?
        .downcast::<glib::BoxedAnyObject>()
        .ok()
        .map(|item| item.borrow::<Entry>().clone())
}
pub(super) fn model_entry(model: &gio::ListStore, index: u32) -> Option<Entry> {
    model
        .item(index)?
        .downcast::<glib::BoxedAnyObject>()
        .ok()
        .map(|item| item.borrow::<Entry>().clone())
}
