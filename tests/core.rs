use niri_tonic_files::{
    associations,
    config::{Config, Paths, View},
    connector::*,
    local::{self, Local},
    navigation::Navigation,
    theme::Theme,
};
use std::{
    collections::BTreeMap,
    ffi::OsString,
    fs,
    os::unix::{
        ffi::{OsStrExt, OsStringExt},
        fs::{MetadataExt, PermissionsExt, symlink},
    },
    path::{Path, PathBuf},
    sync::{
        Arc, Barrier,
        atomic::{AtomicBool, Ordering},
    },
};

fn write(path: &Path, text: &str) {
    fs::write(path, text).unwrap();
}
fn ctx() -> Context {
    Context::default()
}

#[test]
fn configuration_precedence_and_xdg_fallbacks() {
    let mut env = BTreeMap::from([("HOME", PathBuf::from("/home/test"))]);
    let resolve = |env: &BTreeMap<&str, PathBuf>| Paths::resolve(|k| env.get(k).cloned()).unwrap();
    assert_eq!(
        resolve(&env).config,
        Path::new("/home/test/.config/niri-tonic-files/config.yaml")
    );
    env.insert("XDG_CONFIG_HOME", "/config".into());
    assert_eq!(
        resolve(&env).config,
        Path::new("/config/niri-tonic-files/config.yaml")
    );
    env.insert("NIRI_TONIC_FILES_CONFIG_FILE", "/custom.yaml".into());
    assert_eq!(resolve(&env).config, Path::new("/custom.yaml"));
    env.remove("NIRI_TONIC_FILES_CONFIG_FILE");
    env.insert("XDG_CONFIG_HOME", "relative-is-invalid".into());
    assert_eq!(
        resolve(&env).config,
        Path::new("/home/test/.config/niri-tonic-files/config.yaml")
    );
    assert_ne!(resolve(&env).state, resolve(&env).cache);
}

#[test]
fn configuration_defaults_validation_and_secret_rejection() {
    assert_eq!(Config::parse("{}").unwrap().version, 1);
    assert!(!Config::default().navigation.restore_session);
    for yaml in [
        "version: 99",
        "view: { typo: true }",
        "connectors: { password: secret }",
        "connectors: { reserved: [{id: filen, label: Filen, token: secret}] }",
        "associations: {extensions: {txt: []}}",
        "associations: {extensions: {txt: ['app', '{files}']}}",
        "navigation: { animation_ms: 9000 }",
        "appearance: { theme: missing }",
    ] {
        assert!(Config::parse(yaml).is_err(), "accepted {yaml}");
    }
    let tmp = tempfile::tempdir().unwrap();
    assert_eq!(
        Config::load(&tmp.path().join("absent"))
            .unwrap()
            .appearance
            .theme,
        "nord"
    );
    let invalid = tmp.path().join("broken.yaml");
    write(&invalid, "view: [");
    assert!(
        Config::load(&invalid)
            .unwrap_err()
            .to_string()
            .contains("broken.yaml")
    );
    Config::parse(include_str!("../config.example.yaml")).unwrap();
    Theme::load("nord").unwrap();
}

