use super::Ui;
use crate::locations as local;
use gtk::{gio, glib, prelude::*};
use std::{cell::RefCell, rc::Rc};

impl Ui {
    pub(super) fn prompt(
        self: &Rc<Self>,
        title: &str,
        description: &str,
        initial: &str,
        accept: &str,
        callback: impl FnOnce(&Rc<Self>, String) + 'static,
    ) {
        let dialog = gtk::Window::builder()
            .title(title)
            .transient_for(&self.window)
            .modal(true)
            .default_width(440)
            .resizable(false)
            .decorated(false)
            .build();
        dialog.add_css_class("dialog-window");
        let layout = gtk::Box::new(gtk::Orientation::Vertical, 12);
        layout.add_css_class("dialog-body");
        let label = gtk::Label::new(Some(description));
        label.set_wrap(true);
        label.set_xalign(0.0);
        layout.append(&label);
        let entry = gtk::Entry::builder()
            .text(initial)
            .hexpand(true)
            .activates_default(true)
            .build();
        entry.update_property(&[gtk::accessible::Property::Label(description)]);
        layout.append(&entry);
        let actions = gtk::Box::new(gtk::Orientation::Horizontal, 8);
        actions.set_halign(gtk::Align::End);
        let cancel = gtk::Button::with_label("Cancel");
        let ok = gtk::Button::with_label(accept);
        ok.add_css_class("suggested-action");
        actions.append(&cancel);
        actions.append(&ok);
        layout.append(&actions);
        dialog.set_child(Some(&layout));
        dialog.set_default_widget(Some(&ok));
        cancel.connect_clicked(glib::clone!(
            #[weak]
            dialog,
            move |_| dialog.close()
        ));
        let callback = RefCell::new(Some(callback));
        ok.connect_clicked(glib::clone!(
            #[weak(rename_to = ui)]
            self,
            #[weak]
            dialog,
            #[weak]
            entry,
            move |_| {
                if let Some(callback) = callback.borrow_mut().take() {
                    let text = entry.text().to_string();
                    dialog.close();
                    callback(&ui, text);
                }
            }
        ));
        self.dialog_shortcuts(&dialog);
        dialog.present();
        entry.grab_focus();
        entry.select_region(0, -1);
    }

    pub(super) fn new_item(self: &Rc<Self>, directory: bool) {
        let Some(parent) = self.active_path() else {
            return;
        };
        self.prompt(
            if directory { "New Folder" } else { "New File" },
            "Name",
            "",
            "Create",
            move |ui, name| {
                if let Err(e) = local::valid_name(&name) {
                    ui.message(&e.to_string(), true);
                    return;
                }
                let target = local::location(&parent.join(&name));
                ui.run_job("Creating", true, move |connector, ctx| {
                    if directory {
                        connector.create_directory(&target, ctx)
                    } else {
                        connector.write_new(&target, &mut std::io::empty(), ctx)
                    }
                    .map_err(|e| e.to_string())?;
                    Ok(format!("Created {name}"))
                });
            },
        );
    }

    pub(super) fn move_dialog(self: &Rc<Self>) {
        let paths = self.selected_paths();
        if paths.is_empty() {
            return;
        }
        self.prompt(
            "Move To",
            "Destination folder",
            "",
            "Move",
            move |ui, text| {
                let destination = ui.expand_input(&text);
                ui.transfer(paths, destination, true);
            },
        );
    }

    pub(super) fn open_with(self: &Rc<Self>) {
        let paths = self.selected_paths();
        if paths.is_empty() {
            return;
        }
        let storage = self.connector.clone();
        let prepared = std::sync::Arc::new(std::sync::Mutex::new(Vec::new()));
        let output = prepared.clone();
        self.run_job_after(
            "Preparing files",
            false,
            move |_, context| {
                let files = paths
                    .iter()
                    .map(|path| {
                        storage
                            .materialize(&local::location(path), context)
                            .map_err(|e| e.to_string())
                    })
                    .collect::<Result<Vec<_>, _>>()?;
                *output.lock().map_err(|_| "Cannot prepare files")? = files;
                Ok("Files ready".into())
            },
            move |ui, success| {
                if success && let Ok(mut files) = prepared.lock() {
                    ui.choose_application(std::mem::take(&mut *files));
                }
            },
        );
    }

