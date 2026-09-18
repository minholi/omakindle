use std::os::unix::fs::PermissionsExt;
use std::path::PathBuf;

fn config_dir() -> PathBuf {
    let base = std::env::var("XDG_CONFIG_HOME")
        .map(PathBuf::from)
        .unwrap_or_else(|_| {
            let home = std::env::var("HOME").unwrap_or_else(|_| "/tmp".into());
            PathBuf::from(home).join(".config")
        });
    base.join("omakindle")
}

fn session_path() -> PathBuf {
    config_dir().join("session.json")
}

pub async fn store(cookies: &str, device_token: &str, region: &str) -> Result<(), String> {
    let payload = serde_json::json!({
        "cookies": cookies,
        "deviceToken": device_token,
        "region": region,
    })
    .to_string();

    let dir = config_dir();
    std::fs::create_dir_all(&dir).map_err(|error| error.to_string())?;
    let temporary = dir.join(".session.json.tmp");
    std::fs::write(&temporary, payload.as_bytes()).map_err(|error| error.to_string())?;
    std::fs::set_permissions(&temporary, std::fs::Permissions::from_mode(0o600))
        .map_err(|error| error.to_string())?;
    std::fs::rename(&temporary, session_path()).map_err(|error| error.to_string())
}

pub async fn load() -> Result<Option<(String, String, String)>, String> {
    let raw = match std::fs::read_to_string(session_path()) {
        Ok(raw) => raw,
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => return Ok(None),
        Err(error) => return Err(error.to_string()),
    };
    let raw = raw.trim();
    if raw.is_empty() {
        return Ok(None);
    }
    let parsed: serde_json::Value =
        serde_json::from_str(raw).map_err(|error| format!("stored session is not JSON: {error}"))?;
    let cookies = parsed["cookies"].as_str().unwrap_or_default().to_string();
    let device_token = parsed["deviceToken"]
        .as_str()
        .unwrap_or_default()
        .to_string();
    let region = parsed["region"].as_str().unwrap_or("us").to_string();
    if cookies.is_empty() {
        return Ok(None);
    }
    Ok(Some((cookies, device_token, region)))
}

pub async fn clear() -> Result<(), String> {
    match std::fs::remove_file(session_path()) {
        Ok(()) => Ok(()),
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => Ok(()),
        Err(error) => Err(error.to_string()),
    }
}
