use super::Ui;
use gtk::{gdk, glib, prelude::*};
use std::{rc::Rc, time::Duration};

impl Ui {
    pub fn smoke_test(self: &Rc<Self>) {
        let weak = Rc::downgrade(self);
        glib::spawn_future_local(async move {
            let Some(ui) = weak.upgrade() else {
                return;
            };
            let root = ui.active_path().expect("smoke root");
            glib::timeout_future(Duration::from_millis(900)).await;
            assert!(ui.window.is_mapped(), "Wayland window was not mapped");
            assert_eq!(
                ui.columns.borrow()[0].model.n_items(),
                2,
                "initial directory listing"
            );
            eprintln!(
                "SMOKE mapped: scale={}, font={}, path font={}",
                ui.window.scale_factor(),
                ui.window.pango_context().font_description().unwrap(),
                ui.path_entry.pango_context().font_description().unwrap()
            );
            let initial_pane = ui.columns.borrow()[0].root.clone();
            assert!(!ui.window.is_decorated());
            ui.row_clicked(0, 0, 1, gdk::ModifierType::CONTROL_MASK);
            assert_eq!(ui.nav.borrow().snapshot().columns.len(), 1);
            ui.row_clicked(
                0,
                1,
                1,
                gdk::ModifierType::CONTROL_MASK | gdk::ModifierType::SHIFT_MASK,
            );
            assert_eq!(ui.selected_entries().len(), 2);
            assert_eq!(ui.nav.borrow().snapshot().columns.len(), 1);
            ui.row_clicked(0, 1, 1, gdk::ModifierType::empty());
            assert!(ui.job.borrow().is_none(), "one click must not open a file");
            ui.row_clicked(0, 1, 2, gdk::ModifierType::empty());
            assert!(ui.job.borrow().is_some(), "double-click must open the file");
            glib::timeout_future(Duration::from_millis(500)).await;
            ui.row_clicked(0, 0, 1, gdk::ModifierType::empty());
            glib::timeout_future(Duration::from_millis(700)).await;
            assert_eq!(ui.nav.borrow().snapshot().columns.len(), 2);
            assert_eq!(
                ui.columns.borrow()[0].root,
                initial_pane,
                "opening the first folder preserves Home's view"
            );
            assert_eq!(ui.active_path().unwrap(), root.join("Alpha"));
            ui.row_clicked(1, 0, 1, gdk::ModifierType::empty());
            let retained = ui.columns.borrow()[1].root.clone();
            glib::timeout_future(Duration::from_millis(700)).await;
            assert_eq!(ui.nav.borrow().snapshot().columns.len(), 2);
            assert_eq!(
                ui.nav.borrow().snapshot().columns[0],
                root.join("Alpha/Beta")
            );
            assert_eq!(ui.nav.borrow().snapshot().columns[1], root.join("Alpha"));
            assert_eq!(
                ui.columns.borrow()[1].root,
                retained,
                "opposite pane must retain its widgets and scroll state"
            );
            assert!(ui.columns.borrow()[1].selection.is_selected(0));
            assert_eq!(ui.active_path().unwrap(), root.join("Alpha/Beta"));
            ui.window.lookup_action("back").unwrap().activate(None);
            glib::timeout_future(Duration::from_millis(700)).await;
            assert_eq!(ui.active_path().unwrap(), root.join("Alpha"));
            ui.window.lookup_action("forward").unwrap().activate(None);
            glib::timeout_future(Duration::from_millis(700)).await;
            assert_eq!(ui.active_path().unwrap(), root.join("Alpha/Beta"));
            find_button(ui.sidebar.upcast_ref(), "Home")
                .unwrap()
                .emit_clicked();
            glib::timeout_future(Duration::from_millis(500)).await;
            assert_eq!(
                ui.nav.borrow().snapshot().columns,
                vec![root.join("Alpha/Beta"), root.clone()]
            );
            assert_eq!(ui.nav.borrow().snapshot().active, 1);
            ui.activate_column(0);
            ui.window
                .lookup_action("close-column")
                .unwrap()
                .activate(None);
            glib::timeout_future(Duration::from_millis(700)).await;
            assert_eq!(ui.nav.borrow().snapshot().columns.len(), 1);
            assert_eq!(ui.active_path().unwrap(), root);
            ui.window
                .lookup_action("animations")
                .unwrap()
                .activate(None);
            ui.path_entry.set_text(&root.to_string_lossy());
            ui.path_entry.emit_activate();
            glib::timeout_future(Duration::from_millis(700)).await;
            assert_eq!(ui.active_path().unwrap(), root);
            assert_eq!(
                ui.columns.borrow()[1].slot.transition_type(),
                gtk::StackTransitionType::None
            );
            click_menu(&ui, "New File…").await;
            submit_prompt(&ui, "New File", "new item.txt");
            glib::timeout_future(Duration::from_millis(500)).await;
            assert!(root.join("new item.txt").exists());
            select(&ui, "new item.txt");
            click_menu(&ui, "Rename…").await;
            submit_prompt(&ui, "Rename", "hello world.txt");
            glib::timeout_future(Duration::from_millis(500)).await;
            assert!(root.join("new item.txt").exists());
            select(&ui, "new item.txt");
            click_menu(&ui, "Rename…").await;
            submit_prompt(&ui, "Rename", "renamed.txt");
            glib::timeout_future(Duration::from_millis(500)).await;
            assert!(root.join("renamed.txt").exists());
            select(&ui, "renamed.txt");
            click_menu(&ui, "Archive").await;
            glib::timeout_future(Duration::from_millis(500)).await;
            assert!(root.join(".archive/renamed.txt").exists());
            click_menu(&ui, "View Archives").await;
            glib::timeout_future(Duration::from_millis(500)).await;
            select(&ui, "renamed.txt");
            click_menu(&ui, "Restore from Archive").await;
            glib::timeout_future(Duration::from_millis(500)).await;
            assert!(root.join("renamed.txt").exists());
            ui.activate_column(1);
            select(&ui, "hello world.txt");
            click_menu(&ui, "Copy").await;
            ui.activate_column(0);
            click_menu(&ui, "Paste").await;
            glib::timeout_future(Duration::from_millis(500)).await;
            assert!(root.join(".archive/hello world.txt").exists());
            for number in 1..=3 {
                click_menu(&ui, "Paste").await;
                glib::timeout_future(Duration::from_millis(400)).await;
                assert!(
                    root.join(format!(".archive/hello world-{number}.txt"))
                        .exists()
                );
            }
            ui.activate_column(1);
            ui.columns.borrow()[1].selection.unselect_all();
            click_menu(&ui, "New Folder…").await;
            submit_prompt(&ui, "New Folder", "Menu folder");
            glib::timeout_future(Duration::from_millis(500)).await;
            assert!(root.join("Menu folder").is_dir());
            select(&ui, "Menu folder");
            click_menu(&ui, "Open").await;
            glib::timeout_future(Duration::from_millis(500)).await;
            assert_eq!(ui.active_path().unwrap(), root.join("Menu folder"));
            // Cut an item in the other pane and paste it into the active folder.
            ui.activate_column(1);
            select(&ui, "renamed.txt");
            click_menu(&ui, "Cut").await;
            ui.activate_column(0);
            click_menu(&ui, "Paste").await;
            glib::timeout_future(Duration::from_millis(500)).await;
            assert!(root.join("Menu folder/renamed.txt").exists());
            assert!(!root.join("renamed.txt").exists());
            select(&ui, "renamed.txt");
            click_menu(&ui, "Delete").await;
            glib::timeout_future(Duration::from_millis(500)).await;
            assert!(!root.join("Menu folder/renamed.txt").exists());
            // An external writer must trigger an inotify refresh without F5.
            let outside = root.join("external.txt");
            std::thread::spawn(move || std::fs::write(outside, "external change").unwrap())
                .join()
                .unwrap();
            glib::timeout_future(Duration::from_millis(500)).await;
            ui.activate_column(1);
            select(&ui, "external.txt");
            ui.columns.borrow()[1].selection.unselect_all();
            click_menu(&ui, "Add to Favorites").await;
            glib::timeout_future(Duration::from_millis(500)).await;
            assert_eq!(ui.config.borrow().favorites.len(), 1);
            assert_eq!(
                crate::config::Config::load(&ui.paths.config)
                    .unwrap()
                    .favorites
                    .len(),
                1
            );
            click_menu(&ui, "Remove from Favorites").await;
            glib::timeout_future(Duration::from_millis(500)).await;
            assert!(ui.config.borrow().favorites.is_empty());
            ui.change_favorites(vec![root.join("Alpha")], false);
            glib::timeout_future(Duration::from_millis(500)).await;
            let favorite = find_button(ui.sidebar.upcast_ref(), "Alpha").unwrap();
            let menu = ui.favorite_menu(&favorite, &root.join("Alpha"), 5.0, 5.0);
            find_button(menu.upcast_ref(), "Open Parent Folder")
                .unwrap()
                .emit_clicked();
            glib::timeout_future(Duration::from_millis(500)).await;
            assert_eq!(ui.active_path().unwrap(), root);
            let menu = ui.favorite_menu(&favorite, &root.join("Alpha"), 5.0, 5.0);
            find_button(menu.upcast_ref(), "Remove from Favorites")
                .unwrap()
                .emit_clicked();
            glib::timeout_future(Duration::from_millis(500)).await;
            assert!(ui.config.borrow().favorites.is_empty());
            ui.window
                .lookup_action("toggle-column")
                .unwrap()
                .activate(None);
            assert_eq!(ui.nav.borrow().snapshot().columns.len(), 1);
            ui.window
                .lookup_action("toggle-column")
                .unwrap()
                .activate(None);
            assert_eq!(ui.nav.borrow().snapshot().columns.len(), 2);
            eprintln!(
                "SMOKE PASS: single-click folders, double-click files, Control selection/ranges, alternating panes, rename dialog/collision, numbered copies, direct Delete, favorite context actions, column toggle, headerless window"
            );
            ui.window.close();
        });
    }
}

