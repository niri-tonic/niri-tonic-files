mod actions;
mod columns;
mod dialogs;
mod rename;
mod shortcuts;
mod smoke;

use crate::{
    config::{Config, Paths, Session},
    connector::{Connector, Context},
    locations,
    navigation::Navigation,
    storage::Registry,
    theme::{Fonts, Theme},
};
use gtk::{glib, prelude::*};
use std::{
    cell::{Cell, RefCell},
    path::{Path, PathBuf},
    rc::Rc,
    sync::Arc,
};

fn item_count(count: usize) -> String {
    format!("{count} {}", if count == 1 { "item" } else { "items" })
}

pub struct Ui {
    window: gtk::ApplicationWindow,
    config: RefCell<Config>,
    paths: Paths,
    nav: RefCell<Navigation>,
    connector: Arc<Registry>,
    panes: gtk::Box,
    sidebar: gtk::Box,
    path_entry: gtk::Entry,
    status: gtk::Label,
    progress: gtk::ProgressBar,
    cancel: gtk::Button,
    jobbar: gtk::Box,
    rename: RefCell<Option<columns::RenameDraft>>,
    menu: RefCell<Option<gtk::Popover>>,
    columns: RefCell<Vec<columns::Column>>,
    generation: Cell<u64>,
    navigation_request: Cell<u64>,
    job: RefCell<Option<Context>>,
    smoke: bool,
    closing: Cell<bool>,
    saving_favorites: Cell<bool>,
    session_save: RefCell<Option<std::thread::JoinHandle<()>>>,
}