#[test]
fn folders_and_favorites_alternate_between_two_stable_panes() {
    let mut nav = Navigation::new("/home/test".into());
    assert_eq!(nav.snapshot().columns, vec![PathBuf::from("/home/test")]);
    nav.open("/home/test/tmp".into());
    assert_eq!(
        nav.snapshot().columns,
        vec![PathBuf::from("/home/test"), PathBuf::from("/home/test/tmp")]
    );
    assert_eq!(nav.snapshot().active, 1);
    nav.open("/home/test/tmp/child".into());
    assert_eq!(
        nav.snapshot().columns,
        vec![
            PathBuf::from("/home/test/tmp/child"),
            PathBuf::from("/home/test/tmp")
        ]
    );
    assert_eq!(nav.snapshot().active, 0);
    // A favorite uses the same transition as opening a directory.
    nav.open("/home/test".into());
    assert_eq!(
        nav.snapshot().columns,
        vec![
            PathBuf::from("/home/test/tmp/child"),
            PathBuf::from("/home/test")
        ]
    );
    assert_eq!(nav.snapshot().active, 1);
    assert!(nav.back());
    assert_eq!(nav.active_path(), Some(Path::new("/home/test/tmp/child")));
    assert!(nav.forward());
    assert_eq!(nav.active_path(), Some(Path::new("/home/test")));
    nav.activate(0);
    nav.open("/another-favorite".into());
    assert_eq!(
        nav.snapshot().columns,
        vec![
            PathBuf::from("/home/test/tmp/child"),
            PathBuf::from("/another-favorite")
        ]
    );
    assert!(!nav.can_forward());
    assert!(nav.close(0));
    assert_eq!(nav.active_path(), Some(Path::new("/another-favorite")));
    assert!(nav.close(0));
    assert_eq!(nav.active_path(), None);
    assert!(nav.back());
    assert_eq!(nav.snapshot().columns.len(), 1);
    assert!(nav.parent());
    assert_eq!(nav.active_path(), Some(Path::new("/")));
    assert_eq!(nav.snapshot().columns.len(), 2);
    assert!(!nav.parent());
}

#[test]
fn connector_contract_round_trip_and_read_only_enforcement() {
    let tmp = tempfile::tempdir().unwrap();
    let local: Box<dyn Connector> = Box::new(Local::default());
    let target = local::location(&tmp.path().join("file"));
    local
        .write_new(&target, &mut &b"abcdef"[..], &ctx())
        .unwrap();
    assert_eq!(local.read(&target, 2, 3, &ctx()).unwrap(), b"cde");
    assert_eq!(local.stat(&target, &ctx()).unwrap().kind, Kind::File);
    let listing = local
        .list(&local::location(tmp.path()), None, &ctx())
        .unwrap();
    assert_eq!(listing.entries.len(), 1);
    assert!(listing.next.is_none());
    assert_eq!(
        local
            .list(&local::location(tmp.path()), Some("cursor"), &ctx())
            .unwrap_err()
            .code,
        ErrorCode::StaleCursor
    );
    let readonly = Local { read_only: true };
    assert!(readonly.capabilities().read_only());
    assert_eq!(
        readonly
            .delete(&target, DeleteMode::Permanent, &ctx())
            .unwrap_err()
            .code,
        ErrorCode::PermissionDenied
    );
    assert_eq!(
        readonly
            .write_new(
                &local::location(&tmp.path().join("new")),
                &mut &b"x"[..],
                &ctx()
            )
            .unwrap_err()
            .code,
        ErrorCode::PermissionDenied
    );
    assert_eq!(readonly.stat(&target, &ctx()).unwrap().size, 6);
    let mut handshake = Handshake {
        protocol: 1,
        connector: "future".into(),
        capabilities: readonly.capabilities(),
    };
    handshake.validate().unwrap();
    handshake.protocol = 2;
    assert!(handshake.validate().is_err());
    let encoded = serde_json::to_vec(&listing).unwrap();
    let decoded: Page = serde_json::from_slice(&encoded).unwrap();
    assert_eq!(decoded.entries[0].location, target);
}

