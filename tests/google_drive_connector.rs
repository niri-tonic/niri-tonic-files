use niri_tonic_files::{
    config::{ConnectorInstance, Paths},
    connector::*,
    remote::RemoteConnector,
};
use std::{
    process::Command,
    sync::{Arc, Mutex},
};

fn at(id: &str, path: &str) -> Location {
    Location {
        connector: id.into(),
        id: format!("file://{path}"),
    }
}

#[test]
fn canonical_state_ancestors_preserve_lazy_and_adapter_owned_leaves() {
    use std::os::unix::fs::symlink;
    let root = tempfile::tempdir().unwrap();
    let real_home = root.path().join("real-home");
    std::fs::create_dir(&real_home).unwrap();
    let home = root.path().join("home");
    symlink(&real_home, &home).unwrap();
    let canonical_home = real_home.canonicalize().unwrap();
    let state = canonical_home.join(".local/state/niri-tonic-files");
    let marker = root.path().join("launched");
    let script = r#"
import json, pathlib, sys
pathlib.Path(sys.argv[2]).touch()
for line in sys.stdin:
    f = json.loads(line)
    op = f['op']
    if op == 'shutdown': break
    if op == 'hello': result = {'connector': 'fixture'}
    elif op == 'login':
        assert f['args']['token_path'] == sys.argv[1]
        result = None
    else:
        result = dict(location=dict(connector='fixture', id='file:///'), name='/',
                      kind='directory', navigable=True, hidden=False, size=0,
                      modified=None, revision=None, read_only=False)
    print(json.dumps(dict(protocol=1, id=f['id'], result=result)), flush=True)
"#;
    let instance: ConnectorInstance = serde_json::from_value(serde_json::json!({
        "id":"fixture", "name":"fixture", "type":"google_drive",
        "oauth":{"client_id":"synthetic-client"},
        "command":["python3", "-c", script, state.join("oauth/fixture/session.json"), marker]
    }))
    .unwrap();
    let mut paths = Paths {
        home: home.clone(),
        config: root.path().into(),
        state: home.join(".local/state/niri-tonic-files"),
        cache: root.path().into(),
        config_home: root.path().into(),
    };
    let connector = RemoteConnector::new(&instance, &paths).unwrap();
    assert!(!marker.exists(), "constructor launched the adapter");
    assert!(
        !real_home.join(".local").exists(),
        "constructor created state ancestors"
    );
    connector
        .stat(&at("fixture", "/"), &Context::default())
        .unwrap();
    drop(connector);
    assert!(!state.exists(), "host touched token storage during login");

    // An explicitly selected state-directory symlink is allowed, but token
    // leaves must reach the adapter unchanged, even when already symlinked.
    std::fs::create_dir_all(state.join("oauth/fixture")).unwrap();
    let token_target = root.path().join("synthetic-token");
    std::fs::write(&token_target, "untouched").unwrap();
    symlink(&token_target, state.join("oauth/fixture/session.json")).unwrap();
    let state_link = root.path().join("state-link");
    symlink(&state, &state_link).unwrap();
    paths.state = state_link;
    std::fs::remove_file(&marker).unwrap();
    let connector = RemoteConnector::new(&instance, &paths).unwrap();
    assert!(!marker.exists());
    connector
        .stat(&at("fixture", "/"), &Context::default())
        .unwrap();
    assert_eq!(std::fs::read_to_string(&token_target).unwrap(), "untouched");

    paths.state = token_target.join("child");
    assert!(RemoteConnector::new(&instance, &paths).is_err());
    paths.state = root.path().join("dangling-state");
    symlink(root.path().join("absent"), &paths.state).unwrap();
    assert!(RemoteConnector::new(&instance, &paths).is_err());
}

