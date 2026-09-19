use std::collections::HashMap;
use std::path::PathBuf;
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::Arc;
use std::time::{Duration, SystemTime, UNIX_EPOCH};

use serde::{Deserialize, Serialize};
use tokio::sync::{broadcast, Mutex, RwLock};

use crate::amazon::{Amazon, Credentials, Error as AmazonError};
use crate::models::{Book, Highlight, Highlights, Progress};

const PROGRESS_BOOKS: usize = 12;
const PROGRESS_PAUSE: Duration = Duration::from_millis(400);
const HIGHLIGHTS_TTL: Duration = Duration::from_secs(30 * 60);
const HIGHLIGHTS_PAUSE: Duration = Duration::from_millis(300);
const PREVIEW_SUSPECT_LENGTH: usize = 80;
const RECENT_SCAN_CAP: usize = 12;

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

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
struct CachedHighlights {
    fetched_at: u64,
    #[serde(default)]
    guid: String,
    #[serde(default)]
    revision: String,
    items: Vec<Highlight>,
}

impl CachedHighlights {
    fn into_highlights(self, asin: &str) -> Highlights {
        let limited = self.items.iter().any(|item| item.truncated);
        Highlights {
            asin: asin.to_string(),
            count: self.items.len(),
            limited,
            items: self.items,
        }
    }

    fn is_fresh(&self, now: u64) -> bool {
        now.saturating_sub(self.fetched_at) < HIGHLIGHTS_TTL.as_secs()
    }
}

#[derive(Debug, Clone, Serialize, Deserialize, Default)]
struct HighlightsCache {
    #[serde(default)]
    books: HashMap<String, CachedHighlights>,
}

pub struct HighlightsResult {
    pub highlights: Highlights,
    pub cached: bool,
    pub stale: bool,
    pub fetched_at: u64,
}

