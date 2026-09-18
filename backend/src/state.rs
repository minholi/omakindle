use std::collections::HashMap;
use std::path::PathBuf;
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::Arc;
use std::time::{Duration, SystemTime, UNIX_EPOCH};

use serde::{Deserialize, Serialize};
use tokio::sync::{broadcast, Mutex, RwLock};

use crate::amazon::{Amazon, Credentials, Error as AmazonError};
use crate::models::{Book, Progress};

const PROGRESS_BOOKS: usize = 12;
const PROGRESS_PAUSE: Duration = Duration::from_millis(400);

#[derive(Debug, Clone, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct ReadingBook {
    pub asin: String,
    pub title: String,
    pub authors: Vec<String>,
    pub cover_url: String,
    pub web_reader_url: String,
    pub percentage_read: f64,
    pub device_name: String,
    pub sync_time: i64,
}

#[derive(Debug, Clone, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct Snapshot {
    pub lifecycle: String,
    pub error_code: Option<String>,
    pub error: Option<String>,
    pub region: String,
    pub books: Vec<Book>,
    pub reading: Vec<ReadingBook>,
    pub updated_at: Option<u64>,
    pub refreshing: bool,
    pub needs_device_token: bool,
}

impl Default for Snapshot {
    fn default() -> Self {
        Self {
            lifecycle: "unconfigured".into(),
            error_code: None,
            error: None,
            region: "us".into(),
            books: Vec::new(),
            reading: Vec::new(),
            updated_at: None,
            refreshing: false,
            needs_device_token: false,
        }
    }
}

#[derive(Debug, Clone, Serialize, Deserialize, Default)]
struct CachedPoint {
    progress: Option<Progress>,
    percentage: Option<f64>,
}

#[derive(Debug, Clone, Serialize, Deserialize, Default)]
struct Cache {
    region: String,
    books: Vec<Book>,
    #[serde(default)]
    points: HashMap<String, CachedPoint>,
    updated_at: Option<u64>,
}

pub struct App {
    pub state: RwLock<Snapshot>,
    pub client: Mutex<Option<Amazon>>,
    pub refresh_lock: Mutex<()>,
    pub events: broadcast::Sender<String>,
    refresh_minutes: AtomicU64,
}

impl App {
    pub fn new() -> Arc<Self> {
        let (events, _) = broadcast::channel(64);
        let cache = load_cache();
        let mut snapshot = Snapshot {
            region: cache.region.clone(),
            books: cache.books.clone(),
            updated_at: cache.updated_at,
            ..Snapshot::default()
        };
        snapshot.reading = reading_from_cache(&cache);
        Arc::new(Self {
            state: RwLock::new(snapshot),
            client: Mutex::new(None),
            refresh_lock: Mutex::new(()),
            events,
            refresh_minutes: AtomicU64::new(15),
        })
    }

    pub fn refresh_minutes(&self) -> u64 {
        self.refresh_minutes.load(Ordering::Relaxed).clamp(5, 1440)
    }

    pub fn set_refresh_minutes(&self, minutes: u64) {
        self.refresh_minutes.store(minutes.clamp(5, 1440), Ordering::Relaxed);
    }

    pub async fn snapshot(&self) -> Snapshot {
        self.state.read().await.clone()
    }

    async fn set_error(&self, code: &str, message: &str) {
        {
            let mut state = self.state.write().await;
            state.lifecycle = if code == "unconfigured" {
                "unconfigured".into()
            } else {
                "error".into()
            };
            state.error_code = Some(code.to_string());
            state.error = Some(message.to_string());
            state.refreshing = false;
        }
        self.emit().await;
    }

    pub async fn emit(&self) {
        let snapshot = self.snapshot().await;
        let payload = serde_json::json!({
            "type": "event",
            "v": 1,
            "event": "state_changed",
            "state": snapshot,
        });
        let _ = self.events.send(payload.to_string());
    }

    pub async fn set_credentials(
        self: &Arc<Self>,
        cookies: &str,
        device_token: &str,
        region: &str,
    ) -> Result<(), (String, String)> {
        let credentials = Credentials::new(cookies, device_token)
            .map_err(|error| ("invalid_credentials".to_string(), error.to_string()))?;
        let mut client = Amazon::new(region, credentials)
            .map_err(|error| ("invalid_credentials".to_string(), error.to_string()))?;
        let books = client
            .library(false)
            .await
            .map_err(map_error)?;
        if books.is_empty() {
            return Err((
                "invalid_credentials".to_string(),
                "Amazon returned an empty library for these credentials".to_string(),
            ));
        }
        crate::secrets::store(cookies, device_token, region)
            .await
            .map_err(|error| {
                (
                    "session_error".to_string(),
                    format!("could not write the session file: {error}"),
                )
            })?;
        *self.client.lock().await = Some(client);
        let has_token = !device_token.trim().is_empty();
        {
            let mut state = self.state.write().await;
            state.region = region.to_string();
            state.books = books;
            state.lifecycle = "loading".into();
            state.error = None;
            state.error_code = None;
            state.needs_device_token = !has_token;
        }
        self.emit().await;
        Ok(())
    }