#[test]
fn isolated_oauth_accounts_and_browser_environment() {
    // Set desktop settings in an isolated test process, without mutating the
    // environment of the parallel Rust test runner.
    if std::env::var_os("DRIVE_FIXTURE_CHILD").is_none() {
        let status = Command::new(std::env::current_exe().unwrap())
            .args([
                "--exact",
                "isolated_oauth_accounts_and_browser_environment",
                "--nocapture",
            ])
            .env("DRIVE_FIXTURE_CHILD", "1")
            .env(
                "DRIVE_FIXTURE_CLIENT_SECRET",
                "SYNTHETIC_DRIVE_CLIENT_SECRET",
            )
            .env("DISPLAY", ":fixture")
            .env("WAYLAND_DISPLAY", "fixture-wayland")
            .env("XDG_RUNTIME_DIR", "/fixture-runtime")
            .env("DBUS_SESSION_BUS_ADDRESS", "unix:path=/fixture-bus")
            .env("XDG_CURRENT_DESKTOP", "fixture-desktop")
            .env("UNRELATED_ACCOUNT_SECRET", "SYNTHETIC_UNRELATED_SECRET")
            .status()
            .unwrap();
        assert!(status.success());
        return;
    }
    let root = tempfile::tempdir().unwrap();
    let state = root.path().join("state");
    let paths = Paths {
        home: root.path().into(),
        config: root.path().into(),
        state: state.clone(),
        cache: root.path().into(),
        config_home: root.path().into(),
    };
    let make = |id: &str, confidential: bool| {
        let mut oauth =
            serde_json::json!({"client_id":format!("client-{id}"), "persist_session":confidential});
        if confidential {
            oauth["client_secret"] = serde_json::json!({"env":"DRIVE_FIXTURE_CLIENT_SECRET"});
        }
        let instance: ConnectorInstance = serde_json::from_value(serde_json::json!({
            "id":id,"name":id,"type":"google_drive","enabled":true,
            "oauth":oauth,
            "command":["python3",format!("{}/tests/google_drive_rpc.py",env!("CARGO_MANIFEST_DIR")),state]
        })).unwrap();
        RemoteConnector::new(&instance, &paths).unwrap()
    };
    let a = make("personal", true);
    let b = make("work", false);
    assert!(!state.exists(), "construction touched token storage");
    assert!(matches!(
        a.authentication(),
        Authentication::Required { .. }
    ));
    let phases = Arc::new(Mutex::new(Vec::new()));
    let seen = phases.clone();
    let ctx = Context::new(move |progress| seen.lock().unwrap().push(progress.phase));
    let first = a.stat(&at("personal", "/"), &ctx).unwrap();
    assert!(matches!(a.authentication(), Authentication::Ready));
    assert_eq!(first.size, a.stat(&at("personal", "/"), &ctx).unwrap().size);
    assert_ne!(first.size, b.stat(&at("work", "/"), &ctx).unwrap().size);
    assert!(
        phases
            .lock()
            .unwrap()
            .iter()
            .all(|phase| phase == "Authenticating remote account")
    );
    assert_eq!(
        a.stat(&at("work", "/"), &ctx).unwrap_err().code,
        ErrorCode::InvalidInput
    );
    assert_eq!(
        a.stat(&at("personal", "/a/../b"), &ctx).unwrap_err().code,
        ErrorCode::InvalidInput
    );
    assert_eq!(
        a.read(&at("personal", "/"), 0, 1048577, &ctx)
            .unwrap_err()
            .code,
        ErrorCode::InvalidInput
    );
    assert_eq!(
        a.delete(&at("personal", "/"), DeleteMode::Permanent, &ctx)
            .unwrap_err()
            .code,
        ErrorCode::PermissionDenied
    );
    let error = a
        .create_directory(&at("personal", "/ambiguous"), &ctx)
        .unwrap_err();
    assert_eq!(error.code, ErrorCode::Io);
    assert!(error.message.contains("Google Drive"));
    assert!(!error.message.contains("SYNTHETIC"));
    assert!(matches!(
        a.authentication(),
        Authentication::Required { .. }
    ));
    assert_ne!(first.size, a.stat(&at("personal", "/"), &ctx).unwrap().size);
    assert_eq!(
        std::fs::read_to_string(state.join("mutation-count")).unwrap(),
        "1"
    );
    for path in ["/wrong-id", "/leak"] {
        let error = b.stat(&at("work", path), &ctx).unwrap_err();
        assert_eq!(error.code, ErrorCode::Io);
        assert!(error.message.contains("Google Drive"));
        assert!(!error.message.contains("SYNTHETIC"));
    }
    assert!(
        !state.join("oauth").exists(),
        "host touched adapter-owned token storage"
    );
}
