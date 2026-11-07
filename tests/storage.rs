use niri_tonic_files::{
    config::{Config, Paths},
    connector::*,
    local::{self, Local},
    locations,
    navigation::Navigation,
    storage::Registry,
};
use std::{
    fs,
    io::Read,
    path::{Path, PathBuf},
    sync::Arc,
};

type AfterWrite = Arc<dyn Fn(&Context) + Send + Sync>;

#[test]
fn plaintext_authentication_errors_do_not_echo_credentials() {
    let error=Config::parse("connectors:\n  instances:\n    - id: personal\n      name: Personal\n      type: filen\n      auth:\n        email: test@example.invalid\n        password: SYNTHETIC_PASSWORD_MUST_NOT_APPEAR\n").unwrap_err();
    assert!(!error.contains("SYNTHETIC_PASSWORD_MUST_NOT_APPEAR"));
    assert!(error.contains("reference"));
}

#[test]
fn equivalent_uri_escaping_is_not_a_different_remote_item() {
    let tmp = tempfile::tempdir().unwrap();
    let backend = account(tmp.path(), "account", false, false, false);
    fs::write(tmp.path().join("account/a+b.txt"), b"same item").unwrap();
    let registry = Registry::new(tmp.path().join("cache"), vec![backend]).unwrap();
    let entry = registry
        .stat(
            &Location {
                connector: "account".into(),
                id: "file:///%61%2Bb.txt".into(),
            },
            &Context::default(),
        )
        .unwrap();
    assert_eq!(entry.name, "a+b.txt");
}

// Offline accounts backed by separate temporary directories, with strict IDs.
#[derive(Default)]
struct Account {
    id: &'static str,
    root: PathBuf,
    read_only: bool,
    fail: bool,
    loop_cursor: bool,
    fail_after_read: bool,
    fail_write: bool,
    after_write: Option<AfterWrite>,
    transfer_names: std::collections::HashMap<String, String>,
}
impl Account {
    fn local(&self, l: &Location) -> Location {
        assert_eq!(l.connector, self.id);
        let virtual_path = locations::path(l).unwrap();
        local::location(
            &self
                .root
                .join(virtual_path.strip_prefix(locations::root(self.id)).unwrap()),
        )
    }
    fn entry(&self, mut e: Entry) -> Entry {
        e.transfer_name = self.transfer_names.get(&e.name).cloned();
        let p = local::path(&e.location).unwrap();
        e.location = locations::location(
            &locations::root(self.id).join(p.strip_prefix(&self.root).unwrap()),
        );
        e
    }
}
impl Connector for Account {
    fn id(&self) -> &str {
        self.id
    }
    fn capabilities(&self) -> Capabilities {
        Local {
            read_only: self.read_only,
        }
        .capabilities()
    }
    fn authentication(&self) -> Authentication {
        Authentication::Ready
    }
    fn list(&self, l: &Location, cursor: Option<&str>, c: &Context) -> Result<Page> {
        if self.loop_cursor {
            return Ok(Page {
                entries: vec![],
                next: Some("loop".into()),
            });
        }
        let page = Local::default().list(&self.local(l), cursor, c)?;
        Ok(Page {
            entries: page.entries.into_iter().map(|e| self.entry(e)).collect(),
            next: None,
        })
    }
    fn stat(&self, l: &Location, c: &Context) -> Result<Entry> {
        Local::default()
            .stat(&self.local(l), c)
            .map(|e| self.entry(e))
    }
    fn read(&self, l: &Location, offset: u64, limit: usize, c: &Context) -> Result<Vec<u8>> {
        assert!(limit <= 1024 * 1024);
        if self.fail || (self.fail_after_read && offset > 0) {
            return Err(Error::new(ErrorCode::Io, "Injected source failure"));
        }
        Local::default().read(&self.local(l), offset, limit, c)
    }
    fn write_new(&self, l: &Location, input: &mut dyn Read, c: &Context) -> Result<()> {
        if self.fail_write {
            struct FailingInput<'a> {
                input: &'a mut dyn Read,
                read: bool,
            }
            impl Read for FailingInput<'_> {
                fn read(&mut self, bytes: &mut [u8]) -> std::io::Result<usize> {
                    if self.read {
                        return Err(std::io::Error::other("Injected destination write failure"));
                    }
                    self.read = true;
                    let limit = bytes.len().min(32);
                    self.input.read(&mut bytes[..limit])
                }
            }
            return Local::default().write_new(
                &self.local(l),
                &mut FailingInput { input, read: false },
                c,
            );
        }
        Local::default().write_new(&self.local(l), input, c)?;
        if let Some(hook) = &self.after_write {
            hook(c);
        }
        Ok(())
    }
    fn create_directory(&self, l: &Location, c: &Context) -> Result<()> {
        Local::default().create_directory(&self.local(l), c)
    }
    fn rename(&self, s: &Location, d: &Location, c: &Context) -> Result<()> {
        Local::default().rename(&self.local(s), &self.local(d), c)
    }
    fn copy(&self, s: &Location, d: &Location, c: &Context) -> Result<()> {
        Local::default().copy(&self.local(s), &self.local(d), c)
    }
    fn move_item(&self, s: &Location, d: &Location, c: &Context) -> Result<()> {
        Local::default().move_item(&self.local(s), &self.local(d), c)
    }
    fn delete(&self, l: &Location, m: DeleteMode, c: &Context) -> Result<()> {
        Local::default().delete(&self.local(l), m, c)
    }
    fn archive(&self, _: &Location, _: &Context) -> Result<Location> {
        Err(Error::new(ErrorCode::Unsupported, "mock"))
    }
    fn restore(&self, _: &Location, _: &Context) -> Result<Location> {
        Err(Error::new(ErrorCode::Unsupported, "mock"))
    }
}
fn account(
    base: &Path,
    id: &'static str,
    read_only: bool,
    fail: bool,
    loop_cursor: bool,
) -> Arc<dyn Connector> {
    let root = base.join(id);
    fs::create_dir_all(&root).unwrap();
    Arc::new(Account {
        id,
        root,
        read_only,
        fail,
        loop_cursor,
        ..Default::default()
    })
}
fn remote(id: &str, name: &str) -> Location {
    locations::location(&locations::root(id).join(name))
}

