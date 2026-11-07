use serde::{Deserialize, Serialize};
use std::{
    collections::BTreeMap,
    env, fs,
    io::Write,
    path::{Path, PathBuf},
};

#[derive(Debug, thiserror::Error)]
pub enum ConfigError {
    #[error("Cannot read configuration {path}: {source}")]
    Read {
        path: PathBuf,
        source: std::io::Error,
    },
    #[error("Invalid configuration {path}: {message}")]
    Invalid { path: PathBuf, message: String },
}

#[derive(Clone, Debug)]
pub struct Paths {
    pub home: PathBuf,
    pub config: PathBuf,
    pub state: PathBuf,
    pub cache: PathBuf,
    pub config_home: PathBuf,
}

impl Paths {
    pub fn from_env() -> Result<Self, String> {
        Self::resolve(|key| env::var_os(key).map(PathBuf::from))
    }

    pub fn resolve(get: impl Fn(&str) -> Option<PathBuf>) -> Result<Self, String> {
        let home = get("HOME")
            .filter(|p| p.is_absolute())
            .ok_or("HOME must be an absolute path")?;
        let xdg =
            |key: &str, fallback: PathBuf| get(key).filter(|p| p.is_absolute()).unwrap_or(fallback);
        let config_home = xdg("XDG_CONFIG_HOME", home.join(".config"));
        let config = match get("NIRI_TONIC_FILES_CONFIG_FILE") {
            Some(p) if p.as_os_str().is_empty() => {
                return Err("NIRI_TONIC_FILES_CONFIG_FILE is empty".into());
            }
            Some(p) => p,
            None => config_home.join("niri-tonic-files/config.yaml"),
        };
        Ok(Self {
            config,
            config_home,
            state: xdg("XDG_STATE_HOME", home.join(".local/state")).join("niri-tonic-files"),
            cache: xdg("XDG_CACHE_HOME", home.join(".cache")).join("niri-tonic-files"),
            home,
        })
    }

    pub fn expand(&self, path: &Path) -> PathBuf {
        if let Some(uri) = path
            .to_str()
            .filter(|s| s.starts_with("file:///") || crate::locations::is_remote_uri(s))
            && let Ok(path) = crate::locations::parse(uri)
        {
            return path;
        }
        if path == Path::new("~") {
            self.home.clone()
        } else if let Ok(rest) = path.strip_prefix("~/") {
            self.home.join(rest)
        } else {
            path.to_path_buf()
        }
    }
}

#[derive(Clone, Debug, Serialize, Deserialize)]
#[serde(default, deny_unknown_fields)]
pub struct Config {
    pub version: u32,
    pub appearance: Appearance,
    pub favorites: Vec<Favorite>,
    pub associations: Associations,
    pub navigation: Navigation,
    pub view: View,
    pub connectors: Connectors,
}

impl Default for Config {
    fn default() -> Self {
        Self {
            version: 1,
            appearance: Appearance::default(),
            favorites: vec![],
            associations: Associations::default(),
            navigation: Navigation::default(),
            view: View::default(),
            connectors: Connectors::default(),
        }
    }
}

#[derive(Clone, Debug, Serialize, Deserialize)]
#[serde(default, deny_unknown_fields)]
pub struct Appearance {
    /// "nord" or an absolute path to a theme YAML file.
    pub theme: String,
    pub font: Option<String>,
    pub monospace_font: Option<String>,
    pub font_size: Option<u16>,
    pub follow_dank: bool,
}
impl Default for Appearance {
    fn default() -> Self {
        Self {
            theme: "nord".into(),
            font: None,
            monospace_font: None,
            font_size: None,
            follow_dank: true,
        }
    }
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Favorite {
    pub name: String,
    pub path: PathBuf,
}

#[derive(Clone, Debug, Default, Serialize, Deserialize)]
#[serde(default, deny_unknown_fields)]
pub struct Associations {
    pub extensions: BTreeMap<String, Vec<String>>,
    pub mime_types: BTreeMap<String, Vec<String>>,
}

#[derive(Clone, Debug, Serialize, Deserialize)]
#[serde(default, deny_unknown_fields)]
pub struct Navigation {
    pub animations: bool,
    pub animation_ms: u32,
    pub restore_session: bool,
}
impl Default for Navigation {
    fn default() -> Self {
        Self {
            animations: true,
            animation_ms: 220,
            restore_session: false,
        }
    }
}

#[derive(Clone, Copy, Debug, Default, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum Sort {
    #[default]
    Name,
    Modified,
    Size,
}

