use std::io;
use std::os::unix::fs::{DirBuilderExt, FileTypeExt, MetadataExt, PermissionsExt};
use std::path::{Path, PathBuf};
use std::sync::Arc;

use serde::Deserialize;
use serde_json::{json, Value};
use tokio::io::{AsyncBufRead, AsyncBufReadExt, AsyncWriteExt, BufReader};
use tokio::net::{UnixListener, UnixStream};
use tokio::sync::mpsc;

use crate::state::App;

const APP_DIR: &str = "omakindle";
const SOCKET_NAME: &str = "backend.sock";
const MAX_REQUEST_LINE: usize = 256 * 1024;
const MAX_RESPONSE_LINE: usize = 8 * 1024 * 1024;

#[derive(Deserialize)]
struct Request {
    #[serde(default = "default_version")]
    v: u8,
    #[serde(default)]
    id: i64,
    command: String,
    #[serde(default)]
    cookies: Option<String>,
    #[serde(default, rename = "deviceToken")]
    device_token: Option<String>,
    #[serde(default)]
    region: Option<String>,
    #[serde(default)]
    asin: Option<String>,
    #[serde(default)]
    minutes: Option<u64>,
    #[serde(default)]
    force: Option<bool>,
    #[serde(default)]
    enrich: Option<bool>,
    #[serde(default)]
    start: Option<i64>,
    #[serde(default)]
    end: Option<i64>,
    #[serde(default)]
    limit: Option<usize>,
    #[serde(default, rename = "perBook")]
    per_book: Option<usize>,
}

fn default_version() -> u8 {
    1
}

/// The real uid of this process. `/proc/self` is owned by it, which keeps the
/// crate free of a direct libc dependency.
fn current_uid() -> io::Result<u32> {
    Ok(std::fs::metadata("/proc/self")?.uid())
}

fn refuse(message: impl Into<String>) -> io::Error {
    io::Error::new(
        io::ErrorKind::PermissionDenied,
        format!(
            "refusing to use an insecure socket location: {}",
            message.into()
        ),
    )
}

/// No-follow check that `path` is a real directory owned by this process.
fn require_dir_owner(path: &Path, uid: u32) -> io::Result<std::fs::Metadata> {
    let metadata = std::fs::symlink_metadata(path).map_err(|error| {
        refuse(format!("cannot inspect {}: {error}", path.display()))
    })?;
    if !metadata.file_type().is_dir() {
        return Err(refuse(format!("{} is not a directory", path.display())));
    }
    if metadata.uid() != uid {
        return Err(refuse(format!(
            "{} is not owned by the current user",
            path.display()
        )));
    }
    Ok(metadata)
}

/// No-follow check of a directory the process must own and that other local
/// users must not be able to modify. `forbidden` selects how much access
/// group/other are allowed to keep (write bits for the runtime directory,
/// all bits for the plugin-private directory).
fn require_private_dir(path: &Path, uid: u32, forbidden: u32) -> io::Result<()> {
    let metadata = require_dir_owner(path, uid)?;
    if metadata.mode() & forbidden != 0 {
        return Err(refuse(format!(
            "{} is accessible to other local users",
            path.display()
        )));
    }
    Ok(())
}

/// Create the plugin-private directory with mode 0700 and verify the result
/// with no-follow operations. An existing directory is accepted only after a
/// no-follow type/owner check, is repaired to mode 0700 with a checked chmod,
/// and is verified again; a symlink, a foreign owner, or a failed chmod
/// aborts startup.
fn ensure_private_dir(path: &Path, uid: u32) -> io::Result<()> {
    match std::fs::symlink_metadata(path) {
        Ok(_) => {
            require_dir_owner(path, uid)?;
        }
        Err(error) if error.kind() == io::ErrorKind::NotFound => {
            std::fs::DirBuilder::new().mode(0o700).create(path)?;
        }
        Err(error) => return Err(error),
    }
    std::fs::set_permissions(path, std::fs::Permissions::from_mode(0o700))?;
    require_private_dir(path, uid, 0o077)
}