#[test]
fn identity_aliases_use_portable_names_in_recursive_cross_account_copies() {
    let tmp = tempfile::tempdir().unwrap();
    let source = tmp.path().join("drive");
    fs::create_dir_all(source.join("Docs")).unwrap();
    fs::write(source.join("Docs/~iFIRST~Report.docx"), b"first export").unwrap();
    fs::write(source.join("Docs/~iSECOND~Report.docx"), b"second export").unwrap();
    let registry = Registry::new(
        tmp.path().join("cache"),
        vec![
            Arc::new(Account {
                id: "drive",
                root: source,
                transfer_names: [
                    ("~iFIRST~Report.docx".into(), "Report-first.docx".into()),
                    ("~iSECOND~Report.docx".into(), "Report-second.docx".into()),
                ]
                .into(),
                ..Default::default()
            }),
            account(tmp.path(), "filen", false, false, false),
        ],
    )
    .unwrap();
    registry
        .copy(
            &remote("drive", "Docs"),
            &remote("filen", "Docs"),
            &Context::default(),
        )
        .unwrap();
    assert_eq!(
        fs::read(tmp.path().join("filen/Docs/Report-first.docx")).unwrap(),
        b"first export"
    );
    assert_eq!(
        fs::read(tmp.path().join("filen/Docs/Report-second.docx")).unwrap(),
        b"second export"
    );
    let mut entry = registry
        .stat(
            &remote("drive", "Docs/~iFIRST~Report.docx"),
            &Context::default(),
        )
        .unwrap();
    for invalid in ["../escape", "/absolute", "", "..", "bad\\name"] {
        entry.transfer_name = Some(invalid.into());
        assert!(locations::transfer_name(&entry).is_err());
    }
}

#[test]
fn same_folder_paste_numbers_local_and_remote_names() {
    let tmp = tempfile::tempdir().unwrap();
    let base = tmp.path();
    let registry = Registry::new(
        base.join("cache"),
        vec![
            Arc::new(Local::default()),
            account(base, "a", false, false, false),
        ],
    )
    .unwrap();
    for (directory, connector) in [(base.to_path_buf(), "local"), (base.join("a"), "a")] {
        fs::write(directory.join("archive.tar.gz"), "original").unwrap();
        let loc = |name: &str| {
            if connector == "local" {
                local::location(&directory.join(name))
            } else {
                remote(connector, name)
            }
        };
        let source = loc("archive.tar.gz");
        for index in 1..=2 {
            let expected = loc(&format!("archive.tar-{index}.gz"));
            assert_eq!(
                registry
                    .copy_unique(&source, &source, &Context::default())
                    .unwrap(),
                expected
            );
        }
        fs::create_dir(directory.join("folder.ext")).unwrap();
        let folder = loc("folder.ext");
        assert_eq!(
            registry
                .copy_unique(&folder, &folder, &Context::default())
                .unwrap(),
            loc("folder.ext-1")
        );
        assert_eq!(
            registry
                .copy_unique(&folder, &loc("folder.ext/child"), &Context::default())
                .unwrap_err()
                .code,
            ErrorCode::InvalidInput
        );
        assert_eq!(
            fs::read_to_string(directory.join("archive.tar.gz")).unwrap(),
            "original"
        );
    }
}

