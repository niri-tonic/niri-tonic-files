//! Shared subprocess host for Filen and Google Drive adapters.
//! One lazily authenticated subprocess per configured account; worker-thread only.
use crate::config::{ConnectorInstance, ConnectorKind, Paths, SecretRef};
use crate::connector::*;
use serde::de::DeserializeOwned;
use serde_json::{Value, json};
use std::{
    io::{BufRead, BufReader, Read, Write},
    path::PathBuf,
    process::{Child, Command, Stdio},
    sync::{
        Mutex, TryLockError,
        atomic::{AtomicBool, Ordering},
        mpsc,
    },
    thread,
    time::{Duration, Instant},
};

pub struct RemoteConnector {
    instance: ConnectorInstance,
    token_path: PathBuf,
    command: Vec<String>,
    process: Mutex<Option<Process>>,
    ready: AtomicBool,
}

fn failure(message: &str) -> Error {
    Error::new(ErrorCode::Io, message)
}

struct Process {
    provider: &'static str,
    child: Child,
    input: mpsc::Sender<Vec<u8>>,
    frames: mpsc::Receiver<std::result::Result<Value, ()>>,
    next: u64,
}
impl Drop for Process {
    fn drop(&mut self) {
        let _ = self.send(json!({"protocol":1,"id":0,"op":"shutdown","args":{}}));
        // Give the adapter time to remove private transfer staging files.
        let deadline = Instant::now() + Duration::from_millis(300);
        while Instant::now() < deadline {
            if self.child.try_wait().ok().flatten().is_some() {
                return;
            }
            thread::sleep(Duration::from_millis(10));
        }
        let _ = self.child.kill();
        let _ = self.child.wait();
    }
}
impl Process {
    fn failure(&self, message: &str) -> Error {
        failure(&message.replace("Filen", self.provider))
    }
    fn spawn(command: &[String], instance: &ConnectorInstance) -> Result<Self> {
        let mut cmd = Command::new(&command[0]);
        // Do not leak credentials for other configured accounts to this SDK
        // process through an inherited environment. Keep only runtime/network
        // settings required to launch Node and use the system proxy/CA setup.
        cmd.env_clear();
        for key in [
            "PATH",
            "HOME",
            "TMPDIR",
            "TMP",
            "TEMP",
            "LANG",
            "LC_ALL",
            "XDG_CACHE_HOME",
            "XDG_CONFIG_HOME",
            "XDG_RUNTIME_DIR",
            "DISPLAY",
            "WAYLAND_DISPLAY",
            "DBUS_SESSION_BUS_ADDRESS",
            "XDG_CURRENT_DESKTOP",
            "XDG_SESSION_DESKTOP",
            "XDG_SESSION_TYPE",
            "DESKTOP_SESSION",
            "XAUTHORITY",
            "XDG_DATA_DIRS",
            "XDG_DATA_HOME",
            "HTTP_PROXY",
            "HTTPS_PROXY",
            "ALL_PROXY",
            "NO_PROXY",
            "http_proxy",
            "https_proxy",
            "all_proxy",
            "no_proxy",
            "SSL_CERT_FILE",
            "SSL_CERT_DIR",
            "NODE_EXTRA_CA_CERTS",
        ] {
            if let Some(value) = std::env::var_os(key) {
                cmd.env(key, value);
            }
        }
        cmd.args(&command[1..])
            .stdin(Stdio::piped())
            .stdout(Stdio::piped())
            .stderr(Stdio::null());
        // Pass resolved credentials over the private pipe only, not the child's environment.
        if let Some(auth) = &instance.auth {
            for secret in std::iter::once(&auth.password).chain(auth.two_factor_code.iter()) {
                if let Some(env) = &secret.env {
                    cmd.env_remove(env);
                }
            }
        }
        if let Some(secret) = instance
            .oauth
            .as_ref()
            .and_then(|auth| auth.client_secret.as_ref())
        {
            if let Some(env) = &secret.env {
                cmd.env_remove(env);
            }
        }
        let provider = provider(instance);
        let mut child = cmd.spawn().map_err(|_| {
            failure(&format!(
                "Cannot start the {provider} adapter. Check its command and runtime installation."
            ))
        })?;
        let mut stdin = child.stdin.take().unwrap();
        let (input, outgoing) = mpsc::channel::<Vec<u8>>();
        // Pipe writes can block when a broken child stops reading. Keep them off
        // the requesting worker so deadlines/cancellation still kill and reap it.
        thread::spawn(move || {
            for bytes in outgoing {
                if stdin.write_all(&bytes).and_then(|_| stdin.flush()).is_err() {
                    break;
                }
            }
        });
        let stdout = child.stdout.take().unwrap();
        let (send, frames) = mpsc::sync_channel(16);
        thread::spawn(move || {
            let mut reader = BufReader::new(stdout);
            loop {
                let mut line = Vec::new();
                // A one-MiB byte array is at most ~4 MiB of JSON.
                let read = reader
                    .by_ref()
                    .take(5 * 1024 * 1024 + 1)
                    .read_until(b'\n', &mut line);
                if read.is_err()
                    || line.is_empty()
                    || line.len() > 5 * 1024 * 1024
                    || line.last() != Some(&b'\n')
                {
                    let _ = send.send(Err(()));
                    break;
                }
                let value = serde_json::from_slice(&line).map_err(|_| ());
                let invalid = value.is_err();
                if send.send(value).is_err() || invalid {
                    break;
                }
            }
        });
        Ok(Self {
            provider,
            child,
            input,
            frames,
            next: 0,
        })
    }
    fn deadlines(&self, op: &str) -> (Duration, Duration) {
        match op {
            "hello" => (Duration::from_secs(30), Duration::from_secs(30)),
            "login" if self.provider == "Google Drive" => {
                (Duration::from_secs(360), Duration::from_secs(360))
            }
            "login" => (Duration::from_secs(120), Duration::from_secs(120)),
            "read" | "write_new" | "copy" | "copy_unique" => {
                (Duration::from_secs(300), Duration::from_secs(24 * 60 * 60))
            }
            _ => (Duration::from_secs(120), Duration::from_secs(120)),
        }
    }
    fn call(&mut self, op: &str, args: Value, ctx: &Context) -> Result<Value> {
        let (idle, total) = self.deadlines(op);
        self.call_with_deadline(op, args, ctx, idle, total)
    }
    fn send(&self, value: Value) -> Result<()> {
        let mut bytes = serde_json::to_vec(&value)
            .map_err(|_| self.failure("Cannot encode a Filen adapter request."))?;
        bytes.push(b'\n');
        self.input
            .send(bytes)
            .map_err(|_| self.failure("Filen adapter disconnected."))
    }
    fn call_with_deadline(
        &mut self,
        op: &str,
        args: Value,
        ctx: &Context,
        idle: Duration,
        total: Duration,
    ) -> Result<Value> {
        ctx.check()?;
        let started = Instant::now();
        let mut last_activity = started;
        let mut last_progress = (0, 0);
        self.next += 1;
        let id = self.next;
        self.send(json!({"protocol":1,"id":id,"op":op,"args":args}))?;
        let mut cancelled = None;
        loop {
            if ctx.check().is_err() && cancelled.is_none() {
                let _ = self.send(json!({"protocol":1,"id":id,"op":"cancel"}));
                cancelled = Some(Instant::now());
            }
            if cancelled.is_some_and(|at| at.elapsed() > Duration::from_secs(2)) {
                return Err(Error::new(
                    ErrorCode::Cancelled,
                    "Cancelled. Completed items have been kept; refresh before retrying.",
                ));
            }
            if started.elapsed() >= total || last_activity.elapsed() >= idle {
                ctx.check()?;
                return Err(self.failure(
                    "Filen adapter request timed out. Refresh before retrying; completed items may remain.",
                ));
            }
            let frame = match self.frames.recv_timeout(Duration::from_millis(50)) {
                Ok(Ok(frame)) => frame,
                Err(mpsc::RecvTimeoutError::Timeout) => continue,
                _ => {
                    return Err(self.failure(
                        "Filen adapter disconnected or sent a malformed frame. Refresh before retrying.",
                    ));
                }
            };
            if frame.get("protocol").and_then(Value::as_u64) != Some(1)
                || frame.get("id").and_then(Value::as_u64) != Some(id)
            {
                return Err(self.failure("Invalid Filen adapter protocol or request ID."));
            }
            if let Some(progress) = frame.get("progress") {
                let progress: Progress = serde_json::from_value(progress.clone())
                    .map_err(|_| self.failure("Invalid Filen progress frame."))?;
                // Repeated heartbeat frames must not keep a stuck transfer alive.
                if progress.bytes > last_progress.0 || progress.items > last_progress.1 {
                    last_activity = Instant::now();
                    last_progress = (
                        last_progress.0.max(progress.bytes),
                        last_progress.1.max(progress.items),
                    );
                }
                ctx.progress(
                    if op == "login" {
                        "Authenticating remote account"
                    } else {
                        &progress.phase
                    },
                    progress.bytes,
                    progress.items,
                    progress.cancellable,
                );
                continue;
            }
            ctx.check()?;
            if let Some(error) = frame.get("error") {
                // Do not trust external error text to be credential-free.
                let code = error
                    .get("code")
                    .cloned()
                    .and_then(|v| serde_json::from_value::<ErrorCode>(v).ok())
                    .unwrap_or(ErrorCode::Io);
                let message = match code {
                    ErrorCode::AuthenticationRequired => {
                        "Filen authentication failed. Check your credential references."
                    }
                    ErrorCode::AlreadyExists => "A remote item already exists at the destination.",
                    ErrorCode::NotFound => "Remote item not found.",
                    ErrorCode::PermissionDenied => "The remote operation is not permitted.",
                    ErrorCode::Cancelled => "Cancelled. Completed items have been kept.",
                    ErrorCode::InvalidInput => "Invalid or ambiguous remote path or operation.",
                    _ => {
                        "Filen operation failed. Refresh before retrying; completed items or .niri-transfer files may remain."
                    }
                };
                return Err(Error::new(code, message.replace("Filen", self.provider)));
            }
            return frame
                .get("result")
                .cloned()
                .ok_or_else(|| self.failure("Missing Filen adapter result."));
        }
    }
}