#[test]
fn creation_rename_copy_and_move_never_replace() {
    let tmp = tempfile::tempdir().unwrap();
    let a = tmp.path().join("a");
    let b = tmp.path().join("b");
    write(&a, "source");
    write(&b, "existing");
    let local = Local::default();
    let (a, b) = (local::location(&a), local::location(&b));
    assert_eq!(
        local
            .write_new(&b, &mut &b"overwrite"[..], &ctx())
            .unwrap_err()
            .code,
        ErrorCode::AlreadyExists
    );
    assert_eq!(
        local.rename(&a, &b, &ctx()).unwrap_err().code,
        ErrorCode::AlreadyExists
    );
    assert_eq!(
        local.copy(&a, &b, &ctx()).unwrap_err().code,
        ErrorCode::AlreadyExists
    );
    assert_eq!(
        local.move_item(&a, &b, &ctx()).unwrap_err().code,
        ErrorCode::AlreadyExists
    );
    assert_eq!(
        fs::read_to_string(local::path(&a).unwrap()).unwrap(),
        "source"
    );
    assert_eq!(
        fs::read_to_string(local::path(&b).unwrap()).unwrap(),
        "existing"
    );
}

#[test]
fn simultaneous_copies_have_exactly_one_winner() {
    let tmp = tempfile::tempdir().unwrap();
    let a = tmp.path().join("a");
    let b = tmp.path().join("b");
    let dest = tmp.path().join("dest");
    write(&a, "one");
    write(&b, "two");
    let barrier = Arc::new(Barrier::new(2));
    let threads: Vec<_> = [a, b]
        .into_iter()
        .map(|src| {
            let dest = dest.clone();
            let barrier = barrier.clone();
            std::thread::spawn(move || {
                barrier.wait();
                Local::default().copy(&local::location(&src), &local::location(&dest), &ctx())
            })
        })
        .collect();
    let results: Vec<_> = threads.into_iter().map(|t| t.join().unwrap()).collect();
    assert_eq!(results.iter().filter(|r| r.is_ok()).count(), 1);
    assert_eq!(
        results
            .iter()
            .filter_map(|r| r.as_ref().err())
            .next()
            .unwrap()
            .code,
        ErrorCode::AlreadyExists
    );
    assert!(["one", "two"].contains(&fs::read_to_string(dest).unwrap().as_str()));
}

#[test]
fn repeated_pastes_number_before_extension_without_replacing() {
    let tmp = tempfile::tempdir().unwrap();
    let source = tmp.path().join("report.txt");
    write(&source, "original");
    let connector: Box<dyn Connector> = Box::new(Local::default());
    for number in 1..=3 {
        let copied = connector
            .copy_unique(&local::location(&source), &local::location(&source), &ctx())
            .unwrap();
        let expected = tmp.path().join(format!("report-{number}.txt"));
        assert_eq!(local::path(&copied).unwrap(), expected);
        assert_eq!(fs::read_to_string(expected).unwrap(), "original");
    }
    assert_eq!(fs::read_to_string(&source).unwrap(), "original");
}

#[test]
fn numbered_copies_preserve_hidden_non_utf8_and_directory_names() {
    let tmp = tempfile::tempdir().unwrap();
    let connector = Local::default();
    for (original, expected) in [
        (OsString::from(".env"), OsString::from(".env-1")),
        (OsString::from("README"), OsString::from("README-1")),
        (
            OsString::from_vec(b"name\xff.txt".to_vec()),
            OsString::from_vec(b"name\xff-1.txt".to_vec()),
        ),
    ] {
        let source = tmp.path().join(original);
        write(&source, "bytes");
        let copied = connector
            .copy_unique(&local::location(&source), &local::location(&source), &ctx())
            .unwrap();
        assert_eq!(local::path(&copied).unwrap(), tmp.path().join(expected));
    }
    let directory = tmp.path().join("photos.v1");
    fs::create_dir(&directory).unwrap();
    write(&directory.join("file"), "photo");
    let copied = connector
        .copy_unique(
            &local::location(&directory),
            &local::location(&directory),
            &ctx(),
        )
        .unwrap();
    assert_eq!(
        local::path(&copied).unwrap(),
        tmp.path().join("photos.v1-1")
    );
    assert_eq!(
        fs::read_to_string(tmp.path().join("photos.v1-1/file")).unwrap(),
        "photo"
    );
}