#[test]
fn cross_directory_move_rechecks_tree_before_deleting_source() {
    for change in ["file", "child", "cancel"] {
        let tmp = tempfile::tempdir().unwrap();
        let base = tmp.path();
        let source = base.join("source");
        fs::create_dir(&source).unwrap();
        fs::write(source.join("file"), "original").unwrap();
        fs::create_dir(base.join("a")).unwrap();
        let changed_source = source.clone();
        let backend = Account {
            id: "a",
            root: base.join("a"),
            after_write: Some(Arc::new(move |ctx| match change {
                "file" => {
                    fs::write(changed_source.join("file"), "changed after being copied").unwrap()
                }
                "child" => fs::write(changed_source.join("new-child"), "not in copy").unwrap(),
                _ => ctx.cancel(),
            })),
            ..Default::default()
        };
        let registry = Registry::new(
            base.join("cache"),
            vec![Arc::new(Local::default()), Arc::new(backend)],
        )
        .unwrap();
        let error = registry
            .move_item(
                &local::location(&source),
                &remote("a", "dest"),
                &Context::default(),
            )
            .unwrap_err();
        assert_eq!(
            error.code,
            if change == "cancel" {
                ErrorCode::Cancelled
            } else {
                ErrorCode::Io
            }
        );
        assert!(
            error.message.contains("copy completed"),
            "{}",
            error.message
        );
        assert!(error.message.contains("Source has not been removed"));
        assert!(source.join("file").exists());
        assert_eq!(
            fs::read_to_string(base.join("a/dest/file")).unwrap(),
            "original"
        );
        if change == "child" {
            assert!(source.join("new-child").exists());
        }
    }
}

#[test]
fn late_read_and_destination_write_failures_leave_no_final_file() {
    let tmp = tempfile::tempdir().unwrap();
    let base = tmp.path();
    fs::create_dir(base.join("a")).unwrap();
    fs::create_dir(base.join("b")).unwrap();
    fs::write(base.join("a/file"), vec![9; 2 * 1024 * 1024]).unwrap();
    fs::write(base.join("source"), vec![7; 100]).unwrap();
    let registry = Registry::new(
        base.join("cache"),
        vec![
            Arc::new(Local::default()),
            Arc::new(Account {
                id: "a",
                root: base.join("a"),
                fail_after_read: true,
                ..Default::default()
            }),
            Arc::new(Account {
                id: "b",
                root: base.join("b"),
                fail_write: true,
                ..Default::default()
            }),
        ],
    )
    .unwrap();
    let error = registry
        .move_item(
            &remote("a", "file"),
            &local::location(&base.join("local-dest")),
            &Context::default(),
        )
        .unwrap_err();
    assert_eq!(error.code, ErrorCode::Io);
    assert!(!base.join("local-dest").exists());
    assert_eq!(
        fs::metadata(base.join("a/file")).unwrap().len(),
        2 * 1024 * 1024
    );
    let error = registry
        .move_item(
            &local::location(&base.join("source")),
            &remote("b", "dest"),
            &Context::default(),
        )
        .unwrap_err();
    assert_eq!(error.code, ErrorCode::Io);
    assert!(error.message.contains("destination write failed"));
    assert!(base.join("source").exists());
    assert!(!base.join("b/dest").exists());
    assert_eq!(fs::read_dir(base.join("b")).unwrap().count(), 0);
}