fn find_button(widget: &gtk::Widget, label: &str) -> Option<gtk::Button> {
    fn has_label(widget: &gtk::Widget, label: &str) -> bool {
        if widget
            .downcast_ref::<gtk::Label>()
            .is_some_and(|l| l.text() == label)
        {
            return true;
        }
        let mut child = widget.first_child();
        while let Some(current) = child {
            if has_label(&current, label) {
                return true;
            }
            child = current.next_sibling();
        }
        false
    }
    if let Some(button) = widget.downcast_ref::<gtk::Button>()
        && (button.label().as_deref() == Some(label)
            || button.child().is_some_and(|c| has_label(&c, label)))
    {
        return Some(button.clone());
    }
    let mut child = widget.first_child();
    while let Some(current) = child {
        if let Some(button) = find_button(&current, label) {
            return Some(button);
        }
        child = current.next_sibling();
    }
    None
}

async fn click_menu(ui: &Rc<Ui>, label: &str) {
    let index = ui.nav.borrow().snapshot().active;
    let anchor = ui.columns.borrow()[index].root.clone();
    let menu = ui.context_menu(&anchor, 20.0, 20.0);
    for removed in [
        "Navigation",
        "View",
        "Keyboard Shortcuts",
        "About Niri Tonic Files",
        "Open With…",
        "Move To…",
        "Move to Trash",
    ] {
        assert!(find_button(menu.upcast_ref(), removed).is_none());
    }
    let button = find_button(menu.upcast_ref(), label).expect("menu item");
    assert!(button.is_sensitive(), "menu item disabled: {label}");
    button.emit_clicked();
    glib::timeout_future(Duration::from_millis(100)).await;
}