impl Ui {
    pub fn build(
        app: &gtk::Application,
        config: Config,
        paths: Paths,
        visuals: (Theme, Fonts),
        root: PathBuf,
        smoke: bool,
        connector: Arc<Registry>,
    ) -> Rc<Self> {
        let (theme, fonts) = visuals;
        let display = gtk::gdk::Display::default().expect("GTK display initialized");
        let css = gtk::CssProvider::new();
        css.load_from_string(&theme.css(&fonts));
        gtk::style_context_add_provider_for_display(
            &display,
            &css,
            gtk::STYLE_PROVIDER_PRIORITY_APPLICATION,
        );
        let window = gtk::ApplicationWindow::builder()
            .application(app)
            .title("Niri Tonic Files")
            .default_width(1120)
            .default_height(720)
            .decorated(false)
            .build();
        window.add_css_class("tonic");
        let layout = gtk::Box::new(gtk::Orientation::Vertical, theme.metrics.spacing as i32);
        layout.add_css_class("app-content");
        let workspace = gtk::Grid::builder()
            .column_homogeneous(true)
            .column_spacing(theme.metrics.spacing as i32)
            .build();
        workspace.add_css_class("workspace");
        workspace.set_vexpand(true);
        let panes = gtk::Box::new(gtk::Orientation::Horizontal, theme.metrics.spacing as i32);
        panes.set_hexpand(true);
        panes.set_vexpand(true);
        panes.set_homogeneous(true);
        workspace.attach(&panes, 0, 0, 2, 1);
        let sidebar = gtk::Box::new(gtk::Orientation::Vertical, 4);
        sidebar.add_css_class("sidebar");
        let sidebar_scroll = gtk::ScrolledWindow::builder()
            .hscrollbar_policy(gtk::PolicyType::Never)
            .hexpand(true)
            .child(&sidebar)
            .build();
        sidebar_scroll.add_css_class("sidebar-scroll");
        workspace.attach(&sidebar_scroll, 2, 0, 1, 1);
        layout.append(&workspace);
        let jobbar = gtk::Box::new(gtk::Orientation::Horizontal, 8);
        jobbar.set_visible(false);
        let status = gtk::Label::new(None);
        status.set_visible(false);
        status.add_css_class("status");
        status.set_xalign(0.0);
        status.set_hexpand(true);
        status.set_ellipsize(gtk::pango::EllipsizeMode::End);
        jobbar.append(&status);
        let progress = gtk::ProgressBar::new();
        progress.set_visible(false);
        progress.set_width_request(140);
        let cancel = gtk::Button::with_label("Cancel");
        cancel.set_visible(false);
        jobbar.append(&progress);
        jobbar.append(&cancel);
        layout.append(&jobbar);
        let pathbar = gtk::Box::new(gtk::Orientation::Horizontal, 8);
        pathbar.add_css_class("pathbar");
        let path_entry = gtk::Entry::builder()
            .hexpand(true)
            .placeholder_text("Enter a folder path")
            .build();
        path_entry.add_css_class("path");
        path_entry.update_property(&[gtk::accessible::Property::Label("Active folder path")]);
        pathbar.append(&path_entry);
        layout.append(&pathbar);
        window.set_child(Some(&layout));
        let ui = Rc::new(Self {
            window,
            config: RefCell::new(config),
            paths,
            nav: RefCell::new(Navigation::new(root)),
            connector,
            panes,
            sidebar,
            path_entry,
            status,
            progress,
            cancel,
            jobbar,
            rename: RefCell::new(None),
            menu: RefCell::new(None),
            columns: RefCell::new(vec![]),
            generation: Cell::new(0),
            navigation_request: Cell::new(0),
            job: RefCell::new(None),
            smoke,
            closing: Cell::new(false),
            saving_favorites: Cell::new(false),
            session_save: RefCell::new(None),
        });
        ui.cancel.connect_clicked(glib::clone!(
            #[weak]
            ui,
            move |_| if let Some(ctx) = ui.job.borrow().as_ref() {
                ctx.cancel();
            }
        ));
        ui.path_entry.connect_activate(glib::clone!(
            #[weak]
            ui,
            move |entry| {
                let text = entry.text();
                if let Ok(path) = ui.parse_path(&text) {
                    let path = ui.paths.expand(&path);
                    let path = if path.is_absolute() {
                        path
                    } else {
                        ui.active_path()
                            .unwrap_or_else(|| ui.paths.home.clone())
                            .join(path)
                    };
                    ui.navigate(path);
                } else {
                    ui.message(
                        "Enter a local folder path, filen://account/folder or drive://account/folder URI",
                        true,
                    );
                }
            }
        ));
        ui.window.connect_close_request(glib::clone!(
            #[weak]
            ui,
            #[upgrade_or]
            glib::Propagation::Proceed,
            move |_| {
                if ui.saving_favorites.get() {
                    ui.message(
                        "Favorites are being saved. Please wait before closing.",
                        false,
                    );
                    return glib::Propagation::Stop;
                }
                if ui.job.borrow().is_some() {
                    ui.message(
                        "An operation is running. Cancel it or wait before closing.",
                        false,
                    );
                    return glib::Propagation::Stop;
                }
                ui.closing.set(true);
                ui.dismiss_menu();
                for column in ui.columns.borrow().iter() {
                    column.context.cancel();
                }
                ui.save_session();
                glib::Propagation::Proceed
            }
        ));
        let outside_click = gtk::GestureClick::new();
        outside_click.set_button(0);
        outside_click.set_propagation_phase(gtk::PropagationPhase::Capture);
        outside_click.connect_pressed(glib::clone!(
            #[weak]
            ui,
            move |gesture, _, x, y| {
                if gesture.current_event().and_then(|event| event.surface()) == ui.window.surface()
                {
                    ui.dismiss_menu();
                    if gesture.current_button() == 1 {
                        let mut target = ui.window.pick(x, y, gtk::PickFlags::DEFAULT);
                        let mut interactive = false;
                        while let Some(widget) = target {
                            if widget.has_css_class("file-row")
                                || widget.is::<gtk::Button>()
                                || widget.is::<gtk::Entry>()
                                || widget.is::<gtk::Popover>()
                                || widget.is::<gtk::Scrollbar>()
                            {
                                interactive = true;
                                break;
                            }
                            target = widget.parent();
                        }
                        if !interactive {
                            ui.cancel_rename();
                            for column in ui.columns.borrow().iter() {
                                column.selection.unselect_all();
                                column.anchor.replace(None);
                            }
                        }
                    }
                }
            }
        ));
        ui.window.add_controller(outside_click);
        ui.window.connect_is_active_notify(glib::clone!(
            #[weak]
            ui,
            move |window| {
                if !window.is_active() {
                    ui.dismiss_menu();
                }
            }
        ));
        ui.install_actions();
        ui.update_sidebar();
        ui.rebuild(false);
        ui
    }
    pub fn present(&self) {
        self.window.present();
    }
    /// Called after the GTK event loop exits, so the UI never waits for disk I/O.
    pub fn finish(&self) {
        if let Some(handle) = self.session_save.borrow_mut().take() {
            let _ = handle.join();
        }
    }
    pub fn restore(self: &Rc<Self>, session: &Session) {
        if session.version != 1 || session.columns.is_empty() || session.columns.len() > 2 {
            return;
        }
        let paths: Vec<_> = session
            .columns
            .iter()
            .filter_map(|uri| self.parse_path(uri).ok())
            .collect();
        self.nav.borrow_mut().restore(paths, session.active);
        self.rebuild(false);
    }
    fn active_path(&self) -> Option<PathBuf> {
        self.nav.borrow().active_path().map(Path::to_path_buf)
    }
    fn folder_title(&self, path: &Path) -> String {
        if locations::is_remote(path) && locations::parent(path).is_none() {
            let id = locations::location(path).connector;
            if let Some(instance) = self
                .config
                .borrow()
                .connectors
                .instances
                .iter()
                .find(|i| i.id == id)
            {
                return format!("{} · {}", instance.name, instance.kind.label());
            }
        }
        path.file_name()
            .unwrap_or(path.as_os_str())
            .to_string_lossy()
            .into_owned()
    }
    fn dismiss_menu(&self) {
        let menu = self.menu.take();
        if let Some(menu) = menu {
            menu.popdown();
        }
    }
    fn display_path(&self, path: &Path) -> String {
        self.config.borrow().display_path(path)
    }
    fn parse_path(&self, text: &str) -> crate::connector::Result<PathBuf> {
        self.config.borrow().parse_path(text)
    }
    fn refresh_panes(&self) {
        for column in self.columns.borrow().iter() {
            column
                .refresh
                .store(true, std::sync::atomic::Ordering::Relaxed);
        }
        self.update_actions();
    }
    fn message(&self, text: &str, error: bool) {
        let visible = error || self.job.borrow().is_some();
        self.jobbar.set_visible(visible);
        self.status.set_visible(visible && !text.is_empty());
        self.status.set_text(text);
        self.status.set_tooltip_text(Some(text));
        if error {
            self.status.add_css_class("error");
        } else {
            self.status.remove_css_class("error");
        }
    }
    fn navigate(self: &Rc<Self>, path: PathBuf) {
        if locations::is_remote(&path) {
            // The listing worker performs remote authentication and reports its
            // progress/error in the new pane; never probe a virtual path on disk.
            let retry = {
                let nav = self.nav.borrow();
                nav.snapshot().columns.len() == 2
                    && nav.snapshot().columns[1 - nav.snapshot().active] == path
            };
            self.nav.borrow_mut().open(path);
            self.rebuild(true);
            if retry {
                let active = self.nav.borrow().snapshot().active;
                if let Some(column) = self.columns.borrow().get(active) {
                    column
                        .refresh
                        .store(true, std::sync::atomic::Ordering::Relaxed);
                    column.hint.set_text("Loading…");
                    column.hint.remove_css_class("error");
                    column.hint.set_visible(true);
                }
            }
            return;
        }
        // Stat in a worker: typing a slow mount must not freeze the window.
        let connector = self.connector.clone();
        let weak = Rc::downgrade(self);
        let (send, recv) = async_channel::bounded(1);
        let generation = self.generation.get();
        let source = self.nav.borrow().snapshot().active;
        let request = self.navigation_request.get() + 1;
        self.navigation_request.set(request);
        std::thread::spawn(move || {
            let result = connector.stat(&locations::location(&path), &Context::default());
            let _ = send.send_blocking((path, result));
        });
        glib::spawn_future_local(async move {
            let Ok((path, result)) = recv.recv().await else {
                return;
            };
            let Some(ui) = weak.upgrade() else {
                return;
            };
            if ui.closing.get()
                || generation != ui.generation.get()
                || request != ui.navigation_request.get()
            {
                return;
            }
            match result {
                Ok(entry) if entry.navigable => {
                    {
                        let mut nav = ui.nav.borrow_mut();
                        nav.activate(source);
                        nav.open(path);
                    }
                    ui.rebuild(true);
                }
                Ok(_) => ui.message("This path is not a folder", true),
                Err(e) => ui.message(&e.to_string(), true),
            }
        });
    }
    fn activate_column(&self, index: usize) {
        self.nav.borrow_mut().activate(index);
        for (i, column) in self.columns.borrow().iter().enumerate() {
            if i == index {
                column.root.add_css_class("active");
            } else {
                column.root.remove_css_class("active");
            }
        }
        self.update_path();
        self.update_actions();
    }
    fn update_path(&self) {
        let path = self.active_path();
        // File URIs round-trip names that cannot be represented in UTF-8.
        let text = path
            .as_ref()
            .map(|p| self.display_path(p))
            .unwrap_or_default();
        self.path_entry.set_text(&text);
    }
    fn update_sidebar(self: &Rc<Self>) {
        while let Some(child) = self.sidebar.first_child() {
            self.sidebar.remove(&child);
        }
        let category = |text: &str| {
            let label = gtk::Label::new(Some(text));
            label.set_xalign(0.0);
            label.add_css_class("category");
            label
        };
        self.sidebar.append(&category("LOCAL"));
        self.sidebar.append(&self.location_button(
            "Home",
            "user-home-symbolic",
            self.paths.home.clone(),
        ));
        self.sidebar.append(&category("FAVORITES"));
        let favorites = self.config.borrow().favorites.clone();
        if favorites.is_empty() {
            let hint = gtk::Label::new(Some("Right-click a folder\nto add a favorite"));
            hint.add_css_class("muted");
            hint.set_xalign(0.0);
            self.sidebar.append(&hint);
        }
        for favorite in favorites {
            let path = self.paths.expand(&favorite.path);
            let button = self.location_button(&favorite.name, "starred-symbolic", path.clone());
            let menu = gtk::GestureClick::new();
            menu.set_button(3);
            menu.connect_pressed(glib::clone!(
                #[weak(rename_to=ui)]
                self,
                #[weak]
                button,
                move |gesture, _, x, y| {
                    gesture.set_state(gtk::EventSequenceState::Claimed);
                    ui.favorite_menu(&button, &path, x, y);
                }
            ));
            button.add_controller(menu);
            self.sidebar.append(&button);
        }
        let spacer = gtk::Box::new(gtk::Orientation::Vertical, 0);
        spacer.set_vexpand(true);
        self.sidebar.append(&spacer);
        self.sidebar.append(&category("CONNECTORS"));
        let note = gtk::Label::new(Some("Local storage connected"));
        note.add_css_class("muted");
        note.set_xalign(0.0);
        self.sidebar.append(&note);
        for instance in &self.config.borrow().connectors.instances {
            if !instance.enabled {
                continue;
            }
            self.sidebar.append(&self.place_button(
                &instance.name,
                "folder-remote-symbolic",
                locations::root(&instance.id),
                Some(instance.kind.label()),
            ));
        }
        for slot in &self.config.borrow().connectors.reserved {
            let label = gtk::Label::new(Some(&format!("{} · unavailable", slot.label)));
            label.add_css_class("muted");
            label.set_xalign(0.0);
            self.sidebar.append(&label);
        }
    }
    fn location_button(self: &Rc<Self>, label: &str, icon: &str, path: PathBuf) -> gtk::Button {
        self.place_button(label, icon, path, None)
    }
    fn place_button(
        self: &Rc<Self>,
        label: &str,
        icon: &str,
        path: PathBuf,
        kind: Option<&str>,
    ) -> gtk::Button {
        let row = gtk::Box::new(gtk::Orientation::Horizontal, 8);
        row.append(&gtk::Image::from_icon_name(icon));
        let text = gtk::Label::new(Some(label));
        text.set_ellipsize(gtk::pango::EllipsizeMode::End);
        text.set_xalign(0.0);
        text.set_hexpand(true);
        if let Some(kind) = kind {
            let labels = gtk::Box::new(gtk::Orientation::Vertical, 3);
            labels.append(&text);
            let detail = gtk::Label::new(Some(kind));
            detail.add_css_class("muted");
            detail.set_xalign(0.0);
            labels.append(&detail);
            row.append(&labels);
        } else {
            row.append(&text);
        }
        let button = gtk::Button::builder()
            .child(&row)
            .tooltip_text(self.display_path(&path))
            .build();
        let accessible = kind
            .map(|kind| format!("{label} · {kind}"))
            .unwrap_or_else(|| label.into());
        button.update_property(&[gtk::accessible::Property::Label(&accessible)]);
        button.connect_clicked(glib::clone!(
            #[weak(rename_to = ui)]
            self,
            move |_| {
                ui.navigate(path.clone());
            }
        ));
        button
    }
    fn save_session(&self) {
        if self.smoke || !self.config.borrow().navigation.restore_session {
            return;
        }
        let snapshot = self.nav.borrow().snapshot().clone();
        let session = Session {
            version: 1,
            columns: snapshot
                .columns
                .iter()
                .map(|p| self.display_path(p))
                .collect(),
            active: snapshot.active,
        };
        let path = self.paths.state.join("session.json");
        self.session_save.replace(Some(std::thread::spawn(move || {
            let result = serde_json::to_vec_pretty(&session)
                .map_err(|e| e.to_string())
                .and_then(|bytes| crate::config::atomic_write(&path, &bytes));
            if let Err(e) = result {
                eprintln!("Cannot save session: {e}");
            }
        })));
    }
}