pub struct App {
    pub state: RwLock<Snapshot>,
    pub client: Mutex<Option<Amazon>>,
    pub refresh_lock: Mutex<()>,
    pub events: broadcast::Sender<String>,
    highlights: Mutex<HighlightsCache>,
    highlights_locks: Mutex<HashMap<String, Arc<Mutex<()>>>>,
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
            highlights: Mutex::new(load_highlights_cache()),
            highlights_locks: Mutex::new(HashMap::new()),
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
        self.persist_session().await;

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
        force: bool,
        enrich: bool,
    ) -> Result<HighlightsResult, (String, String)> {
        self.ensure_client().await?;
        if enrich {
            let app = self.clone();
            let target = asin.to_string();
            tokio::spawn(async move {
                app.enrich_highlights(&target).await;
            });
        }
        let now = now_secs();
        let cached = {
            let cache = self.highlights.lock().await;
            cache
                .books
                .get(asin)
                .map(|entry| (entry.clone(), entry.is_fresh(now)))
        };
        if let Some((entry, fresh)) = cached {
            if fresh && !force {
                return Ok(HighlightsResult {
                    fetched_at: entry.fetched_at,
                    highlights: entry.into_highlights(asin),
                    cached: true,
                    stale: false,
                });
            }
            if !force {
                let app = self.clone();
                let target = asin.to_string();
                tokio::spawn(async move {
                    let _ = app.fetch_highlights(&target, false).await;
                });
                return Ok(HighlightsResult {
                    fetched_at: entry.fetched_at,
                    highlights: entry.into_highlights(asin),
                    cached: true,
                    stale: true,
                });
            }
        }

        let entry = self.fetch_highlights(asin, force).await?;
        Ok(HighlightsResult {
            fetched_at: entry.fetched_at,
            highlights: entry.into_highlights(asin),
            cached: false,
            stale: false,
        })
    }

    async fn asin_lock(&self, asin: &str) -> Arc<Mutex<()>> {
        let mut locks = self.highlights_locks.lock().await;
        locks
            .entry(asin.to_string())
            .or_insert_with(|| Arc::new(Mutex::new(())))
            .clone()
    }

    async fn fetch_highlights(
        self: &Arc<Self>,
        asin: &str,
        force: bool,
    ) -> Result<CachedHighlights, (String, String)> {
        let lock = self.asin_lock(asin).await;
        let _guard = lock.lock().await;

        if !force {
            if let Some(entry) = self.highlights.lock().await.books.get(asin).cloned() {
                if entry.is_fresh(now_secs()) && !entry.guid.is_empty() {
                    return Ok(entry);
                }
            }
        }

        let (highlights, guid, revision) = {
            let mut client_guard = self.client.lock().await;
            let Some(client) = client_guard.as_mut() else {
                return Err(("unconfigured".to_string(), "no credentials".into()));
            };
            if !client.has_device_token() {
                return Err((
                    "needs_device_token".to_string(),
                    "Paste the device token in Settings to load highlights".into(),
                ));
            }
            let annotations = client.annotations(asin).await.map_err(map_error)?;
            (annotations.highlights, annotations.guid, annotations.revision)
        };
        self.persist_session().await;

        let previous = self.highlights.lock().await.books.get(asin).cloned();
        if guid.is_empty() && highlights.items.is_empty() {
            // Amazon refused reading data (for example ContentLicenseExceeded);
            // keep whatever was cached instead of wiping it.
            if let Some(previous) = previous.as_ref() {
                return Ok(previous.clone());
            }
        }
        let entry = CachedHighlights {
            fetched_at: now_secs(),
            guid,
            revision,
            items: merge_full_texts(previous.as_ref(), highlights.items),
        };
        {
            let mut cache = self.highlights.lock().await;
            cache.books.insert(asin.to_string(), entry.clone());
            save_highlights_cache(&cache);
        }
        self.emit_highlights(asin, &entry).await;
        Ok(entry)
    }

    /// Fetch the full text for a highlight through the reader's copy API.
    /// Updates the cached item and notifies clients when the text improves.
    pub async fn highlight_text(
        self: &Arc<Self>,
        asin: &str,
        start: i64,
        end: i64,
    ) -> Result<String, (String, String)> {
        self.ensure_client().await?;
        if let Some(text) = cached_full_text(self, asin, start, end).await {
            return Ok(text);
        }
        let entry = {
            let cache = self.highlights.lock().await;
            cache.books.get(asin).cloned()
        };
        let entry = match entry {
            Some(entry) if !entry.guid.is_empty() && !entry.revision.is_empty() => entry,
            _ => self.fetch_highlights(asin, false).await?,
        };
        if entry.guid.is_empty() || entry.revision.is_empty() {
            return Err((
                "unavailable".to_string(),
                "Amazon does not provide reading data for this book".into(),
            ));
        }

        let lock = self.asin_lock(asin).await;
        let _guard = lock.lock().await;
        if let Some(text) = cached_full_text(self, asin, start, end).await {
            return Ok(text);
        }

        let text = {
            let mut client_guard = self.client.lock().await;
            let Some(client) = client_guard.as_mut() else {
                return Err(("unconfigured".to_string(), "no credentials".into()));
            };
            client
                .copy_text(asin, &entry.guid, &entry.revision, start, end)
                .await
                .map_err(map_error)?
        };
        self.persist_session().await;
        self.store_full_text(asin, start, end, &text).await;
        Ok(text)
    }

    /// Replace previews with full passages in the background; emits
    /// `highlights_changed` after each improvement so panels update live.
    async fn enrich_highlights(self: &Arc<Self>, asin: &str) {
        let Some(entry) = self.highlights.lock().await.books.get(asin).cloned() else {
            return;
        };
        let entry = if entry.guid.is_empty() || entry.revision.is_empty() {
            match self.fetch_highlights(asin, true).await {
                Ok(refreshed) => refreshed,
                Err(_) => return,
            }
        } else {
            entry
        };
        if entry.guid.is_empty() || entry.revision.is_empty() {
            return;
        }

        let lock = self.asin_lock(asin).await;
        let _guard = lock.lock().await;
        let targets: Vec<(i64, i64)> = entry
            .items
            .iter()
            .filter(|item| !item.verified && !item.text.is_empty())
            .filter(|item| item.truncated || item.text.chars().count() >= PREVIEW_SUSPECT_LENGTH)
            .filter_map(|item| match (item.start, item.end) {
                (Some(start), Some(end)) if end > start => Some((start, end)),
                _ => None,
            })
            .collect();
        if targets.is_empty() {
            return;
        }

        let mut fetched = false;
        for (start, end) in targets {
            if cached_full_text(self, asin, start, end).await.is_some() {
                continue;
            }
            let text = {
                let mut client_guard = self.client.lock().await;
                let Some(client) = client_guard.as_mut() else {
                    return;
                };
                match client
                    .copy_text(asin, &entry.guid, &entry.revision, start, end)
                    .await
                {
                    Ok(text) => text,
                    Err(_) => {
                        tokio::time::sleep(HIGHLIGHTS_PAUSE).await;
                        continue;
                    }
                }
            };
            fetched = true;
            self.store_full_text(asin, start, end, &text).await;
            tokio::time::sleep(HIGHLIGHTS_PAUSE).await;
        }
        if fetched {
            self.persist_session().await;
        }
    }

    async fn store_full_text(self: &Arc<Self>, asin: &str, start: i64, end: i64, text: &str) {
        let snapshot = {
            let mut cache = self.highlights.lock().await;
            let Some(current) = cache.books.get_mut(asin) else {
                return;
            };
            let Some(item) = current
                .items
                .iter_mut()
                .find(|item| item.start == Some(start) && item.end == Some(end))
            else {
                return;
            };
            let improved = text.chars().count() > item.text.chars().count();
            if improved {
                item.text = text.to_string();
            }
            item.truncated = false;
            item.verified = true;
            let snapshot = current.clone();
            save_highlights_cache(&cache);
            snapshot
        };
        self.emit_highlights(asin, &snapshot).await;
    }

    pub async fn recent_highlights(
        self: &Arc<Self>,
        limit: usize,
        per_book: usize,
        force: bool,
    ) -> Result<serde_json::Value, (String, String)> {
        self.ensure_client().await?;
        let state = self.snapshot().await;
        let limit = limit.clamp(1, 50);
        let per_book = per_book.clamp(1, 20);

        let mut candidates: Vec<(String, String, String)> = Vec::new();
        let mut seen: std::collections::HashSet<String> = std::collections::HashSet::new();
        for book in &state.reading {
            if seen.insert(book.asin.clone()) {
                candidates.push((book.asin.clone(), book.title.clone(), book.authors.join(", ")));
            }
        }
        for book in &state.books {
            if seen.insert(book.asin.clone()) {
                candidates.push((book.asin.clone(), book.title.clone(), book.authors.join(", ")));
            }
        }

        let mut items: Vec<serde_json::Value> = Vec::new();
        let mut scanned = 0usize;
        for (asin, title, authors) in candidates {
            if items.len() >= limit || scanned >= RECENT_SCAN_CAP {
                break;
            }
            scanned += 1;
            match self.highlights(&asin, force, false).await {
                Ok(result) => {
                    for item in result.highlights.items.into_iter().take(per_book) {
                        if items.len() >= limit {
                            break;
                        }
                        items.push(enrich_highlight(item, &asin, &title, &authors));
                    }
                    if !result.cached {
                        tokio::time::sleep(HIGHLIGHTS_PAUSE).await;
                    }
                }
                Err(_) => continue,
            }
        }
        Ok(serde_json::json!({ "items": items, "scanned": scanned }))
    }

    async fn emit_highlights(&self, asin: &str, entry: &CachedHighlights) {
        let payload = serde_json::json!({
            "type": "event",
            "v": 1,
            "event": "highlights_changed",
            "asin": asin,
            "highlights": entry.clone().into_highlights(asin),
        });
        let _ = self.events.send(payload.to_string());
    }

    /// Amazon rotates `session-token` and friends per response; persist the
    /// refreshed cookie header so a daemon restart keeps the newest session.
    async fn persist_session(self: &Arc<Self>) {
        let (cookies, device_token) = {
            let guard = self.client.lock().await;
            let Some(client) = guard.as_ref() else {
                return;
            };
            (client.cookies(), client.device_token().to_string())
        };
        if let Ok(Some((stored, _, _))) = crate::secrets::load().await {
            if stored == cookies {
                return;
            }
        }
        let region = self.state.read().await.region.clone();
        let _ = crate::secrets::store(&cookies, &device_token, &region).await;
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

fn enrich_highlight(item: Highlight, asin: &str, title: &str, authors: &str) -> serde_json::Value {
    let mut value = serde_json::to_value(item).unwrap_or(serde_json::Value::Null);
    if let Some(object) = value.as_object_mut() {
        object.insert("bookAsin".into(), serde_json::json!(asin));
        object.insert("bookTitle".into(), serde_json::json!(title));
        object.insert("bookAuthors".into(), serde_json::json!(authors));
    }
    value
}

/// Keep full texts fetched earlier when a refresh brings back previews.
fn merge_full_texts(previous: Option<&CachedHighlights>, items: Vec<Highlight>) -> Vec<Highlight> {
    let Some(previous) = previous else {
        return items;
    };
    let mut known: HashMap<(i64, i64), &Highlight> = HashMap::new();
    for item in &previous.items {
        if !item.verified {
            continue;
        }
        if let (Some(start), Some(end)) = (item.start, item.end) {
            known.entry((start, end)).or_insert(item);
        }
    }
    items
        .into_iter()
        .map(|mut item| {
            if let (Some(start), Some(end)) = (item.start, item.end) {
                if let Some(old) = known.get(&(start, end)) {
                    if old.text.chars().count() >= item.text.chars().count() {
                        item.text = old.text.clone();
                        item.truncated = false;
                        item.verified = true;
                    }
                }
            }
            item
        })
        .collect()
}

async fn cached_full_text(app: &Arc<App>, asin: &str, start: i64, end: i64) -> Option<String> {
    let cache = app.highlights.lock().await;
    let item = cache
        .books
        .get(asin)?
        .items
        .iter()
        .find(|item| item.start == Some(start) && item.end == Some(end))?;
    if !item.verified || item.text.is_empty() {
        return None;
    }
    Some(item.text.clone())
}

fn cache_dir() -> PathBuf {
    let base = std::env::var("XDG_CACHE_HOME")
        .map(PathBuf::from)
        .unwrap_or_else(|_| {
            let home = std::env::var("HOME").unwrap_or_else(|_| "/tmp".into());
            PathBuf::from(home).join(".cache")
        });
    base.join("omakindle")
}

fn cache_path() -> PathBuf {
    cache_dir().join("state.json")
}

fn load_cache() -> Cache {
    let path = cache_path();
    std::fs::read_to_string(&path)
        .ok()
        .and_then(|raw| serde_json::from_str(&raw).ok())
        .unwrap_or_default()
}

fn save_cache(cache: &Cache) {
    write_json(&cache_path(), cache);
}

fn highlights_cache_path() -> PathBuf {
    cache_dir().join("annotations.json")
}

fn load_highlights_cache() -> HighlightsCache {
    let path = highlights_cache_path();
    std::fs::read_to_string(&path)
        .ok()
        .and_then(|raw| serde_json::from_str(&raw).ok())
        .unwrap_or_default()
}

fn save_highlights_cache(cache: &HighlightsCache) {
    write_json(&highlights_cache_path(), cache);
}

fn write_json<T: Serialize>(path: &std::path::Path, value: &T) {
    let Some(parent) = path.parent() else {
        return;
    };
    let _ = std::fs::create_dir_all(parent);
    let Ok(raw) = serde_json::to_string(value) else {
        return;
    };
    let temporary = path.with_extension("json.tmp");
    if std::fs::write(&temporary, raw).is_ok() {
        let _ = std::fs::rename(&temporary, path);
    }
}

fn now_secs() -> u64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map(|duration| duration.as_secs())
        .unwrap_or(0)
}