#[test]
fn recursive_roundtrip_across_accounts_and_unique_collisions() {
    let tmp = tempfile::tempdir().unwrap();
    let base = tmp.path();
    let registry = Registry::new(
        base.join("cache"),
        vec![
            Arc::new(Local::default()),
            account(base, "a", false, false, false),
            account(base, "b", false, false, false),
        ],
    )
    .unwrap();
    let ctx = Context::default();
    fs::create_dir_all(base.join("source/nested")).unwrap();
    let bytes = vec![42; 2 * 1024 * 1024 + 17];
    fs::write(base.join("source/nested/é %.bin"), &bytes).unwrap();
    registry
        .copy(
            &local::location(&base.join("source")),
            &remote("a", "tree"),
            &ctx,
        )
        .unwrap();
    registry
        .copy(&remote("a", "tree"), &remote("b", "tree"), &ctx)
        .unwrap();
    registry
        .copy(
            &remote("b", "tree"),
            &local::location(&base.join("result")),
            &ctx,
        )
        .unwrap();
    assert_eq!(fs::read(base.join("result/nested/é %.bin")).unwrap(), bytes);
    let numbered = registry
        .copy_unique(
            &remote("a", "tree/nested/é %.bin"),
            &remote("b", "tree/nested/é %.bin"),
            &ctx,
        )
        .unwrap();
    assert_eq!(numbered, remote("b", "tree/nested/é %-1.bin"));
    assert_eq!(
        registry
            .copy(&remote("a", "tree"), &remote("b", "tree"), &ctx)
            .unwrap_err()
            .code,
        ErrorCode::AlreadyExists
    );
    registry
        .move_item(
            &remote("a", "tree/nested/é %.bin"),
            &remote("b", "moved.bin"),
            &ctx,
        )
        .unwrap();
    assert!(!base.join("a/tree/nested/é %.bin").exists());
    assert_eq!(fs::read(base.join("b/moved.bin")).unwrap(), bytes);
}

#[test]
fn read_only_unknown_failed_move_cancellation_and_pagination() {
    let tmp = tempfile::tempdir().unwrap();
    let base = tmp.path();
    let registry = Registry::new(
        base.join("cache"),
        vec![
            Arc::new(Local::default()),
            account(base, "readonly", true, false, false),
            account(base, "broken", false, true, false),
            account(base, "loop", false, false, true),
        ],
    )
    .unwrap();
    fs::write(base.join("source"), "data").unwrap();
    fs::write(base.join("broken/source"), "data").unwrap();
    let src = local::location(&base.join("source"));
    let ctx = Context::default();
    assert_eq!(
        registry
            .copy(&src, &remote("readonly", "dest"), &ctx)
            .unwrap_err()
            .code,
        ErrorCode::PermissionDenied
    );
    assert!(!registry.capabilities_at(&remote("missing", "")).read);
    assert!(registry.stat(&remote("missing", ""), &ctx).is_err());
    assert_eq!(
        registry
            .move_item(
                &remote("broken", "source"),
                &local::location(&base.join("dest")),
                &ctx
            )
            .unwrap_err()
            .code,
        ErrorCode::Io
    );
    assert!(base.join("broken/source").exists());
    assert!(!base.join("dest").exists());
    ctx.cancel();
    assert_eq!(
        registry
            .copy(&src, &local::location(&base.join("cancelled")), &ctx)
            .unwrap_err()
            .code,
        ErrorCode::Cancelled
    );
    let err = registry
        .copy(
            &remote("loop", ""),
            &local::location(&base.join("loop-copy")),
            &Context::default(),
        )
        .unwrap_err();
    assert_eq!(err.code, ErrorCode::StaleCursor);
    assert!(err.message.contains("partial destination"));
}

#[test]
fn materialization_is_private_and_account_scoped() {
    use std::os::unix::fs::PermissionsExt;
    let tmp = tempfile::tempdir().unwrap();
    let base = tmp.path();
    let registry = Registry::new(
        base.join("cache"),
        vec![
            Arc::new(Local::default()),
            account(base, "a", false, false, false),
            account(base, "b", false, false, false),
        ],
    )
    .unwrap();
    fs::write(base.join("a/same.txt"), "account a").unwrap();
    fs::write(base.join("b/same.txt"), "account b").unwrap();
    let a = registry
        .materialize(&remote("a", "same.txt"), &Context::default())
        .unwrap();
    let b = registry
        .materialize(&remote("b", "same.txt"), &Context::default())
        .unwrap();
    assert_ne!(a, b);
    assert!(a.starts_with(base.join("cache/materialized/a")));
    assert!(b.starts_with(base.join("cache/materialized/b")));
    assert_eq!(fs::read_to_string(a.clone()).unwrap(), "account a");
    assert_eq!(
        fs::metadata(a.parent().unwrap())
            .unwrap()
            .permissions()
            .mode()
            & 0o777,
        0o700
    );
    let local = base.join("a/same.txt");
    assert_eq!(
        registry
            .materialize(&local::location(&local), &Context::default())
            .unwrap(),
        local
    );
}