fn provider(instance: &ConnectorInstance) -> &'static str {
    match instance.kind {
        ConnectorKind::Filen => "Filen",
        ConnectorKind::GoogleDrive => "Google Drive",
    }
}

// Resolve only the user-selected state directory. OAuth/account/token leaves
// remain untouched so the adapter can reject symlinks there itself.
fn canonical_state_directory(path: &std::path::Path) -> std::result::Result<PathBuf, String> {
    let invalid = || "Cannot resolve the remote account state directory".to_owned();
    let mut ancestor = std::path::absolute(path).map_err(|_| invalid())?;
    let mut tail = Vec::new();
    loop {
        match std::fs::symlink_metadata(&ancestor) {
            Ok(_) => {
                let mut resolved = ancestor.canonicalize().map_err(|_| invalid())?;
                if !std::fs::metadata(&resolved)
                    .map_err(|_| invalid())?
                    .is_dir()
                {
                    return Err(invalid());
                }
                for component in tail.into_iter().rev() {
                    resolved.push(component);
                }
                return Ok(resolved);
            }
            Err(error) if error.kind() == std::io::ErrorKind::NotFound => {
                // Missing tails must consist of normal directory names; do not
                // silently reinterpret unresolved parent-directory components.
                let name = ancestor.file_name().ok_or_else(invalid)?.to_owned();
                tail.push(name);
                if !ancestor.pop() {
                    return Err(invalid());
                }
            }
            Err(_) => return Err(invalid()),
        }
    }
}