#[derive(Clone, Debug, Serialize, Deserialize)]
#[serde(default, deny_unknown_fields)]
pub struct View {
    pub hidden: bool,
    pub sort: Sort,
    pub descending: bool,
    pub folders_first: bool,
}
impl Default for View {
    fn default() -> Self {
        Self {
            hidden: false,
            sort: Sort::Name,
            descending: false,
            folders_first: true,
        }
    }
}

#[derive(Clone, Debug, Default, Serialize, Deserialize)]
#[serde(default, deny_unknown_fields)]
pub struct Connectors {
    pub local: LocalConfig,
    pub reserved: Vec<ReservedConnector>,
    pub instances: Vec<ConnectorInstance>,
}

#[derive(Clone, Debug, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct ConnectorInstance {
    pub id: String,
    pub name: String,
    #[serde(rename = "type")]
    pub kind: ConnectorKind,
    #[serde(default = "enabled_default")]
    pub enabled: bool,
    #[serde(default)]
    pub read_only: bool,
    pub auth: Option<FilenAuth>,
    pub oauth: Option<GoogleDriveAuth>,
    pub command: Option<Vec<String>>,
}
fn enabled_default() -> bool {
    true
}
#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum ConnectorKind {
    Filen,
    GoogleDrive,
}
impl ConnectorKind {
    pub fn label(self) -> &'static str {
        match self {
            Self::Filen => "Filen",
            Self::GoogleDrive => "Google Drive",
        }
    }
    pub fn scheme(self) -> &'static str {
        match self {
            Self::Filen => "filen",
            Self::GoogleDrive => "drive",
        }
    }
}
#[derive(Clone, Debug, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct GoogleDriveAuth {
    pub client_id: String,
    pub client_secret: Option<SecretRef>,
    #[serde(default = "enabled_default")]
    pub persist_session: bool,
}
#[derive(Clone, Debug, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct FilenAuth {
    pub email: String,
    pub password: SecretRef,
    pub two_factor_code: Option<SecretRef>,
}
#[derive(Clone, Serialize)]
pub struct SecretRef {
    #[serde(skip_serializing_if = "Option::is_none")]
    pub value: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub env: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub secret_service: Option<String>,
}
impl std::fmt::Debug for SecretRef {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        formatter
            .debug_struct("SecretRef")
            .field("value", &self.value.as_ref().map(|_| "[REDACTED]"))
            .field("env", &self.env)
            .field("secret_service", &self.secret_service)
            .finish()
    }
}
impl<'de> Deserialize<'de> for SecretRef {
    fn deserialize<D: serde::Deserializer<'de>>(deserializer: D) -> Result<Self, D::Error> {
        use serde::de::Error as _;
        let value = serde_yaml_ng::Value::deserialize(deserializer)?;
        let map = value.as_mapping().ok_or_else(|| {
            D::Error::custom("Credentials require a value, env or secret_service reference mapping")
        })?;
        let mut reference = Self {
            value: None,
            env: None,
            secret_service: None,
        };
        for (key, value) in map {
            let field = match key.as_str() {
                Some("value") => &mut reference.value,
                Some("env") => &mut reference.env,
                Some("secret_service") => &mut reference.secret_service,
                _ => return Err(D::Error::custom("Unknown credential reference field")),
            };
            *field = match value {
                serde_yaml_ng::Value::Null => None,
                serde_yaml_ng::Value::String(value) => Some(value.clone()),
                _ => {
                    return Err(D::Error::custom(
                        "Credential values and references must be strings",
                    ));
                }
            };
        }
        reference.validate().map_err(D::Error::custom)?;
        Ok(reference)
    }
}
impl SecretRef {
    pub fn validate(&self) -> Result<(), String> {
        match (&self.value, &self.env, &self.secret_service) {
            (Some(value), None, None) if !value.is_empty() && !value.contains('\0') => Ok(()),
            (None, Some(value), None) | (None, None, Some(value))
                if !value.trim().is_empty() && !value.contains('\0') =>
            {
                Ok(())
            }
            _ => Err(
                "Secrets require exactly one nonempty value, env or secret_service reference"
                    .into(),
            ),
        }
    }
}

