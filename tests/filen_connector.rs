use niri_tonic_files::{
    config::{ConnectorInstance, Paths},
    connector::*,
    filen::Filen,
};
use std::{
    path::PathBuf,
    sync::{Arc, Mutex},
    time::Instant,
};

fn connector(id: &str, read_only: bool) -> Filen {
    let instance: ConnectorInstance = serde_json::from_value(serde_json::json!({
        "id":id,"name":id,"type":"filen","enabled":true,"read_only":read_only,
        "auth":{"email":"fixture@example.invalid","password":{"env":"CARGO_MANIFEST_DIR"}},
        "command":["python3",format!("{}/tests/filen_rpc.py",env!("CARGO_MANIFEST_DIR"))]
    }))
    .unwrap();
    let root = PathBuf::from("/unused");
    let paths = Paths {
        home: root.clone(),
        config: root.clone(),
        state: root.clone(),
        cache: root.clone(),
        config_home: root,
    };
    Filen::new(&instance, &paths).unwrap()
}
fn at(id: &str, path: &str) -> Location {
    Location {
        connector: id.into(),
        id: format!("file://{path}"),
    }
}

#[test]
fn lazy_isolated_persistent_sessions_and_private_stream_staging() {
    let a = connector("personal", false);
    let b = connector("work", false);
    let ctx = Context::default();
    assert!(matches!(
        a.authentication(),
        Authentication::Required { .. }
    ));
    let first = a.stat(&at("personal", "/"), &ctx).unwrap();
    assert!(matches!(a.authentication(), Authentication::Ready));
    assert_eq!(first.size, a.stat(&at("personal", "/"), &ctx).unwrap().size);
    let second = b.stat(&at("work", "/"), &ctx).unwrap();
    assert_ne!(first.size, second.size);
    assert_eq!(
        a.stat(&at("work", "/"), &ctx).unwrap_err().code,
        ErrorCode::InvalidInput
    );
    let data = vec![42; 2 * 1024 * 1024 + 17];
    a.write_new(&at("personal", "/stream"), &mut data.as_slice(), &ctx)
        .unwrap();
    assert_eq!(
        a.read(&at("personal", "/stream"), 1024 * 1024, 1024 * 1024, &ctx)
            .unwrap(),
        vec![42; 1024 * 1024]
    );
    assert_eq!(
        a.read(&at("personal", "/stream"), 0, 1048577, &ctx)
            .unwrap_err()
            .code,
        ErrorCode::InvalidInput
    );
}

#[test]
fn malformed_frames_redacted_errors_and_reconnect_without_replay() {
    let a = connector("personal", false);
    let ctx = Context::default();
    let initial = a.stat(&at("personal", "/"), &ctx).unwrap().size;
    for path in ["/malformed", "/wrong-version", "/leak"] {
        let error = a.stat(&at("personal", path), &ctx).unwrap_err();
        assert_eq!(error.code, ErrorCode::Io);
        assert!(!error.message.contains("SYNTHETIC_SECRET"));
        assert!(matches!(
            a.authentication(),
            Authentication::Required { .. }
        ));
    }
    assert_ne!(initial, a.stat(&at("personal", "/"), &ctx).unwrap().size);
    assert_eq!(
        a.stat(&at("personal", "/expired"), &ctx).unwrap_err().code,
        ErrorCode::AuthenticationRequired
    );
}

#[test]
fn cancellation_progress_and_preflight_validation() {
    let a = connector("personal", false);
    let holder: Arc<Mutex<Option<Context>>> = Arc::new(Mutex::new(None));
    let callback = holder.clone();
    let ctx = Context::new(move |progress| {
        assert_eq!(progress.phase, "Waiting");
        callback.lock().unwrap().as_ref().unwrap().cancel();
    });
    *holder.lock().unwrap() = Some(ctx.clone());
    let start = Instant::now();
    assert_eq!(
        a.stat(&at("personal", "/wait"), &ctx).unwrap_err().code,
        ErrorCode::Cancelled
    );
    assert!(start.elapsed().as_secs() < 5);
    *holder.lock().unwrap() = None;
    let ctx = Context::default();
    for path in ["/%FF", "/a/../b", "/a//b", "/a%00", "/a/"] {
        assert_eq!(
            a.stat(&at("personal", path), &ctx).unwrap_err().code,
            ErrorCode::InvalidInput
        );
    }
    assert_eq!(
        a.delete(&at("personal", "/"), DeleteMode::Permanent, &ctx)
            .unwrap_err()
            .code,
        ErrorCode::PermissionDenied
    );
    let ro = connector("readonly", true);
    assert_eq!(
        ro.create_directory(&at("readonly", "/new"), &ctx)
            .unwrap_err()
            .code,
        ErrorCode::PermissionDenied
    );
    assert!(matches!(
        ro.authentication(),
        Authentication::Required { .. }
    ));
}