impl RemoteConnector {
    fn failure(&self, message: &str) -> Error {
        failure(&message.replace("Filen", provider(&self.instance)))
    }
    /// Construction performs no process launch, secret resolution, or network I/O.
    pub fn new(instance: &ConnectorInstance, paths: &Paths) -> std::result::Result<Self, String> {
        let provider = provider(instance);
        let plugin = match instance.kind {
            ConnectorKind::Filen => "filen",
            ConnectorKind::GoogleDrive => "google-drive",
        };
        let command = if let Some(command) = &instance.command {
            command.clone()
        } else {
            let exe = std::env::current_exe()
                .map_err(|_| format!("Cannot locate the {provider} adapter"))?;
            let mut candidates = Vec::new();
            let filename = "adapter.mjs";
            for dir in exe.ancestors().skip(1) {
                candidates.push(dir.join(format!(
                    "share/niri-tonic-files/plugins/{plugin}/{filename}"
                )));
                candidates.push(dir.join(format!("plugins/{plugin}/{filename}")));
            }
            let adapter = candidates
                .into_iter()
                .find(|p| p.is_file())
                .ok_or_else(|| {
                    format!("{provider} adapter not installed (plugins/{plugin}/{filename})")
                })?;
            vec![
                "node".into(),
                adapter
                    .to_str()
                    .ok_or_else(|| format!("{provider} adapter path is not UTF-8"))?
                    .into(),
            ]
        };
        if command.is_empty() || command[0].is_empty() || command.iter().any(|s| s.contains('\0')) {
            return Err(format!("Invalid {provider} adapter command"));
        }
        Ok(Self {
            token_path: match instance.kind {
                ConnectorKind::GoogleDrive => canonical_state_directory(&paths.state)?
                    .join("oauth")
                    .join(&instance.id)
                    .join("session.json"),
                ConnectorKind::Filen => PathBuf::new(),
            },
            instance: instance.clone(),
            command,
            process: Mutex::new(None),
            ready: AtomicBool::new(false),
        })
    }
    fn path(&self, location: &Location, mutation: bool) -> Result<String> {
        if location.connector != self.instance.id {
            return Err(Error::new(
                ErrorCode::InvalidInput,
                "Location belongs to another connector instance.",
            ));
        }
        let path = decode_uri(&location.id)?;
        if mutation && (self.instance.read_only || path == "/") {
            return Err(Error::new(
                ErrorCode::PermissionDenied,
                "The account is read-only or the account root cannot be modified.",
            ));
        }
        Ok(location.id.clone())
    }
    fn request<T: DeserializeOwned>(&self, op: &str, args: Value, ctx: &Context) -> Result<T> {
        let mut state = loop {
            ctx.check()?;
            match self.process.try_lock() {
                Ok(guard) => break guard,
                Err(TryLockError::WouldBlock) => thread::sleep(Duration::from_millis(25)),
                Err(_) => return Err(self.failure("Filen worker state is unavailable.")),
            }
        };
        if state.is_none() {
            self.ready.store(false, Ordering::Relaxed);
            let login = match self.instance.kind {
                ConnectorKind::Filen => {
                    let auth = self.instance.auth.as_ref().ok_or_else(|| {
                        Error::new(
                            ErrorCode::AuthenticationRequired,
                            "Configure Filen credential references for this account.",
                        )
                    })?;
                    let password = resolve_secret(&auth.password, ctx)?;
                    let two_factor = auth
                        .two_factor_code
                        .as_ref()
                        .map(|secret| resolve_secret(secret, ctx))
                        .transpose()?;
                    json!({"email":auth.email,"password":password,"twoFactorCode":two_factor.unwrap_or_else(|| "XXXXXX".into())})
                }
                ConnectorKind::GoogleDrive => {
                    let auth = self.instance.oauth.as_ref().ok_or_else(|| {
                        Error::new(
                            ErrorCode::AuthenticationRequired,
                            "Configure Google Drive OAuth for this account.",
                        )
                    })?;
                    let mut args = json!({"client_id":auth.client_id,"token_path":self.token_path,"persist_session":auth.persist_session});
                    if let Some(secret) = &auth.client_secret {
                        args["client_secret"] = json!(resolve_secret(secret, ctx)?);
                    }
                    args
                }
            };
            let mut process = Process::spawn(&self.command, &self.instance)?;
            let hello = process.call(
                "hello",
                json!({"instance":self.instance.id,"read_only":self.instance.read_only}),
                ctx,
            )?;
            if hello.get("connector").and_then(Value::as_str) != Some(&self.instance.id) {
                return Err(self.failure("Filen adapter returned the wrong instance ID."));
            }
            process.call("login", login, ctx)?;
            self.ready.store(true, Ordering::Relaxed);
            *state = Some(process);
        }
        let result = state
            .as_mut()
            .unwrap()
            .call(op, args, ctx)
            .and_then(|value| {
                let check_location = |location: &Value| -> Result<()> {
                    let location: Location = serde_json::from_value(location.clone())
                        .map_err(|_| self.failure("Malformed Filen adapter location."))?;
                    self.path(&location, false).map(|_| ()).map_err(|_| {
                        self.failure("Filen adapter returned an invalid or cross-account location.")
                    })
                };
                match op {
                    "list" => {
                        let entries = value
                            .get("entries")
                            .and_then(Value::as_array)
                            .ok_or_else(|| self.failure("Malformed Filen listing."))?;
                        for entry in entries {
                            check_location(&entry["location"])?;
                        }
                    }
                    "stat" => check_location(&value["location"])?,
                    "copy_unique" | "archive" | "restore" => check_location(&value)?,
                    _ => {}
                }
                serde_json::from_value(value)
                    .map_err(|_| self.failure("Malformed Filen adapter result."))
            });
        if result.as_ref().is_err_and(|e| {
            matches!(
                e.code,
                ErrorCode::Io | ErrorCode::Cancelled | ErrorCode::AuthenticationRequired
            )
        }) {
            *state = None;
            self.ready.store(false, Ordering::Relaxed);
        }
        result
    }
    fn pair(
        &self,
        op: &str,
        source: &Location,
        destination: &Location,
        ctx: &Context,
    ) -> Result<()> {
        let path = self.path(source, op != "copy")?;
        let destination = self.path(destination, true)?;
        self.request(op, json!({"path":path,"destination":destination}), ctx)
    }
}