#[derive(Clone, Debug, Default, Serialize, Deserialize)]
#[serde(default, deny_unknown_fields)]
pub struct LocalConfig {
    pub read_only: bool,
}

/// Descriptive slots only. No executable or credential is loaded by the MVP.
#[derive(Clone, Debug, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct ReservedConnector {
    pub id: String,
    pub label: String,
    pub credential_ref: Option<String>,
}

impl Config {
    /// Preserve the provider qualification until it can be checked against the
    /// configured instance. Backend locations themselves are scoped only by ID.
    pub fn parse_path(&self, text: &str) -> crate::connector::Result<PathBuf> {
        let path = crate::locations::parse(text)?;
        if crate::locations::is_remote_uri(text) {
            let id = crate::locations::location(&path).connector;
            if let Some(instance) = self.connectors.instances.iter().find(|i| i.id == id)
                && !text.starts_with(&format!("{}://", instance.kind.scheme()))
            {
                return Err(crate::connector::Error::new(
                    crate::connector::ErrorCode::InvalidInput,
                    "URI provider does not match the configured connector instance",
                ));
            }
        }
        Ok(path)
    }
    pub fn display_path(&self, path: &Path) -> String {
        let id = crate::locations::location(path).connector;
        let scheme = self
            .connectors
            .instances
            .iter()
            .find(|i| i.id == id)
            .map(|i| i.kind.scheme())
            .unwrap_or("filen");
        crate::locations::display_with_scheme(path, scheme)
    }
    pub fn load(path: &Path) -> Result<Self, ConfigError> {
        match fs::read_to_string(path) {
            Ok(text) => Self::parse(&text).map_err(|message| ConfigError::Invalid {
                path: path.into(),
                message,
            }),
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => Ok(Self::default()),
            Err(source) => Err(ConfigError::Read {
                path: path.into(),
                source,
            }),
        }
    }