#[test]
fn numbered_copies_skip_broken_links_and_racing_destinations() {
    let tmp = tempfile::tempdir().unwrap();
    let source = tmp.path().join("source.txt");
    write(&source, "new");
    let destination = tmp.path().join("target.txt");
    write(&destination, "keep");
    symlink("missing", tmp.path().join("target-1.txt")).unwrap();
    let barrier = Arc::new(Barrier::new(3));
    let threads: Vec<_> = (0..3)
        .map(|_| {
            let source = source.clone();
            let destination = destination.clone();
            let barrier = barrier.clone();
            std::thread::spawn(move || {
                barrier.wait();
                Local::default()
                    .copy_unique(
                        &local::location(&source),
                        &local::location(&destination),
                        &ctx(),
                    )
                    .unwrap()
            })
        })
        .collect();
    let mut paths: Vec<_> = threads
        .into_iter()
        .map(|thread| local::path(&thread.join().unwrap()).unwrap())
        .collect();
    paths.sort();
    assert_eq!(
        paths,
        (2..=4)
            .map(|n| tmp.path().join(format!("target-{n}.txt")))
            .collect::<Vec<_>>()
    );
    assert_eq!(fs::read_to_string(destination).unwrap(), "keep");
    assert_eq!(
        fs::read_link(tmp.path().join("target-1.txt")).unwrap(),
        Path::new("missing")
    );
}

#[test]
fn cancelling_a_numbered_copy_keeps_existing_items_and_cleans_staging() {
    let tmp = tempfile::tempdir().unwrap();
    let source = tmp.path().join("source.txt");
    fs::write(&source, vec![42; 2 * 1024 * 1024]).unwrap();
    let cancelled = Arc::new(AtomicBool::new(false));
    let flag = cancelled.clone();
    let mut context = Context::new(move |p| {
        if p.bytes > 0 {
            flag.store(true, Ordering::Relaxed);
        }
    });
    context.cancelled = cancelled;
    assert_eq!(
        Local::default()
            .copy_unique(
                &local::location(&source),
                &local::location(&source),
                &context
            )
            .unwrap_err()
            .code,
        ErrorCode::Cancelled
    );
    assert_eq!(fs::read_dir(tmp.path()).unwrap().count(), 1);
    assert_eq!(fs::metadata(source).unwrap().len(), 2 * 1024 * 1024);
}

#[test]
fn archive_restore_and_collisions_preserve_both_items() {
    let tmp = tempfile::tempdir().unwrap();
    let file = tmp.path().join("report.txt");
    write(&file, "old");
    let local = Local::default();
    let archived = local.archive(&local::location(&file), &ctx()).unwrap();
    assert_eq!(
        local::path(&archived).unwrap(),
        tmp.path().join(".archive/report.txt")
    );
    assert!(!file.exists());
    write(&file, "new");
    assert_eq!(
        local
            .archive(&local::location(&file), &ctx())
            .unwrap_err()
            .code,
        ErrorCode::AlreadyExists
    );
    assert_eq!(
        local.restore(&archived, &ctx()).unwrap_err().code,
        ErrorCode::AlreadyExists
    );
    assert_eq!(fs::read_to_string(&file).unwrap(), "new");
    fs::remove_file(&file).unwrap();
    local.restore(&archived, &ctx()).unwrap();
    assert_eq!(fs::read_to_string(&file).unwrap(), "old");
}

#[test]
fn archive_does_not_follow_a_preexisting_archive_symlink() {
    let tmp = tempfile::tempdir().unwrap();
    let target = tempfile::tempdir().unwrap();
    let file = tmp.path().join("file");
    write(&file, "keep");
    symlink(target.path(), tmp.path().join(".archive")).unwrap();
    assert_eq!(
        Local::default()
            .archive(&local::location(&file), &ctx())
            .unwrap_err()
            .code,
        ErrorCode::InvalidInput
    );
    assert!(file.exists());
    assert_eq!(fs::read_dir(target.path()).unwrap().count(), 0);
}