pub fn default_socket_path() -> io::Result<PathBuf> {
    let runtime = std::env::var("XDG_RUNTIME_DIR")
        .map_err(|_| refuse("XDG_RUNTIME_DIR is not set"))?;
    let runtime = PathBuf::from(runtime);
    if runtime.as_os_str().is_empty() || !runtime.is_absolute() {
        return Err(refuse("XDG_RUNTIME_DIR is empty or not an absolute path"));
    }
    let uid = current_uid()?;
    require_private_dir(&runtime, uid, 0o022)?;
    let dir = runtime.join(APP_DIR);
    ensure_private_dir(&dir, uid)?;
    Ok(dir.join(SOCKET_NAME))
}

pub async fn serve(app: Arc<App>, path: &Path) -> std::io::Result<()> {
    let uid = current_uid()?;
    if !path.is_absolute() {
        return Err(refuse("socket path is not absolute"));
    }
    let parent = path
        .parent()
        .ok_or_else(|| refuse("socket path has no parent directory"))?;
    require_private_dir(parent, uid, 0o022)?;

    match std::fs::symlink_metadata(path) {
        Ok(metadata) => {
            if !metadata.file_type().is_socket() || metadata.uid() != uid {
                return Err(refuse(format!(
                    "{} already exists and is not a user-owned socket",
                    path.display()
                )));
            }
            std::fs::remove_file(path)?;
        }
        Err(error) if error.kind() == io::ErrorKind::NotFound => {}
        Err(error) => return Err(error),
    }

    let listener = UnixListener::bind(path)?;
    std::fs::set_permissions(path, std::fs::Permissions::from_mode(0o600))?;
    let metadata = std::fs::symlink_metadata(path)?;
    if !metadata.file_type().is_socket()
        || metadata.uid() != uid
        || metadata.mode() & 0o777 != 0o600
    {
        return Err(refuse("socket permissions could not be verified"));
    }

    let refresh_app = app.clone();
    tokio::spawn(async move {
        let stored = crate::secrets::load().await;
        let has_stored = stored.as_ref().ok().and_then(|value| value.as_ref()).is_some();
        eprintln!(
            "omakindle: startup session file: {}",
            if has_stored { "found" } else { "none" }
        );
        if let Err(error) = &stored {
            eprintln!("omakindle: session file read failed: {error}");
        }
        if has_stored || crate::state::dev_credentials_present() {
            match refresh_app.refresh().await {
                Ok(()) => eprintln!("omakindle: startup refresh finished"),
                Err((code, message)) => {
                    eprintln!("omakindle: startup refresh failed: {code}: {message}")
                }
            }
        }
        let mut ticker = tokio::time::interval(std::time::Duration::from_secs(30));
        ticker.tick().await;
        let mut elapsed: u64 = 0;
        loop {
            ticker.tick().await;
            elapsed += 30;
            let minutes = refresh_app.refresh_minutes();
            if elapsed >= minutes.saturating_mul(60) {
                elapsed = 0;
                let _ = refresh_app.refresh().await;
            }
        }
    });

    loop {
        let (stream, _) = listener.accept().await?;
        match stream.peer_cred() {
            Ok(credentials) if credentials.uid() == uid => {}
            Ok(_) => {
                eprintln!("omakindle: rejected a connection from another local user");
                continue;
            }
            Err(error) => {
                eprintln!("omakindle: rejected a connection without peer credentials: {error}");
                continue;
            }
        }
        let app = app.clone();
        tokio::spawn(async move {
            let _ = handle(stream, app).await;
        });
    }
}

enum LineRead {
    Line(String),
    Eof,
    TooLong,
}