fn resolve_secret(secret: &SecretRef, ctx: &Context) -> Result<String> {
    ctx.check()?;
    let invalid = || {
        Error::new(
            ErrorCode::AuthenticationRequired,
            "Cannot resolve the configured credential value or reference.",
        )
    };
    secret.validate().map_err(|_| invalid())?;
    match (&secret.value, &secret.env, &secret.secret_service) {
        (Some(value), None, None) => Ok(value.clone()),
        (None, Some(key), None) => std::env::var(key)
            .ok()
            .filter(|s| !s.is_empty())
            .ok_or_else(invalid),
        (None, None, Some(reference)) => {
            let mut child = Command::new("secret-tool")
                .args(["lookup", "service", "niri-tonic-files", "key", reference])
                .stdin(Stdio::null())
                .stderr(Stdio::null())
                .stdout(Stdio::piped())
                .spawn()
                .map_err(|_| invalid())?;
            let stdout = child.stdout.take().unwrap();
            let (send, receive) = mpsc::channel();
            thread::spawn(move || {
                let mut bytes = Vec::new();
                let result = stdout.take(65537).read_to_end(&mut bytes).map(|_| bytes);
                let _ = send.send(result);
            });
            let started = Instant::now();
            let status = loop {
                if ctx.check().is_err() || started.elapsed() > Duration::from_secs(30) {
                    let _ = child.kill();
                    let _ = child.wait();
                    ctx.check()?;
                    return Err(invalid());
                }
                match child.try_wait() {
                    Ok(Some(status)) => break status,
                    Ok(None) => thread::sleep(Duration::from_millis(25)),
                    Err(_) => {
                        let _ = child.kill();
                        let _ = child.wait();
                        return Err(invalid());
                    }
                }
            };
            if !status.success() {
                return Err(invalid());
            }
            let bytes = receive
                .recv_timeout(Duration::from_secs(1))
                .map_err(|_| invalid())?
                .map_err(|_| invalid())?;
            if bytes.len() > 65536 {
                return Err(invalid());
            }
            let text = String::from_utf8(bytes).map_err(|_| invalid())?;
            let text = text.strip_suffix('\n').unwrap_or(&text).to_owned();
            if text.is_empty() {
                Err(invalid())
            } else {
                Ok(text)
            }
        }
        _ => Err(invalid()),
    }
}