#[test]
fn byte_preserving_locations_and_bounded_navigation() {
    use std::os::unix::ffi::OsStringExt;
    for p in [
        PathBuf::from("/tmp/é %?#"),
        PathBuf::from(std::ffi::OsString::from_vec(b"/tmp/nonutf8-\xff".to_vec())),
        locations::root("account-a").join("é %?#"),
    ] {
        assert_eq!(locations::path(&locations::location(&p)).unwrap(), p);
        assert_eq!(locations::parse(&locations::display(&p)).unwrap(), p);
    }
    let mut nav = Navigation::new(locations::root("a").join("folder"));
    assert!(nav.parent());
    assert!(!nav.parent());
    assert!(locations::parse("filen://../x").is_err());
    assert!(locations::parse("filen://a/%FF").is_err());
    assert!(locations::parse("filen://a/../escape").is_err());
    assert!(local::path(&remote("a", "x")).is_err());
}

#[test]
fn config_instances_and_secret_references_are_strict() {
    let good = "connectors:\n  instances:\n    - {id: a, name: Account, type: filen, auth: {email: a@example.test, password: {env: TEST_PASSWORD}}}\nfavorites: [{name: Remote, path: 'filen://a/My%20Files'}]";
    let cfg = Config::parse(good).unwrap();
    assert!(cfg.connectors.instances[0].enabled);
    let paths = Paths::resolve(|key| (key == "HOME").then(|| PathBuf::from("/isolated"))).unwrap();
    assert_eq!(
        paths.expand(&cfg.favorites[0].path),
        locations::root("a").join("My Files")
    );
    for invalid in [
        good.replace("id: a", "id: local"),
        good.replace("id: a", "id: ../a"),
        good.replace("env: TEST_PASSWORD", "env: ''"),
        good.replace(
            "env: TEST_PASSWORD",
            "env: TEST_PASSWORD, secret_service: ref",
        ),
        good.replace("env: TEST_PASSWORD", "plaintext: password"),
        good.replace("name: Account", "name: Account, command: []"),
        format!("{good}\n  invalid: true"),
    ] {
        assert!(Config::parse(&invalid).is_err(), "{invalid}");
    }
    assert!(Config::parse("connectors: {instances: [{id: a, name: A, type: filen}]}").is_err());
    assert!(
        Config::parse("connectors: {instances: [{id: a, name: A, type: filen, enabled: false}]}")
            .is_ok()
    );
    assert!(Config::parse("connectors: {reserved: [{id: a, label: A}], instances: [{id: a, name: A, type: filen, enabled: false}]}").is_err());
}

#[test]
fn remote_transfers_reject_links_and_non_utf8_names() {
    use std::os::unix::{ffi::OsStringExt, fs::symlink};
    let tmp = tempfile::tempdir().unwrap();
    let base = tmp.path();
    let registry = Registry::new(
        base.join("cache"),
        vec![
            Arc::new(Local::default()),
            account(base, "a", false, false, false),
        ],
    )
    .unwrap();
    fs::write(base.join("target"), "data").unwrap();
    symlink("target", base.join("link")).unwrap();
    assert_eq!(
        registry
            .copy(
                &local::location(&base.join("link")),
                &remote("a", "link"),
                &Context::default()
            )
            .unwrap_err()
            .code,
        ErrorCode::Unsupported
    );
    let nonutf8 = base.join(std::ffi::OsString::from_vec(b"name-\xff".to_vec()));
    fs::write(&nonutf8, "data").unwrap();
    assert_eq!(
        registry
            .copy(
                &local::location(&nonutf8),
                &remote("a", "valid"),
                &Context::default()
            )
            .unwrap_err()
            .code,
        ErrorCode::InvalidInput
    );
}

