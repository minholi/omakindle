use std::os::unix::fs::PermissionsExt;
use std::path::{Path, PathBuf};
use std::sync::Arc;

use serde::Deserialize;
use serde_json::{json, Value};
use tokio::io::{AsyncBufReadExt, AsyncWriteExt, BufReader};
use tokio::net::{UnixListener, UnixStream};
use tokio::sync::mpsc;

use crate::state::App;

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
}

fn default_version() -> u8 {
    1
}

pub fn default_socket_path() -> PathBuf {
    let runtime = std::env::var("XDG_RUNTIME_DIR").unwrap_or_else(|_| "/tmp".into());
    PathBuf::from(runtime).join("omakindle").join("backend.sock")
}

pub async fn serve(app: Arc<App>, path: &Path) -> std::io::Result<()> {
    let _ = std::fs::remove_file(path);
    if let Some(parent) = path.parent() {
        std::fs::create_dir_all(parent)?;
        let _ = std::fs::set_permissions(parent, std::fs::Permissions::from_mode(0o700));
    }
    let listener = UnixListener::bind(path)?;
    let _ = std::fs::set_permissions(path, std::fs::Permissions::from_mode(0o600));

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
        let mut ticker = tokio::time::interval(std::time::Duration::from_secs(15 * 60));
        ticker.tick().await;
        loop {
            ticker.tick().await;
            let _ = refresh_app.refresh().await;
        }
    });

    loop {
        let (stream, _) = listener.accept().await?;
        let app = app.clone();
        tokio::spawn(async move {
            let _ = handle(stream, app).await;
        });
    }
}

async fn handle(stream: UnixStream, app: Arc<App>) -> std::io::Result<()> {
    let (reader, mut writer) = stream.into_split();
    let (tx, mut rx) = mpsc::channel::<String>(64);

    let write_task = tokio::spawn(async move {
        while let Some(message) = rx.recv().await {
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

    let mut lines = BufReader::new(reader).lines();
    while let Some(line) = lines.next_line().await? {
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
            match app.highlights(&asin).await {
                Ok(highlights) => success(id, json!({ "highlights": highlights })),
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