fn decode_uri(uri: &str) -> Result<String> {
    let invalid = || {
        Error::new(
            ErrorCode::InvalidInput,
            "Expected a valid UTF-8 absolute remote file URI.",
        )
    };
    let encoded = uri
        .strip_prefix("file://")
        .filter(|p| p.starts_with('/'))
        .ok_or_else(invalid)?;
    let mut bytes = Vec::new();
    let mut input = encoded.bytes();
    while let Some(b) = input.next() {
        if b == b'%' {
            let hex = |b: u8| {
                (b as char)
                    .to_digit(16)
                    .map(|n| n as u8)
                    .ok_or_else(invalid)
            };
            let a = hex(input.next().ok_or_else(invalid)?)?;
            let b = hex(input.next().ok_or_else(invalid)?)?;
            bytes.push(a * 16 + b);
        } else {
            if b == b'?' || b == b'#' {
                return Err(invalid());
            }
            bytes.push(b);
        }
    }
    let path = String::from_utf8(bytes).map_err(|_| invalid())?;
    if path != "/"
        && path.split('/').skip(1).any(|s| {
            s.is_empty() || s == "." || s == ".." || s.chars().any(|c| c.is_control() || c == '\\')
        })
    {
        return Err(invalid());
    }
    Ok(path)
}