#[test]
fn recursive_copy_preserves_links_and_delete_never_follows_them() {
    let tmp = tempfile::tempdir().unwrap();
    let source = tmp.path().join("source");
    let dest = tmp.path().join("copy");
    let outside = tmp.path().join("outside");
    fs::create_dir(&source).unwrap();
    fs::create_dir(&outside).unwrap();
    write(&outside.join("keep"), "safe");
    write(&source.join("file"), "data");
    symlink(&outside, source.join("link")).unwrap();
    symlink("missing", source.join("broken")).unwrap();
    let local = Local::default();
    local
        .copy(&local::location(&source), &local::location(&dest), &ctx())
        .unwrap();
    assert!(
        fs::symlink_metadata(dest.join("link"))
            .unwrap()
            .is_symlink()
    );
    assert_eq!(
        fs::read_link(dest.join("broken")).unwrap(),
        Path::new("missing")
    );
    local
        .delete(&local::location(&dest), DeleteMode::Permanent, &ctx())
        .unwrap();
    assert_eq!(fs::read_to_string(outside.join("keep")).unwrap(), "safe");
    assert!(source.exists());
}

#[test]
fn copy_inside_itself_and_through_symlink_parent_is_rejected() {
    let tmp = tempfile::tempdir().unwrap();
    let source = tmp.path().join("source");
    fs::create_dir(&source).unwrap();
    let alias = tmp.path().join("alias");
    symlink(&source, &alias).unwrap();
    let local = Local::default();
    for dest in [source.join("child"), alias.join("child")] {
        assert_eq!(
            local
                .copy(&local::location(&source), &local::location(&dest), &ctx())
                .unwrap_err()
                .code,
            ErrorCode::InvalidInput
        );
    }
}

#[test]
fn non_utf8_and_special_names_round_trip_through_connector() {
    let tmp = tempfile::tempdir().unwrap();
    let name = OsString::from_vec(b"special \xff ; $(touch nope)".to_vec());
    let file = tmp.path().join(name);
    write(&file, "data");
    let loc = local::location(&file);
    assert_eq!(local::path(&loc).unwrap(), file);
    let local = Local::default();
    let page = local
        .list(&local::location(tmp.path()), None, &ctx())
        .unwrap();
    assert_eq!(page.entries[0].location, loc);
    let archived = local.archive(&loc, &ctx()).unwrap();
    local.restore(&archived, &ctx()).unwrap();
    assert!(file.exists());
    let (_, args) = associations::argv(&["viewer".into()], std::slice::from_ref(&file)).unwrap();
    assert_eq!(args[0].as_bytes(), file.as_os_str().as_bytes());
}

#[test]
fn cancellation_mid_copy_removes_staging_and_keeps_original() {
    let tmp = tempfile::tempdir().unwrap();
    let src = tmp.path().join("large");
    let dest = tmp.path().join("dest");
    fs::write(&src, vec![42u8; 4 * 1024 * 1024]).unwrap();
    let cancel = Arc::new(AtomicBool::new(false));
    let flag = cancel.clone();
    let mut context = Context::new(move |p| {
        if p.bytes > 0 {
            flag.store(true, Ordering::Relaxed);
        }
    });
    context.cancelled = cancel;
    assert_eq!(
        Local::default()
            .copy(&local::location(&src), &local::location(&dest), &context)
            .unwrap_err()
            .code,
        ErrorCode::Cancelled
    );
    assert!(!dest.exists());
    assert_eq!(fs::metadata(&src).unwrap().len(), 4 * 1024 * 1024);
    assert_eq!(fs::read_dir(tmp.path()).unwrap().count(), 1);
}