    pub fn parse(text: &str) -> Result<Self, String> {
        let config: Self = serde_yaml_ng::from_str(text).map_err(|e| e.to_string())?;
        if config.version != 1 {
            return Err(format!(
                "Unsupported version {}; expected version: 1",
                config.version
            ));
        }
        if config.navigation.animation_ms > 2000 {
            return Err("navigation.animation_ms must be between 0 and 2000".into());
        }
        if config
            .appearance
            .font_size
            .is_some_and(|n| !(8..=36).contains(&n))
        {
            return Err("appearance.font_size must be between 8 and 36 points".into());
        }
        for value in [&config.appearance.font, &config.appearance.monospace_font]
            .into_iter()
            .flatten()
        {
            if value.is_empty() || value.chars().any(char::is_control) {
                return Err("Font names must be nonempty and contain no control characters".into());
            }
        }
        if config.appearance.theme != "nord" && !Path::new(&config.appearance.theme).is_absolute() {
            return Err("appearance.theme must be 'nord' or an absolute theme file path".into());
        }
        for favorite in &config.favorites {
            if favorite.name.is_empty()
                || !(favorite.path.is_absolute()
                    || favorite.path.starts_with("~")
                    || favorite.path.to_str().is_some_and(|p| {
                        (p.starts_with("file:///") || crate::locations::is_remote_uri(p))
                            && config.parse_path(p).is_ok()
                    }))
            {
                return Err(
                    "Favorites need a name and an absolute path, ~/ path or file/filen/drive URI"
                        .into(),
                );
            }
        }
        for (key, command) in config
            .associations
            .extensions
            .iter()
            .chain(&config.associations.mime_types)
        {
            if key.is_empty()
                || command.is_empty()
                || command[0].is_empty()
                || command.iter().any(|arg| arg.contains('\0'))
            {
                return Err(format!(
                    "Association '{key}' needs a nonempty argv list without NUL characters"
                ));
            }
            if command
                .iter()
                .any(|arg| arg.contains('{') || arg.contains('}'))
            {
                return Err(format!(
                    "Association '{key}': paths are appended automatically; placeholders are not supported"
                ));
            }
        }
        for extension in config.associations.extensions.keys() {
            if extension.starts_with('.')
                || extension.contains('/')
                || extension.to_lowercase() != *extension
            {
                return Err(format!(
                    "Extension '{extension}' must be lowercase without a leading dot"
                ));
            }
        }
        let mut ids = std::collections::HashSet::new();
        for slot in &config.connectors.reserved {
            if !crate::locations::valid_id(&slot.id)
                || !ids.insert(&slot.id)
                || slot.label.is_empty()
            {
                return Err(
                    "Reserved connectors need unique non-local IDs and nonempty labels".into(),
                );
            }
            if slot.credential_ref.as_ref().is_some_and(|r| {
                !r.starts_with("secret-service:") || r.len() == "secret-service:".len()
            }) {
                return Err("credential_ref must be null or a secret-service: reference, never a password or token".into());
            }
        }
        for instance in &config.connectors.instances {
            if !crate::locations::valid_id(&instance.id)
                || !ids.insert(&instance.id)
                || instance.name.trim().is_empty()
            {
                return Err(
                    "Connector instances need unique safe non-local IDs and nonempty names".into(),
                );
            }
            match instance.kind {
                ConnectorKind::Filen => {
                    if instance.oauth.is_some() {
                        return Err("Filen uses auth, not oauth".into());
                    }
                    if instance.enabled && instance.auth.is_none() {
                        return Err(format!("Connector '{}' requires auth", instance.id));
                    }
                }
                ConnectorKind::GoogleDrive => {
                    if instance.auth.is_some() {
                        return Err("Google Drive uses oauth, not password authentication".into());
                    }
                    if instance.enabled && instance.oauth.is_none() {
                        return Err(format!("Connector '{}' requires oauth", instance.id));
                    }
                }
            }
            if let Some(oauth) = &instance.oauth {
                if oauth.client_id.trim().is_empty()
                    || oauth.client_id.chars().any(char::is_control)
                {
                    return Err("Google Drive OAuth client_id must be nonempty and contain no control characters".into());
                }
                if let Some(secret) = &oauth.client_secret {
                    secret.validate()?;
                }
            }
            if let Some(auth) = &instance.auth {
                if auth.email.trim().is_empty() || auth.email.contains('\0') {
                    return Err("Filen email must be nonempty".into());
                }
                auth.password.validate()?;
                if let Some(code) = &auth.two_factor_code {
                    code.validate()?;
                }
            }
            if let Some(command) = &instance.command
                && (command.is_empty()
                    || command[0].trim().is_empty()
                    || command.iter().any(|arg| arg.contains('\0')))
            {
                return Err(
                    "Connector command must be a nonempty argv list without NUL characters".into(),
                );
            }
        }
        Ok(config)
    }
}

/// Session paths are stored as file URIs to preserve non-UTF-8 names.
#[derive(Clone, Debug, Default, Serialize, Deserialize)]
#[serde(default)]
pub struct Session {
    pub version: u32,
    pub columns: Vec<String>,
    pub active: usize,
}

pub fn atomic_write(path: &Path, bytes: &[u8]) -> Result<(), String> {
    // Preserve a user's symlinked configuration rather than replacing the link.
    let target = if fs::symlink_metadata(path).is_ok_and(|m| m.is_symlink()) {
        fs::canonicalize(path).map_err(|e| e.to_string())?
    } else {
        path.to_path_buf()
    };
    let path = target.as_path();
    let parent = path
        .parent()
        .filter(|p| !p.as_os_str().is_empty())
        .unwrap_or(Path::new("."));
    fs::create_dir_all(parent).map_err(|e| e.to_string())?;
    let mut file = tempfile::NamedTempFile::new_in(parent).map_err(|e| e.to_string())?;
    file.write_all(bytes)
        .and_then(|_| file.as_file().sync_all())
        .map_err(|e| e.to_string())?;
    file.persist(path).map_err(|e| e.to_string())?;
    Ok(())
}
