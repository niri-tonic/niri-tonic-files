use gtk::{gio, glib, prelude::*};
use niri_tonic_files::{
    config::{Config, Paths, Session},
    locations,
    storage::Registry,
    theme::{Theme, detect_fonts},
    ui::Ui,
};
use std::{cell::RefCell, path::PathBuf, rc::Rc, sync::Arc};

fn main() -> glib::ExitCode {
    // Before GTK/GIO initialization or any worker: toolkit dialogs are English too.
    unsafe {
        std::env::set_var("LANGUAGE", "en");
    }
    match run() {
        Ok(code) => code,
        Err(message) => {
            eprintln!("niri-tonic-files: {message}");
            glib::ExitCode::FAILURE
        }
    }
}

fn run() -> Result<glib::ExitCode, String> {
    let args: Vec<_> = std::env::args_os().skip(1).collect();
    if args.iter().any(|s| s == "--version" || s == "-V") {
        println!("niri-tonic-files {}", niri_tonic_files::APP_VERSION);
        return Ok(glib::ExitCode::SUCCESS);
    }
    let mut paths = Paths::from_env()?;
    let mut config = Config::load(&paths.config).map_err(|e| e.to_string())?;
    let theme = Theme::load(&config.appearance.theme)?;
    let fonts = detect_fonts(&config.appearance, &paths);
    if args.iter().any(|s| s == "--help" || s == "-h") {
        println!(
            "niri-tonic-files [FOLDER|file:///URI|filen://ACCOUNT/PATH|drive://ACCOUNT/PATH ...]\n\n  --check-config   Validate configuration and theme\n  --smoke-test     Exercise the UI in a temporary directory, then exit\n  --version        Show the application version\n  --help           Show this help\n\nA two-pane Wayland file explorer. Starts in Home by default."
        );
        return Ok(glib::ExitCode::SUCCESS);
    }
    if args.iter().any(|s| s == "--check-config") {
        println!(
            "Configuration OK: {}\nTheme: {}\nInterface font: {}\nMonospace font: {}",
            paths.config.display(),
            theme.name,
            fonts.interface.as_deref().unwrap_or("GTK system default"),
            fonts.mono.as_deref().unwrap_or("system monospace")
        );
        return Ok(glib::ExitCode::SUCCESS);
    }
    let smoke = args.iter().any(|s| s == "--smoke-test");
    if smoke {
        // A panic inside a GLib task otherwise only logs the failed assertion
        // and leaves the test window running, which can mask a failed smoke test.
        std::panic::set_hook(Box::new(|info| {
            eprintln!("SMOKE FAILED: {info}");
            std::process::exit(1);
        }));
    }
    let fixture = if smoke {
        Some(
            tempfile::Builder::new()
                .prefix("niri-tonic-smoke-")
                .tempdir()
                .map_err(|e| e.to_string())?,
        )
    } else {
        None
    };
    let initial: Vec<PathBuf> = if let Some(fixture) = &fixture {
        paths.config = fixture.path().join("config.yaml");
        paths.state = fixture.path().join("state");
        paths.home = fixture.path().to_path_buf();
        config.favorites.clear();
        config.connectors.local.read_only = false;
        config.connectors.instances.clear();
        config.view = Default::default();
        config
            .associations
            .extensions
            .insert("txt".into(), vec!["/usr/bin/true".into()]);
        config.navigation.restore_session = false;
        std::fs::create_dir_all(fixture.path().join("Alpha/Beta/Gamma"))
            .map_err(|e| e.to_string())?;
        std::fs::write(fixture.path().join("hello world.txt"), "Hello, Nord!\n")
            .map_err(|e| e.to_string())?;
        vec![fixture.path().into()]
    } else {
        args.iter()
            .filter(|s| s.as_os_str() != "--")
            .map(|arg| {
                if arg.to_string_lossy().starts_with('-') {
                    return Err(format!("Unknown option: {}", arg.to_string_lossy()));
                }
                let file = gio::File::for_commandline_arg(arg);
                if let Some(uri) = arg.to_str().filter(|s| locations::is_remote_uri(s)) {
                    config.parse_path(uri).map_err(|e| e.to_string())
                } else {
                    file.path().ok_or_else(|| {
                        "Use a local folder path or filen:// or drive:// account URI".into()
                    })
                }
            })
            .collect::<Result<_, _>>()?
    };
    let session: Session = if !smoke && initial.is_empty() && config.navigation.restore_session {
        std::fs::read(paths.state.join("session.json"))
            .ok()
            .and_then(|b| serde_json::from_slice(&b).ok())
            .filter(|s: &Session| s.version == 1)
            .unwrap_or_default()
    } else {
        Session::default()
    };
    let storage = Arc::new(Registry::from_config(&config, &paths)?);
    let application = gtk::Application::builder()
        .application_id("io.github.tdesaules.NiriTonicFiles")
        .flags(gio::ApplicationFlags::NON_UNIQUE)
        .build();
    let windows: Rc<RefCell<Vec<Rc<Ui>>>> = Rc::default();
    let keep = windows.clone();
    application.connect_activate(move |app| {
        if !keep.borrow().is_empty() {
            return;
        }
        let roots = if initial.is_empty() {
            vec![paths.home.clone()]
        } else {
            initial.clone()
        };
        for (index, root) in roots.into_iter().enumerate() {
            let ui = Ui::build(
                app,
                config.clone(),
                paths.clone(),
                (theme.clone(), fonts.clone()),
                root,
                smoke,
                storage.clone(),
            );
            if index == 0 {
                ui.restore(&session);
            }
            ui.present();
            if smoke {
                ui.smoke_test();
            }
            keep.borrow_mut().push(ui);
        }
    });
    let code = application.run_with_args::<&str>(&[]);
    for ui in windows.borrow().iter() {
        ui.finish();
    }
    drop(windows);
    drop(fixture);
    Ok(code)
}