#[test]
fn google_drive_configuration_and_uri_persistence_are_provider_aware() {
    let yaml = "connectors:\n  instances:\n    - {id: drive-personal, name: Personal, type: google_drive, oauth: {client_id: desktop.apps.googleusercontent.com, client_secret: {env: DRIVE_CLIENT_SECRET}}}\n    - {id: filen-work, name: Work, type: filen, enabled: false}\nfavorites: [{name: Docs, path: 'drive://drive-personal/My%20Docs'}]";
    let config = Config::parse(yaml).unwrap();
    let oauth = config.connectors.instances[0].oauth.as_ref().unwrap();
    assert!(oauth.persist_session);
    assert!(config.connectors.instances[0].auth.is_none());
    let paths = Paths::resolve(|key| (key == "HOME").then(|| PathBuf::from("/isolated"))).unwrap();
    let favorite = paths.expand(&config.favorites[0].path);
    assert_eq!(favorite, locations::root("drive-personal").join("My Docs"));
    assert_eq!(
        config.display_path(&favorite),
        "drive://drive-personal/My%20Docs"
    );
    assert_eq!(
        config.display_path(&locations::root("filen-work")),
        "filen://filen-work/"
    );
    let saved = serde_yaml_ng::to_string(&config).unwrap();
    let restored = Config::parse(&saved).unwrap();
    assert!(
        config
            .parse_path("filen://drive-personal/My%20Docs")
            .is_err()
    );
    assert!(config.parse_path("drive://filen-work/").is_err());
    assert_eq!(
        config
            .parse_path("drive://drive-personal/My%20Docs")
            .unwrap(),
        favorite
    );
    assert!(
        Config::parse(&yaml.replace(
            "drive://drive-personal/My%20Docs",
            "filen://drive-personal/My%20Docs"
        ))
        .is_err()
    );
    assert_eq!(
        restored.display_path(&favorite),
        config.display_path(&favorite)
    );
    for uri in [
        "drive://../x",
        "drive://drive-personal/%FF",
        "drive://drive-personal/../escape",
    ] {
        assert!(locations::parse(uri).is_err());
    }
    for bad in [
        yaml.replace("oauth: {client_id: desktop.apps.googleusercontent.com, client_secret: {env: DRIVE_CLIENT_SECRET}}", "auth: {email: user@example.test, password: {env: PASSWORD}}"),
        yaml.replace("client_id: desktop.apps.googleusercontent.com", "client_id: ''"),
        yaml.replace("{env: DRIVE_CLIENT_SECRET}", "SYNTHETIC_SECRET"),
        yaml.replace("id: filen-work", "id: drive-personal"),
    ] {
        let error = Config::parse(&bad).unwrap_err();
        assert!(!error.contains("SYNTHETIC_SECRET"));
    }
    assert!(
        Config::parse("connectors: {instances: [{id: drive, name: Drive, type: google_drive}]}")
            .is_err()
    );
    assert!(Config::parse("connectors: {instances: [{id: drive, name: Drive, type: google_drive, enabled: false}]}").is_ok());
}

#[test]
fn cancellation_during_remote_move_keeps_source() {
    use std::sync::atomic::{AtomicBool, Ordering};
    let tmp = tempfile::tempdir().unwrap();
    let base = tmp.path();
    let registry = Registry::new(
        base.join("cache"),
        vec![
            Arc::new(Local::default()),
            account(base, "a", false, false, false),
        ],
    )
    .unwrap();
    fs::write(base.join("a/large"), vec![8; 3 * 1024 * 1024]).unwrap();
    let cancelled = Arc::new(AtomicBool::new(false));
    let flag = cancelled.clone();
    let mut ctx = Context::new(move |progress| {
        if progress.bytes > 0 {
            flag.store(true, Ordering::Relaxed);
        }
    });
    ctx.cancelled = cancelled;
    let error = registry
        .move_item(
            &remote("a", "large"),
            &local::location(&base.join("dest")),
            &ctx,
        )
        .unwrap_err();
    assert_eq!(error.code, ErrorCode::Cancelled);
    assert!(base.join("a/large").exists());
    assert!(!base.join("dest").exists());
}

#[test]
fn recursive_move_failure_reports_partial_copy_and_keeps_source() {
    use std::os::unix::fs::symlink;
    let tmp = tempfile::tempdir().unwrap();
    let base = tmp.path();
    let registry = Registry::new(
        base.join("cache"),
        vec![
            Arc::new(Local::default()),
            account(base, "a", false, false, false),
        ],
    )
    .unwrap();
    fs::create_dir(base.join("source")).unwrap();
    fs::write(base.join("source/file"), "keep").unwrap();
    symlink("file", base.join("source/link")).unwrap();
    let error = registry
        .move_item(
            &local::location(&base.join("source")),
            &remote("a", "dest"),
            &Context::default(),
        )
        .unwrap_err();
    assert_eq!(error.code, ErrorCode::Unsupported);
    assert!(error.message.contains("partial destination"));
    assert!(base.join("source/file").exists());
    assert!(
        base.join("source/link")
            .symlink_metadata()
            .unwrap()
            .is_symlink()
    );
    assert!(base.join("a/dest").is_dir());
}