    pub async fn set_device_token(
        self: &Arc<Self>,
        device_token: &str,
    ) -> Result<(), (String, String)> {
        let stored = crate::secrets::load()
            .await
            .map_err(|error| ("session_error".to_string(), error.to_string()))?
            .or_else(dev_credentials);
        let Some((cookies, _stored_token, region)) = stored else {
            return Err((
                "unconfigured".to_string(),
                "no stored session; sign in first".to_string(),
            ));
        };
        self.set_credentials(&cookies, device_token, &region).await
    }

    pub async fn clear_credentials(self: &Arc<Self>) -> Result<(), (String, String)> {
        crate::secrets::clear()
            .await
            .map_err(|error| ("session_error".to_string(), error.to_string()))?;
        *self.client.lock().await = None;
        {
            let mut state = self.state.write().await;
            *state = Snapshot::default();
        }
        self.emit().await;
        Ok(())
    }

    async fn ensure_client(self: &Arc<Self>) -> Result<(), (String, String)> {
        if self.client.lock().await.is_some() {
            return Ok(());
        }
        let stored = crate::secrets::load()
            .await
            .map_err(|error| ("session_error".to_string(), error.to_string()))?
            .or_else(dev_credentials);
        let Some((cookies, device_token, region)) = stored else {
            self.set_error("unconfigured", "No Amazon session configured yet").await;
            return Err(("unconfigured".to_string(), "no credentials".into()));
        };
        let credentials = Credentials::new(&cookies, &device_token).map_err(map_error)?;
        let client = Amazon::new(&region, credentials).map_err(map_error)?;
        *self.client.lock().await = Some(client);
        let mut state = self.state.write().await;
        state.region = region;
        Ok(())
    }

    pub async fn refresh(self: &Arc<Self>) -> Result<(), (String, String)> {
        let _guard = self.refresh_lock.try_lock().map_err(|_| {
            ("busy".to_string(), "a refresh is already running".to_string())
        })?;
        {
            let mut state = self.state.write().await;
            state.refreshing = true;
        }
        self.emit().await;

        if let Err(error) = self.ensure_client().await {
            let mut state = self.state.write().await;
            state.refreshing = false;
            drop(state);
            self.emit().await;
            return Err(error);
        }

        let result = self.refresh_inner().await;
        let mut state = self.state.write().await;
        state.refreshing = false;
        drop(state);
        match result {
            Ok(()) => {
                self.emit().await;
                Ok(())
            }
            Err((code, message)) => {
                self.set_error(&code, &message).await;
                Err((code, message))
            }
        }
    }

    async fn refresh_inner(self: &Arc<Self>) -> Result<(), (String, String)> {
        let mut client_guard = self.client.lock().await;
        let Some(client) = client_guard.as_mut() else {
            return Err(("unconfigured".to_string(), "no credentials".into()));
        };
        let has_token = client.has_device_token();
        let books = client.library(false).await.map_err(map_error)?;
        if books.is_empty() {
            return Err((
                "auth_expired".to_string(),
                "library came back empty; cookies may have expired".into(),
            ));
        }

        let targets: Vec<String> = books
            .iter()
            .take(PROGRESS_BOOKS)
            .map(|book| book.asin.clone())
            .collect();
        let mut points: HashMap<String, CachedPoint> = HashMap::new();
        if has_token {
            for asin in targets {
                match progress_for(client, &asin).await {
                    Ok(point) => {
                        points.insert(asin, point);
                    }
                    Err(AmazonError::AuthExpired) => {
                        drop(client_guard);
                        return Err(("auth_expired".to_string(), "cookies have expired".into()));
                    }
                    Err(_) => {}
                }
                tokio::time::sleep(PROGRESS_PAUSE).await;
            }
        }
        drop(client_guard);

        let region = self.state.read().await.region.clone();
        let cache = Cache {
            region,
            books: books.clone(),
            points,
            updated_at: Some(now_secs()),
        };
        save_cache(&cache);

        let mut state = self.state.write().await;
        state.lifecycle = "ready".into();
        state.error = None;
        state.error_code = None;
        state.books = books;
        state.reading = reading_from_cache(&cache);
        state.updated_at = cache.updated_at;
        state.needs_device_token = !has_token;
        Ok(())
    }