    fn choose_application(self: &Rc<Self>, paths: Vec<std::path::PathBuf>) {
        // GIO desktop application discovery can touch disk; load it off-thread.
        let (send, recv) = async_channel::bounded(1);
        std::thread::spawn(move || {
            let apps: Vec<_> = gio::AppInfo::all()
                .into_iter()
                .filter(|a| a.should_show() && a.supports_files())
                .filter_map(|a| {
                    a.id()
                        .map(|id| (a.display_name().to_string(), id.to_string()))
                })
                .collect();
            let _ = send.send_blocking(apps);
        });
        let weak = Rc::downgrade(self);
        glib::spawn_future_local(async move {
            let Ok(mut apps) = recv.recv().await else {
                return;
            };
            let Some(ui) = weak.upgrade() else {
                return;
            };
            apps.sort_by_key(|(name, _)| name.to_lowercase());
            let dialog = gtk::Window::builder()
                .title("Open With")
                .transient_for(&ui.window)
                .modal(true)
                .default_width(440)
                .default_height(500)
                .decorated(false)
                .build();
            dialog.add_css_class("dialog-window");
            let layout = gtk::Box::new(gtk::Orientation::Vertical, 12);
            layout.add_css_class("dialog-body");
            let label = gtk::Label::new(Some("Choose an application for the selected files"));
            label.set_wrap(true);
            layout.append(&label);
            let list = gtk::ListBox::new();
            list.set_selection_mode(gtk::SelectionMode::Single);
            for (name, _) in &apps {
                let label = gtk::Label::new(Some(name));
                label.set_xalign(0.0);
                label.set_margin_top(8);
                label.set_margin_bottom(8);
                list.append(&label);
            }
            let scroll = gtk::ScrolledWindow::builder()
                .vexpand(true)
                .child(&list)
                .build();
            layout.append(&scroll);
            let cancel = gtk::Button::with_label("Cancel");
            layout.append(&cancel);
            cancel.connect_clicked(glib::clone!(
                #[weak]
                dialog,
                move |_| dialog.close()
            ));
            list.connect_row_activated(glib::clone!(
                #[weak]
                ui,
                #[weak]
                dialog,
                move |_, row| {
                    let Some((_, id)) = apps.get(row.index() as usize) else {
                        return;
                    };
                    let id = id.clone();
                    let paths = paths.clone();
                    dialog.close();
                    ui.run_job("Opening files", false, move |_, _| {
                        let app = gio_unix::DesktopAppInfo::new(&id)
                            .ok_or("This application is no longer installed")?;
                        let files: Vec<_> = paths.iter().map(gio::File::for_path).collect();
                        app.launch(&files, gio::AppLaunchContext::NONE)
                            .map_err(|e| e.to_string())?;
                        Ok("Files opened".into())
                    });
                }
            ));
            dialog.set_child(Some(&layout));
            ui.dialog_shortcuts(&dialog);
            dialog.present();
        });
    }

    fn dialog_shortcuts(self: &Rc<Self>, window: &gtk::Window) {
        let keys = gtk::EventControllerKey::new();
        keys.set_propagation_phase(gtk::PropagationPhase::Capture);
        keys.connect_key_pressed(glib::clone!(
            #[weak(rename_to=ui)]
            self,
            #[weak]
            window,
            #[upgrade_or]
            glib::Propagation::Proceed,
            move |_, key, _, modifiers| {
                if super::shortcuts::command(key, modifiers) == Some("quit") {
                    window.close();
                    ui.window.close();
                    glib::Propagation::Stop
                } else {
                    glib::Propagation::Proceed
                }
            }
        ));
        window.add_controller(keys);
    }
}