impl Connector for RemoteConnector {
    fn id(&self) -> &str {
        &self.instance.id
    }
    fn capabilities(&self) -> Capabilities {
        let write = !self.instance.read_only;
        Capabilities {
            read: true,
            write,
            create: write,
            rename: write,
            copy: write,
            move_item: write,
            trash: write,
            delete: write,
            archive: write,
            watch: false,
            pagination: false,
            atomic_move: false,
        }
    }
    fn authentication(&self) -> Authentication {
        if self.ready.load(Ordering::Relaxed) {
            Authentication::Ready
        } else {
            Authentication::Required {
                method: match self.instance.kind {
                    ConnectorKind::Filen => "Configured environment or Secret Service references",
                    ConnectorKind::GoogleDrive => "Google Drive browser OAuth",
                }
                .into(),
            }
        }
    }
    fn authenticate(&self, _credential_ref: &str, ctx: &Context) -> Result<()> {
        self.stat(
            &Location {
                connector: self.id().into(),
                id: "file:///".into(),
            },
            ctx,
        )
        .map(|_| ())
    }
    fn list(&self, directory: &Location, cursor: Option<&str>, ctx: &Context) -> Result<Page> {
        if cursor.is_some() {
            return Err(Error::new(
                ErrorCode::StaleCursor,
                format!(
                    "{} does not use pagination cursors.",
                    provider(&self.instance)
                ),
            ));
        }
        self.request("list", json!({"path":self.path(directory, false)?}), ctx)
    }
    fn stat(&self, location: &Location, ctx: &Context) -> Result<Entry> {
        self.request("stat", json!({"path":self.path(location, false)?}), ctx)
    }
    fn read(
        &self,
        location: &Location,
        offset: u64,
        limit: usize,
        ctx: &Context,
    ) -> Result<Vec<u8>> {
        if limit > 1024 * 1024 || offset > 9_007_199_254_740_991 {
            return Err(Error::new(
                ErrorCode::InvalidInput,
                format!(
                    "{} reads are limited to one MiB and safe integer offsets.",
                    provider(&self.instance)
                ),
            ));
        }
        let data: Vec<u8> = self.request(
            "read",
            json!({"path":self.path(location, false)?,"offset":offset,"limit":limit}),
            ctx,
        )?;
        if data.len() > limit {
            return Err(self.failure("Filen adapter exceeded the read limit."));
        }
        Ok(data)
    }
    fn write_new(&self, location: &Location, input: &mut dyn Read, ctx: &Context) -> Result<()> {
        let path = self.path(location, true)?;
        ctx.check()?;
        let mut temp = tempfile::NamedTempFile::new()
            .map_err(|_| self.failure("Cannot create a private Filen transfer file."))?;
        let mut buffer = [0; 64 * 1024];
        let mut bytes = 0;
        loop {
            ctx.check()?;
            let size = input
                .read(&mut buffer)
                .map_err(|_| failure("Cannot read upload source."))?;
            if size == 0 {
                break;
            }
            temp.write_all(&buffer[..size])
                .map_err(|_| failure("Cannot stage upload data."))?;
            bytes += size as u64;
            ctx.progress("Staging upload", bytes, 0, true);
        }
        temp.flush()
            .map_err(|_| failure("Cannot flush upload staging data."))?;
        let source: PathBuf = temp.path().into();
        let source = source
            .to_str()
            .ok_or_else(|| failure("Upload staging path is not UTF-8."))?;
        self.request("write_new", json!({"path":path,"source":source}), ctx)
    }
    fn create_directory(&self, location: &Location, ctx: &Context) -> Result<()> {
        self.request(
            "create_directory",
            json!({"path":self.path(location, true)?}),
            ctx,
        )
    }
    fn rename(&self, source: &Location, destination: &Location, ctx: &Context) -> Result<()> {
        self.pair("rename", source, destination, ctx)
    }
    fn copy(&self, source: &Location, destination: &Location, ctx: &Context) -> Result<()> {
        self.pair("copy", source, destination, ctx)
    }
    fn copy_unique(
        &self,
        source: &Location,
        destination: &Location,
        ctx: &Context,
    ) -> Result<Location> {
        self.request(
            "copy_unique",
            json!({"path":self.path(source, false)?,"destination":self.path(destination, true)?}),
            ctx,
        )
    }
    fn move_item(&self, source: &Location, destination: &Location, ctx: &Context) -> Result<()> {
        self.pair("move_item", source, destination, ctx)
    }
    fn delete(&self, location: &Location, mode: DeleteMode, ctx: &Context) -> Result<()> {
        self.request(
            "delete",
            json!({"path":self.path(location, true)?,"mode":mode}),
            ctx,
        )
    }
    fn archive(&self, location: &Location, ctx: &Context) -> Result<Location> {
        self.request("archive", json!({"path":self.path(location, true)?}), ctx)
    }
    fn restore(&self, location: &Location, ctx: &Context) -> Result<Location> {
        self.request("restore", json!({"path":self.path(location, true)?}), ctx)
    }
}