/// Read one newline-terminated line while never buffering more than `max`
/// bytes, so a peer cannot make the backend allocate without bound.
async fn read_line_bounded<R>(reader: &mut R, max: usize) -> io::Result<LineRead>
where
    R: AsyncBufRead + Unpin,
{
    let mut buffer: Vec<u8> = Vec::new();
    loop {
        let available = reader.fill_buf().await?;
        if available.is_empty() {
            if buffer.is_empty() {
                return Ok(LineRead::Eof);
            }
            break;
        }
        if let Some(position) = available.iter().position(|byte| *byte == b'\n') {
            if buffer.len() + position > max {
                return Ok(LineRead::TooLong);
            }
            buffer.extend_from_slice(&available[..position]);
            reader.consume(position + 1);
            break;
        }
        if buffer.len() + available.len() > max {
            return Ok(LineRead::TooLong);
        }
        let consumed = available.len();
        buffer.extend_from_slice(available);
        reader.consume(consumed);
    }
    match String::from_utf8(buffer) {
        Ok(line) => Ok(LineRead::Line(line)),
        Err(_) => Err(io::Error::new(
            io::ErrorKind::InvalidData,
            "request line is not valid UTF-8",
        )),
    }
}

async fn handle(stream: UnixStream, app: Arc<App>) -> std::io::Result<()> {
    let (reader, mut writer) = stream.into_split();
    let (tx, mut rx) = mpsc::channel::<String>(64);

    let write_task = tokio::spawn(async move {
        while let Some(message) = rx.recv().await {
            let message = if message.len() > MAX_RESPONSE_LINE {
                error_response(
                    -1,
                    "response_too_large",
                    &format!("responses are limited to {MAX_RESPONSE_LINE} bytes"),
                )
            } else {
                message
            };
            if writer.write_all(message.as_bytes()).await.is_err() {
                break;
            }
            if writer.write_all(b"\n").await.is_err() {
                break;
            }
        }
    });

    let snapshot = json!({
        "type": "snapshot",
        "v": 1,
        "state": app.snapshot().await,
    });
    let _ = tx.send(snapshot.to_string()).await;

    let mut events = app.events.subscribe();
    let event_tx = tx.clone();
    tokio::spawn(async move {
        loop {
            tokio::select! {
                event = events.recv() => match event {
                    Ok(event) => {
                        if event_tx.send(event).await.is_err() {
                            break;
                        }
                    }
                    Err(tokio::sync::broadcast::error::RecvError::Closed) => break,
                    Err(tokio::sync::broadcast::error::RecvError::Lagged(_)) => continue,
                },
                _ = event_tx.closed() => break,
            }
        }
    });

    let mut reader = BufReader::new(reader);
    loop {
        let line = match read_line_bounded(&mut reader, MAX_REQUEST_LINE).await {
            Ok(LineRead::Eof) => break,
            Ok(LineRead::Line(line)) => line,
            Ok(LineRead::TooLong) => {
                let response = error_response(
                    -1,
                    "line_too_long",
                    &format!("request lines are limited to {MAX_REQUEST_LINE} bytes"),
                );
                let _ = tx.send(response).await;
                break;
            }
            Err(error) => return Err(error),
        };
        let line = line.trim().to_string();
        if line.is_empty() {
            continue;
        }
        let response = dispatch(&app, &line).await;
        if tx.send(response).await.is_err() {
            break;
        }
    }

    drop(tx);
    let _ = write_task.await;
    Ok(())
}