fn select(ui: &Rc<Ui>, name: &str) {
    let index = ui.nav.borrow().snapshot().active;
    let columns = ui.columns.borrow();
    let column = &columns[index];
    let position = (0..column.model.n_items())
        .find(|i| super::columns::model_entry(&column.model, *i).is_some_and(|e| e.name == name))
        .unwrap_or_else(|| panic!("Missing {name} in {:?}", ui.active_path()));
    column.selection.select_item(position, true);
}

fn submit_prompt(ui: &Rc<Ui>, title: &str, text: &str) {
    let dialog = ui
        .window
        .application()
        .unwrap()
        .windows()
        .into_iter()
        .find(|w| w.title().as_deref() == Some(title))
        .or_else(|| {
            gtk::Window::list_toplevels()
                .into_iter()
                .filter_map(|w| w.downcast::<gtk::Window>().ok())
                .find(|w| w.title().as_deref() == Some(title))
        })
        .expect("prompt window");
    fn entry(widget: gtk::Widget) -> Option<gtk::Entry> {
        if let Ok(entry) = widget.clone().downcast::<gtk::Entry>() {
            return Some(entry);
        }
        let mut child = widget.first_child();
        while let Some(current) = child {
            if let Some(found) = entry(current.clone()) {
                return Some(found);
            }
            child = current.next_sibling();
        }
        None
    }
    entry(dialog.clone().upcast())
        .expect("prompt entry")
        .set_text(text);
    dialog
        .default_widget()
        .and_downcast::<gtk::Button>()
        .expect("default button")
        .emit_clicked();
}