#[cfg(test)]
mod process_tests {
    use super::*;

    fn fake(script: &str) -> Process {
        let instance: ConnectorInstance = serde_json::from_value(json!({
            "id":"isolated", "name":"Isolated", "type":"filen", "enabled":true,
            "read_only":false, "auth":null, "command":null
        }))
        .unwrap();
        Process::spawn(&["python3".into(), "-c".into(), script.into()], &instance).unwrap()
    }
    fn assert_reaped(process: Process) {
        let pid = process.child.id();
        drop(process);
        assert!(
            !std::path::Path::new(&format!("/proc/{pid}")).exists(),
            "adapter child was not reaped"
        );
    }
    #[test]
    fn oauth_interaction_has_six_minutes_without_extending_filen_login() {
        let mut process = fake("import time; time.sleep(600)");
        assert_eq!(
            process.deadlines("login"),
            (Duration::from_secs(120), Duration::from_secs(120))
        );
        process.provider = "Google Drive";
        assert_eq!(
            process.deadlines("login"),
            (Duration::from_secs(360), Duration::from_secs(360))
        );
        assert_eq!(
            process.deadlines("hello"),
            (Duration::from_secs(30), Duration::from_secs(30))
        );
        assert_eq!(
            process.deadlines("stat"),
            (Duration::from_secs(120), Duration::from_secs(120))
        );
        assert_reaped(process);
    }
    #[test]
    fn hanging_hello_login_and_blocked_pipe_are_bounded_and_reaped() {
        for op in ["hello", "login", "write_new"] {
            let mut process = fake("import time; time.sleep(600)");
            let start = Instant::now();
            let error = process
                .call_with_deadline(
                    op,
                    json!({"synthetic": "x".repeat(262144)}),
                    &Context::default(),
                    Duration::from_millis(100),
                    Duration::from_millis(200),
                )
                .unwrap_err();
            assert_eq!(error.code, ErrorCode::Io);
            assert!(error.message.contains("timed out"));
            assert!(start.elapsed() < Duration::from_secs(2));
            assert_reaped(process);
        }
    }
    #[test]
    fn stalled_progress_and_continuous_progress_have_deadlines() {
        for counter in ["0", "i"] {
            let script = format!(
                "import sys,json,time\nf=json.loads(sys.stdin.readline())\ni=0\nwhile True:\n i+=1\n print(json.dumps(dict(protocol=1,id=f['id'],progress=dict(phase='transfer',bytes={counter},items=0,cancellable=True))),flush=True)\n time.sleep(.01)"
            );
            let mut process = fake(&script);
            let error = process
                .call_with_deadline(
                    "copy",
                    json!({}),
                    &Context::default(),
                    Duration::from_millis(100),
                    Duration::from_millis(250),
                )
                .unwrap_err();
            assert_eq!(error.code, ErrorCode::Io);
            assert!(error.message.contains("timed out"));
            assert_reaped(process);
        }
    }
    #[test]
    fn wrong_request_ids_and_ignored_cancellation_reap_children() {
        let mut process = fake(
            "import sys,json,time\nf=json.loads(sys.stdin.readline())\nprint(json.dumps(dict(protocol=1,id=f['id']+1,result='SYNTHETIC_SECRET')),flush=True)\ntime.sleep(600)",
        );
        let error = process
            .call("stat", json!({}), &Context::default())
            .unwrap_err();
        assert_eq!(error.code, ErrorCode::Io);
        assert!(!error.message.contains("SYNTHETIC_SECRET"));
        assert_reaped(process);

        let mut process = fake("import time; time.sleep(600)");
        let ctx = Context::default();
        let cancel = ctx.clone();
        let thread = thread::spawn(move || {
            thread::sleep(Duration::from_millis(50));
            cancel.cancel();
        });
        assert_eq!(
            process.call("stat", json!({}), &ctx).unwrap_err().code,
            ErrorCode::Cancelled
        );
        thread.join().unwrap();
        assert_reaped(process);
    }
}