async fn dispatch(app: &Arc<App>, line: &str) -> String {
    let request: Request = match serde_json::from_str(line) {
        Ok(request) => request,
        Err(error) => {
            return json!({
                "type": "response",
                "v": 1,
                "id": -1,
                "ok": false,
                "errorCode": "bad_request",
                "error": format!("invalid request: {error}"),
            })
            .to_string();
        }
    };

    let id = request.id;
    if request.v != 1 {
        return error_response(id, "unsupported_version", "protocol version must be 1");
    }

    match request.command.as_str() {
        "hello" => success(id, json!({ "protocolVersion": 1, "backendVersion": env!("CARGO_PKG_VERSION") })),
        "ping" => success(id, json!({})),
        "get_state" => success(id, json!({ "state": app.snapshot().await })),
        "refresh" => match app.refresh().await {
            Ok(()) => success(id, json!({ "state": app.snapshot().await })),
            Err((code, message)) => error_response(id, &code, &message),
        },
        "set_credentials" => {
            let cookies = request.cookies.unwrap_or_default();
            let device_token = request.device_token.unwrap_or_default();
            let region = request.region.unwrap_or_else(|| "us".into());
            match app.set_credentials(&cookies, &device_token, &region).await {
                Ok(()) => {
                    let refresh = app.refresh().await;
                    match refresh {
                        Ok(()) => success(id, json!({ "state": app.snapshot().await })),
                        Err((code, message)) => error_response(id, &code, &message),
                    }
                }
                Err((code, message)) => error_response(id, &code, &message),
            }
        }
        "store_cookies" => {
            let cookies = request.cookies.unwrap_or_default();
            let region = request.region.unwrap_or_else(|| "us".into());
            if cookies.is_empty() {
                return error_response(id, "bad_request", "cookies are required");
            }
            match app.set_credentials(&cookies, "", &region).await {
                Ok(()) => {
                    let refresh = app.refresh().await;
                    match refresh {
                        Ok(()) => success(id, json!({ "state": app.snapshot().await })),
                        Err((code, message)) => error_response(id, &code, &message),
                    }
                }
                Err((code, message)) => error_response(id, &code, &message),
            }
        }
        "set_device_token" => {
            let device_token = request.device_token.unwrap_or_default();
            if device_token.is_empty() {
                return error_response(id, "bad_request", "deviceToken is required");
            }
            match app.set_device_token(&device_token).await {
                Ok(()) => {
                    let refresh = app.refresh().await;
                    match refresh {
                        Ok(()) => success(id, json!({ "state": app.snapshot().await })),
                        Err((code, message)) => error_response(id, &code, &message),
                    }
                }
                Err((code, message)) => error_response(id, &code, &message),
            }
        }
        "set_refresh_minutes" => {
            let minutes = request.minutes.unwrap_or(15);
            app.set_refresh_minutes(minutes);
            success(id, json!({ "refreshMinutes": app.refresh_minutes() }))
        }
        "clear_credentials" => match app.clear_credentials().await {
            Ok(()) => success(id, json!({ "state": app.snapshot().await })),
            Err((code, message)) => error_response(id, &code, &message),
        },
        "get_highlights" => {
            let asin = request.asin.unwrap_or_default();
            if asin.is_empty() {
                return error_response(id, "bad_request", "asin is required");
            }
            let force = request.force.unwrap_or(false);
            let enrich = request.enrich.unwrap_or(false);
            match app.highlights(&asin, force, enrich).await {
                Ok(result) => success(
                    id,
                    json!({
                        "highlights": result.highlights,
                        "cached": result.cached,
                        "stale": result.stale,
                        "fetchedAt": result.fetched_at,
                    }),
                ),
                Err((code, message)) => error_response(id, &code, &message),
            }
        }
        "get_recent_highlights" => {
            let limit = request.limit.unwrap_or(8);
            let per_book = request.per_book.unwrap_or(4);
            let force = request.force.unwrap_or(false);
            match app.recent_highlights(limit, per_book, force).await {
                Ok(result) => success(id, result),
                Err((code, message)) => error_response(id, &code, &message),
            }
        }
        "get_highlight_text" => {
            let asin = request.asin.unwrap_or_default();
            if asin.is_empty() {
                return error_response(id, "bad_request", "asin is required");
            }
            let (Some(start), Some(end)) = (request.start, request.end) else {
                return error_response(id, "bad_request", "start and end are required");
            };
            match app.highlight_text(&asin, start, end).await {
                Ok(text) => success(id, json!({ "text": text })),
                Err((code, message)) => error_response(id, &code, &message),
            }
        }
        other => error_response(id, "unknown_command", &format!("unknown command: {other}")),
    }
}

fn success(id: i64, result: Value) -> String {
    json!({
        "type": "response",
        "v": 1,
        "id": id,
        "ok": true,
        "result": result,
    })
    .to_string()
}

fn error_response(id: i64, code: &str, message: &str) -> String {
    json!({
        "type": "response",
        "v": 1,
        "id": id,
        "ok": false,
        "errorCode": code,
        "error": message,
    })
    .to_string()
}