    pub async fn highlights(
        self: &Arc<Self>,
        asin: &str,
    ) -> Result<crate::models::Highlights, (String, String)> {
        self.ensure_client().await?;
        let mut client_guard = self.client.lock().await;
        let Some(client) = client_guard.as_mut() else {
            return Err(("unconfigured".to_string(), "no credentials".into()));
        };
        client.highlights(asin).await.map_err(map_error)
    }
}

async fn progress_for(client: &mut Amazon, asin: &str) -> Result<CachedPoint, AmazonError> {
    let start = client.start_reading(asin).await?;
    let progress = Progress {
        position: Some(start.last_page_read_data.position),
        device_name: Some(start.last_page_read_data.device_name.clone()),
        sync_time: Some(start.last_page_read_data.sync_time),
    };
    let percentage = if start.metadata_url.is_empty() {
        0.0
    } else {
        match client.metadata(&start.metadata_url).await {
            Ok(meta) if meta.end_position > 0 => {
                let position = start.last_page_read_data.position.max(0);
                let fraction =
                    (meta.start_position + position) as f64 / meta.end_position as f64;
                (fraction * 1000.0).round() / 10.0
            }
            _ => 0.0,
        }
    };
    Ok(CachedPoint {
        progress: Some(progress),
        percentage: Some(percentage),
    })
}

fn reading_from_cache(cache: &Cache) -> Vec<ReadingBook> {
    let mut reading: Vec<ReadingBook> = cache
        .books
        .iter()
        .filter_map(|book| {
            let point = cache.points.get(&book.asin)?;
            let percentage = point.percentage.unwrap_or(0.0);
            let progress = point.progress.clone().unwrap_or_default();
            let position = progress.position.unwrap_or(0);
            if percentage <= 0.0 && position <= 0 {
                return None;
            }
            Some(ReadingBook {
                asin: book.asin.clone(),
                title: book.title.clone(),
                authors: book.authors.clone(),
                cover_url: book.cover_url.clone(),
                web_reader_url: book.web_reader_url.clone(),
                percentage_read: percentage,
                device_name: progress.device_name.unwrap_or_default(),
                sync_time: progress.sync_time.unwrap_or(0),
            })
        })
        .collect();
    reading.sort_by(|a, b| b.sync_time.cmp(&a.sync_time));
    reading
}

fn map_error(error: AmazonError) -> (String, String) {
    match error {
        AmazonError::AuthExpired => (
            "auth_expired".to_string(),
            "Amazon signed this session out; paste fresh cookies".to_string(),
        ),
        AmazonError::Amazon { status, message } => (
            format!("amazon_{status}"),
            message,
        ),
        AmazonError::Http(message) => ("network".to_string(), message),
        AmazonError::Parse(message) => ("parse".to_string(), message),
    }
}

fn dev_credentials() -> Option<(String, String, String)> {
    let cookies = std::env::var("OMAKINDLE_COOKIES").ok()?;
    let token = std::env::var("OMAKINDLE_DEVICE_TOKEN").ok()?;
    let region = std::env::var("OMAKINDLE_REGION").unwrap_or_else(|_| "us".into());
    Some((cookies, token, region))
}

pub fn dev_credentials_present() -> bool {
    std::env::var("OMAKINDLE_COOKIES").is_ok() && std::env::var("OMAKINDLE_DEVICE_TOKEN").is_ok()
}

fn cache_path() -> PathBuf {
    let base = std::env::var("XDG_CACHE_HOME")
        .map(PathBuf::from)
        .unwrap_or_else(|_| {
            let home = std::env::var("HOME").unwrap_or_else(|_| "/tmp".into());
            PathBuf::from(home).join(".cache")
        });
    base.join("omakindle").join("state.json")
}

fn load_cache() -> Cache {
    let path = cache_path();
    std::fs::read_to_string(&path)
        .ok()
        .and_then(|raw| serde_json::from_str(&raw).ok())
        .unwrap_or_default()
}

fn save_cache(cache: &Cache) {
    let path = cache_path();
    if let Some(parent) = path.parent() {
        let _ = std::fs::create_dir_all(parent);
    }
    if let Ok(raw) = serde_json::to_string(cache) {
        let _ = std::fs::write(path, raw);
    }
}

fn now_secs() -> u64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map(|duration| duration.as_secs())
        .unwrap_or(0)
}