#[cfg(test)]
mod tests {
    use super::{merge_full_texts, CachedHighlights};
    use crate::models::Highlight;

    fn item(start: i64, end: i64, text: &str, truncated: bool, verified: bool) -> Highlight {
        Highlight {
            id: None,
            text: text.to_string(),
            note: None,
            color: None,
            location: Some(start.to_string()),
            page: None,
            position_type: None,
            start: Some(start),
            end: Some(end),
            truncated,
            verified,
            modified_at: None,
        }
    }

    fn entry(items: Vec<Highlight>) -> CachedHighlights {
        CachedHighlights {
            fetched_at: 0,
            guid: "CR!TEST".into(),
            revision: "abc123".into(),
            items,
        }
    }

    #[test]
    fn keeps_verified_text_when_refresh_returns_a_preview() {
        let previous = entry(vec![item(10, 120, "the full passage fetched earlier", false, true)]);
        let merged = merge_full_texts(Some(&previous), vec![item(10, 120, "the preview", true, false)]);
        assert_eq!(merged[0].text, "the full passage fetched earlier");
        assert!(!merged[0].truncated);
        assert!(merged[0].verified);
    }

    #[test]
    fn prefers_the_longer_verified_text() {
        let previous = entry(vec![item(10, 120, "short", false, true)]);
        let merged = merge_full_texts(
            Some(&previous),
            vec![item(10, 120, "a much longer fresh preview", true, false)],
        );
        assert_eq!(merged[0].text, "a much longer fresh preview");
        assert!(merged[0].truncated);
        assert!(!merged[0].verified);
    }

    #[test]
    fn ignores_unverified_entries_from_the_previous_cache() {
        let previous = entry(vec![item(10, 120, "old preview text", true, false)]);
        let merged = merge_full_texts(Some(&previous), vec![item(10, 120, "new preview", true, false)]);
        assert_eq!(merged[0].text, "new preview");
        assert!(!merged[0].verified);
    }

    #[test]
    fn keeps_verified_text_even_when_the_refresh_preview_is_as_long() {
        let previous = entry(vec![item(10, 120, "same length text that was verified", false, true)]);
        let merged = merge_full_texts(
            Some(&previous),
            vec![item(10, 120, "same length text that was verified", true, false)],
        );
        assert!(!merged[0].truncated);
        assert!(merged[0].verified);
    }
}