#[test]
fn cancelled_move_leaves_source_intact() {
    let tmp = tempfile::tempdir().unwrap();
    let src = tmp.path().join("src");
    let dest = tmp.path().join("dest");
    write(&src, "original");
    let context = ctx();
    context.cancel();
    assert_eq!(
        Local::default()
            .move_item(&local::location(&src), &local::location(&dest), &context)
            .unwrap_err()
            .code,
        ErrorCode::Cancelled
    );
    assert!(src.exists());
    assert!(!dest.exists());
}

#[test]
fn vanished_files_and_permission_errors_are_typed() {
    let tmp = tempfile::tempdir().unwrap();
    let missing = local::location(&tmp.path().join("missing"));
    assert_eq!(
        Local::default().stat(&missing, &ctx()).unwrap_err().code,
        ErrorCode::NotFound
    );
    let private = tmp.path().join("private");
    fs::create_dir(&private).unwrap();
    fs::set_permissions(&private, fs::Permissions::from_mode(0o000)).unwrap();
    let result = Local::default().list(&local::location(&private), None, &ctx());
    fs::set_permissions(&private, fs::Permissions::from_mode(0o700)).unwrap();
    if fs::metadata(tmp.path()).unwrap().uid() != 0 {
        assert_eq!(result.unwrap_err().code, ErrorCode::PermissionDenied);
    }
}

#[test]
fn cross_device_move_commits_copy_before_removing_source() {
    let source = tempfile::tempdir().unwrap();
    let destination = tempfile::tempdir_in("/dev/shm").unwrap();
    if fs::metadata(source.path()).unwrap().dev() == fs::metadata(destination.path()).unwrap().dev()
    {
        return;
    }
    let folder = source.path().join("folder");
    fs::create_dir(&folder).unwrap();
    write(&folder.join("file"), "content");
    symlink("file", folder.join("link")).unwrap();
    let dest = destination.path().join("folder");
    Local::default()
        .move_item(&local::location(&folder), &local::location(&dest), &ctx())
        .unwrap();
    assert!(!folder.exists());
    assert_eq!(fs::read_to_string(dest.join("file")).unwrap(), "content");
    assert_eq!(fs::read_link(dest.join("link")).unwrap(), Path::new("file"));
}

#[test]
fn changed_source_during_copy_is_not_committed() {
    let tmp = tempfile::tempdir().unwrap();
    let src = tmp.path().join("source");
    let dest = tmp.path().join("dest");
    fs::write(&src, vec![1u8; 2 * 1024 * 1024]).unwrap();
    let changed = Arc::new(AtomicBool::new(false));
    let flag = changed.clone();
    let path = src.clone();
    let context = Context::new(move |p| {
        if p.bytes > 0 && !flag.swap(true, Ordering::Relaxed) {
            write(&path, "new content");
        }
    });
    assert!(
        Local::default()
            .copy(&local::location(&src), &local::location(&dest), &context)
            .is_err()
    );
    assert!(!dest.exists());
    assert_eq!(fs::read_to_string(src).unwrap(), "new content");
}

#[test]
fn broken_symlink_collision_is_not_treated_as_absent() {
    let tmp = tempfile::tempdir().unwrap();
    let src = tmp.path().join("source");
    let dest = tmp.path().join("dest");
    write(&src, "data");
    symlink("missing", &dest).unwrap();
    assert_eq!(
        Local::default()
            .copy(&local::location(&src), &local::location(&dest), &ctx())
            .unwrap_err()
            .code,
        ErrorCode::AlreadyExists
    );
    assert_eq!(fs::read_link(dest).unwrap(), Path::new("missing"));
}

