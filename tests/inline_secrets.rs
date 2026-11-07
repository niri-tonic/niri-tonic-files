use niri_tonic_files::{
    config::{Config, Paths},
    connector::{Connector, Context, ErrorCode, Location},
    remote::RemoteConnector,
};
use serde_json::json;

const PASSWORD: &str = "  SYNTHETIC_PASSWORD:'\"\\$\n☃  ";
const CLIENT_SECRET: &str = "SYNTHETIC_CLIENT_SECRET:#'\"\\\n";

fn configuration() -> Config {
    let fixture = format!("{}/tests/inline_secrets_rpc.py", env!("CARGO_MANIFEST_DIR"));
    Config::parse(
        &serde_yaml_ng::to_string(&json!({
            "connectors": {"instances": [
                {"id":"filen", "name":"Filen", "type":"filen",
                 "auth":{"email":"fixture@example.invalid", "password":{"value":PASSWORD},
                         "two_factor_code":{"value":"012345"}},
                 "command":["python3",fixture]},
                {"id":"drive", "name":"Drive", "type":"google_drive",
                 "oauth":{"client_id":"synthetic-client", "client_secret":{"value":CLIENT_SECRET},
                          "persist_session":false},
                 "command":["python3",fixture]}
            ]}
        }))
        .unwrap(),
    )
    .unwrap()
}

#[test]
fn inline_values_roundtrip_exactly_but_debug_is_redacted() {
    let config = configuration();
    let debug = format!("{config:?}");
    for forbidden in ["SYNTHETIC_PASSWORD", "SYNTHETIC_CLIENT_SECRET", "012345"] {
        assert!(!debug.contains(forbidden));
    }
    assert!(debug.contains("[REDACTED]"));
    let restored = Config::parse(&serde_yaml_ng::to_string(&config).unwrap()).unwrap();
    let filen = restored.connectors.instances[0].auth.as_ref().unwrap();
    assert_eq!(filen.password.value.as_deref(), Some(PASSWORD));
    assert_eq!(
        filen.two_factor_code.as_ref().unwrap().value.as_deref(),
        Some("012345")
    );
    assert_eq!(
        restored.connectors.instances[1]
            .oauth
            .as_ref()
            .unwrap()
            .client_secret
            .as_ref()
            .unwrap()
            .value
            .as_deref(),
        Some(CLIENT_SECRET)
    );
}

#[test]
fn ambiguous_or_malformed_inline_credentials_do_not_echo_values() {
    for value in [
        json!({"value": "SYNTHETIC_SECRET", "env": "UNUSED"}),
        json!({"value": "SYNTHETIC_SECRET", "secret_service": "unused"}),
        json!({"value": {"nested": "SYNTHETIC_SECRET"}}),
        json!({"value": ["SYNTHETIC_SECRET"]}),
        json!({"value": ""}),
        json!({"value": "SYNTHETIC_SECRET\0"}),
        json!({"value": 123456}),
        json!({"unknown": "SYNTHETIC_SECRET"}),
    ] {
        let yaml = serde_yaml_ng::to_string(&json!({"connectors":{"instances":[{
            "id":"a", "name":"A", "type":"filen",
            "auth":{"email":"fixture@example.invalid", "password":value}
        }]}}))
        .unwrap();
        let error = Config::parse(&yaml).unwrap_err();
        assert!(!error.contains("SYNTHETIC_SECRET"));
        assert!(!error.contains("123456"));
    }
}

#[test]
fn both_providers_send_inline_values_only_through_the_private_pipe() {
    let temp = tempfile::tempdir().unwrap();
    let paths = Paths::resolve(|key| (key == "HOME").then(|| temp.path().to_path_buf())).unwrap();
    for instance in &configuration().connectors.instances {
        let connector = RemoteConnector::new(instance, &paths).unwrap();
        let root = Location {
            connector: instance.id.clone(),
            id: "file:///".into(),
        };
        assert!(
            connector
                .list(&root, None, &Context::default())
                .unwrap()
                .entries
                .is_empty()
        );
        let error = connector.stat(&root, &Context::default()).unwrap_err();
        assert_eq!(error.code, ErrorCode::AuthenticationRequired);
        assert!(!error.message.contains("SYNTHETIC"));
        assert!(!error.message.contains("012345"));
    }
    assert!(!paths.state.exists());
}
