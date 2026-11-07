use super::Ui;
use crate::locations as local;
use gtk::prelude::*;
use std::{rc::Rc, sync::atomic::Ordering};

impl Ui {
    pub(super) fn rename_item(self: &Rc<Self>) {
        let entries = self.selected_entries();
        let [entry] = entries.as_slice() else {
            return;
        };
        if self.job.borrow().is_some() {
            return;
        }
        self.cancel_rename();
        let source = entry.location.clone();
        let original = entry.name.clone();
        self.prompt(
            "Rename",
            "New name",
            &entry.name,
            "Rename",
            move |ui, name| {
                if name == original {
                    return;
                }
                if let Err(error) = local::valid_name(&name) {
                    ui.message(&error.to_string(), true);
                    return;
                }
                let Ok(path) = local::path(&source) else {
                    return;
                };
                let Some(parent) = path.parent() else {
                    return;
                };
                let destination = local::location(&parent.join(&name));
                ui.run_job("Renaming", true, move |connector, context| {
                    connector
                        .rename(&source, &destination, context)
                        .map_err(|error| error.to_string())?;
                    Ok(format!("Renamed to {name}"))
                });
            },
        );
    }

    pub(super) fn rename_editor(&self) -> Option<gtk::Stack> {
        let draft = self.rename.borrow().clone()?;
        self.columns
            .borrow()
            .get(draft.column)?
            .editors
            .borrow()
            .get(&draft.location)?
            .upgrade()
    }

    pub(super) fn cancel_rename(&self) {
        let editor = self.rename_editor();
        let draft = self.rename.take();
        if let Some(editor) = editor {
            editor.set_visible_child_name("label");
        }
        if let Some(draft) = draft {
            if let Some(column) = self.columns.borrow().get(draft.column) {
                column.refresh.store(true, Ordering::Relaxed);
            }
            self.update_actions();
        }
    }

    pub(super) fn commit_rename(self: &Rc<Self>, name: String) {
        if self.job.borrow().is_some() {
            return;
        }
        let Some(draft) = self.rename.borrow().clone() else {
            return;
        };
        if name == draft.original {
            self.cancel_rename();
            return;
        }
        if let Err(error) = local::valid_name(&name) {
            self.message(&error.to_string(), true);
            return;
        }
        let Ok(source) = local::path(&draft.location) else {
            return;
        };
        let Some(parent) = source.parent() else {
            return;
        };
        let target = local::location(&parent.join(&name));
        if let Some(input) = self
            .rename_editor()
            .and_then(|e| e.child_by_name("edit"))
            .and_downcast::<gtk::Entry>()
        {
            input.set_sensitive(false);
        }
        self.run_job_after(
            "Renaming",
            false,
            move |connector, ctx| {
                connector
                    .rename(&draft.location, &target, ctx)
                    .map_err(|e| e.to_string())?;
                Ok(format!("Renamed to {name}"))
            },
            |ui, success| {
                if success {
                    ui.cancel_rename();
                } else if let Some(input) = ui
                    .rename_editor()
                    .and_then(|e| e.child_by_name("edit"))
                    .and_downcast::<gtk::Entry>()
                {
                    input.set_sensitive(true);
                    input.grab_focus();
                }
            },
        );
    }
}