#[test]
fn atomic_configuration_write_preserves_symlinks_and_non_utf8_favorites() {
    let tmp = tempfile::tempdir().unwrap();
    let target = tmp.path().join("real.yaml");
    let link = tmp.path().join("config.yaml");
    write(&target, "version: 1");
    symlink(&target, &link).unwrap();
    niri_tonic_files::config::atomic_write(&link, b"version: 1\nview: {hidden: true}\n").unwrap();
    assert!(fs::symlink_metadata(&link).unwrap().is_symlink());
    assert!(Config::load(&target).unwrap().view.hidden);
    let bytes = tmp
        .path()
        .join(OsString::from_vec(b"favorite-\xff".to_vec()));
    let uri = local::location(&bytes).id;
    let yaml = format!("favorites:\n  - name: Byte path\n    path: {uri}\n");
    let config = Config::parse(&yaml).unwrap();
    let paths = Paths::resolve(|key| {
        if key == "HOME" {
            Some(tmp.path().into())
        } else {
            None
        }
    })
    .unwrap();
    assert_eq!(paths.expand(&config.favorites[0].path), bytes);
}

#[test]
fn association_priority_and_arguments_do_not_use_shell_expansion() {
    let config=Config::parse("associations:\n  extensions:\n    txt: ['editor', '--']\n  mime_types:\n    text/plain: ['mime-editor']\n    image/*: ['images']\n").unwrap();
    assert_eq!(
        associations::matching(
            &config.associations,
            Path::new("FILE.TXT"),
            Some("text/plain")
        )
        .unwrap()[0],
        "editor"
    );
    assert_eq!(
        associations::matching(
            &config.associations,
            Path::new("unknown"),
            Some("text/plain")
        )
        .unwrap()[0],
        "mime-editor"
    );
    assert_eq!(
        associations::matching(&config.associations, Path::new("photo"), Some("image/png"))
            .unwrap()[0],
        "images"
    );
    let paths = vec![
        PathBuf::from("/a b/'quote'"),
        PathBuf::from("/$(touch should-not-exist);*"),
    ];
    let (program, args) = associations::argv(&["viewer".into(), "--".into()], &paths).unwrap();
    assert_eq!(program, "viewer");
    assert_eq!(args.len(), 3);
    assert_eq!(args[2], paths[1]);
    assert!(associations::argv(&["viewer".into()], &[PathBuf::from("-option")]).is_err());
}

#[test]
fn folders_first_is_preserved_with_descending_sort() {
    let tmp = tempfile::tempdir().unwrap();
    fs::create_dir(tmp.path().join("a-folder")).unwrap();
    write(&tmp.path().join("z-file"), "file");
    let mut entries = Local::default()
        .list(&local::location(tmp.path()), None, &ctx())
        .unwrap()
        .entries;
    local::sort(
        &mut entries,
        &View {
            descending: true,
            ..View::default()
        },
    );
    assert!(entries[0].navigable);
}

#[test]
fn trash_uses_freedesktop_trash_in_an_isolated_process() {
    if let Some(root) = std::env::var_os("NIRI_TONIC_TRASH_TEST") {
        let root = PathBuf::from(root);
        let source = root.join("original.txt");
        write(&source, "recoverable");
        Local::default()
            .delete(&local::location(&source), DeleteMode::Trash, &ctx())
            .unwrap();
        assert!(!source.exists());
        let data = root.join("data/Trash");
        assert_eq!(
            fs::read_to_string(data.join("files/original.txt")).unwrap(),
            "recoverable"
        );
        let info = fs::read_to_string(data.join("info/original.txt.trashinfo")).unwrap();
        assert!(info.contains("[Trash Info]"));
        return;
    }
    let tmp = tempfile::tempdir().unwrap();
    fs::create_dir(tmp.path().join("data")).unwrap();
    let output = std::process::Command::new(std::env::current_exe().unwrap())
        .args([
            "--exact",
            "trash_uses_freedesktop_trash_in_an_isolated_process",
            "--nocapture",
        ])
        .env("NIRI_TONIC_TRASH_TEST", tmp.path())
        .env("HOME", tmp.path())
        .env("XDG_DATA_HOME", tmp.path().join("data"))
        .output()
        .unwrap();
    assert!(
        output.status.success(),
        "{}\n{}",
        String::from_utf8_lossy(&output.stdout),
        String::from_utf8_lossy(&output.stderr)
    );
}
