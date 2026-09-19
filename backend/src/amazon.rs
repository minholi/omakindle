use std::fmt;
use std::time::Duration;

use serde::Deserialize;
use wreq::header::{HeaderMap, HeaderValue};
use wreq_util::{Emulation, Platform, Profile};

use crate::highlights;
use crate::models::{
    Book, BookMetadata, DeviceInfo, Highlights, LibraryPage, Progress, StartReading,
};

pub const REGION_SUFFIXES: &[(&str, &str)] = &[
    ("us", "com"),
    ("uk", "co.uk"),
    ("de", "de"),
    ("fr", "fr"),
    ("it", "it"),
    ("es", "es"),
    ("jp", "co.jp"),
    ("ca", "ca"),
    ("au", "com.au"),
    ("in", "in"),
    ("br", "com.br"),
    ("mx", "com.mx"),
    ("nl", "nl"),
];

const REQUIRED_COOKIES: [&str; 4] = ["ubid-main", "at-main", "x-main", "session-id"];

#[derive(Debug)]
pub enum Error {
    AuthExpired,
    Http(String),
    Amazon { status: u16, message: String },
    Parse(String),
}

pub struct Annotations {
    pub highlights: Highlights,
    pub guid: String,
    pub revision: String,
}

#[derive(Deserialize)]
struct CopyTextResponse {
    #[serde(default)]
    status: Option<String>,
    #[serde(default)]
    text: String,
}

impl fmt::Display for Error {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Error::AuthExpired => write!(f, "Amazon session expired or cookies incomplete"),
            Error::Http(message) => write!(f, "network error: {message}"),
            Error::Amazon { status, message } => write!(f, "Amazon returned {status}: {message}"),
            Error::Parse(message) => write!(f, "could not parse Amazon response: {message}"),
        }
    }
}

impl std::error::Error for Error {}

#[derive(Clone)]
pub struct Credentials {
    cookies: Vec<(String, String)>,
    device_token: String,
}

impl Credentials {
    pub fn new(cookies: &str, device_token: &str) -> Result<Self, Error> {
        let parsed = parse_cookies(cookies);
        let missing: Vec<&str> = REQUIRED_COOKIES
            .iter()
            .filter(|name| !parsed.iter().any(|(key, _)| key == *name))
            .copied()
            .collect();
        if !missing.is_empty() {
            return Err(Error::Parse(format!(
                "missing cookies: {}",
                missing.join(", ")
            )));
        }
        let device_token = extract_device_token(device_token);
        Ok(Self {
            cookies: parsed,
            device_token,
        })
    }

    pub fn has_device_token(&self) -> bool {
        !self.device_token.is_empty()
    }

    pub fn cookie_header(&self) -> String {
        self.cookies
            .iter()
            .map(|(name, value)| format!("{name}={value}"))
            .collect::<Vec<_>>()
            .join("; ")
    }

    fn get(&self, name: &str) -> Option<&str> {
        self.cookies
            .iter()
            .find(|(key, _)| key == name)
            .map(|(_, value)| value.as_str())
    }

    fn set(&mut self, name: &str, value: String) {
        if let Some(entry) = self.cookies.iter_mut().find(|(key, _)| key == name) {
            entry.1 = value;
        } else {
            self.cookies.push((name.to_string(), value));
        }
    }
}

pub fn region_suffix(region: &str) -> String {
    let region = region.to_ascii_lowercase();
    REGION_SUFFIXES
        .iter()
        .find(|(key, _)| *key == region)
        .map(|(_, suffix)| (*suffix).to_string())
        .unwrap_or(region)
}

pub fn build_client() -> Result<wreq::Client, Error> {
    let emulation = Emulation::builder()
        .profile(Profile::Chrome149)
        .platform(Platform::MacOS)
        .build();
    wreq::Client::builder()
        .emulation(emulation)
        .timeout(Duration::from_secs(30))
        .build()
        .map_err(|error| Error::Http(error.to_string()))
}

pub struct Amazon {
    client: wreq::Client,
    base: String,
    credentials: Credentials,
    session_id: String,
    adp_session_token: String,
}

impl Amazon {
    pub fn new(region: &str, credentials: Credentials) -> Result<Self, Error> {
        let client = build_client()?;
        let session_id = credentials
            .get("session-id")
            .unwrap_or_default()
            .to_string();
        Ok(Self {
            client,
            base: format!("https://read.amazon.{}", region_suffix(region)),
            credentials,
            session_id,
            adp_session_token: String::new(),
        })
    }

    pub fn has_device_token(&self) -> bool {
        self.credentials.has_device_token()
    }

    fn headers(&self, adp: bool) -> Result<HeaderMap, Error> {
        let mut headers = HeaderMap::new();
        let cookie = self.credentials.cookie_header();
        headers.insert(
            wreq::header::COOKIE,
            HeaderValue::from_str(&cookie).map_err(|e| Error::Parse(e.to_string()))?,
        );
        headers.insert(
            wreq::header::ACCEPT_LANGUAGE,
            HeaderValue::from_static("en-US,en;q=0.9"),
        );
        headers.insert(
            wreq::header::REFERER,
            HeaderValue::from_str(&format!("{}/kindle-library", self.base))
                .map_err(|e| Error::Parse(e.to_string()))?,
        );
        if !self.session_id.is_empty() {
            headers.insert(
                "x-amzn-sessionid",
                HeaderValue::from_str(&self.session_id).map_err(|e| Error::Parse(e.to_string()))?,
            );
        }
        if adp && !self.adp_session_token.is_empty() {
            headers.insert(
                "x-adp-session-token",
                HeaderValue::from_str(&self.adp_session_token)
                    .map_err(|e| Error::Parse(e.to_string()))?,
            );
        }
        Ok(headers)
    }

    async fn get(&mut self, url: &str, adp: bool) -> Result<(u16, HeaderMap, String, String), Error> {
        let request = self.client.get(url);
        self.send(request, adp).await
    }

    async fn post_json(
        &mut self,
        url: &str,
        body: &serde_json::Value,
    ) -> Result<(u16, HeaderMap, String, String), Error> {
        let payload = serde_json::to_string(body).map_err(|error| Error::Parse(error.to_string()))?;
        let request = self
            .client
            .post(url)
            .header(wreq::header::CONTENT_TYPE, "application/json")
            .body(payload);
        self.send(request, true).await
    }

    async fn send(
        &mut self,
        request: wreq::RequestBuilder,
        adp: bool,
    ) -> Result<(u16, HeaderMap, String, String), Error> {
        let headers = self.headers(adp)?;
        let response = request
            .headers(headers)
            .send()
            .await
            .map_err(|error| Error::Http(error.to_string()))?;

        let status = response.status().as_u16();
        let final_url = response.uri().to_string();
        let headers = response.headers().clone();
        for cookie in response.cookies() {
            self.credentials
                .set(cookie.name(), cookie.value().to_string());
            if cookie.name() == "session-id" {
                self.session_id = cookie.value().to_string();
            }
        }
        let body = response
            .text()
            .await
            .map_err(|error| Error::Http(error.to_string()))?;

        if final_url.contains("signin") {
            return Err(Error::AuthExpired);
        }
        if status == 401 || status == 403 {
            return Err(Error::Amazon {
                status,
                message: body.chars().take(200).collect(),
            });
        }
        if status >= 400 {
            return Err(Error::Amazon {
                status,
                message: body.chars().take(200).collect(),
            });
        }
        Ok((status, headers, final_url, body))
    }

    pub async fn library(&mut self, fetch_all: bool) -> Result<Vec<Book>, Error> {
        let mut books = Vec::new();
        let mut pagination_token: Option<String> = None;
        loop {
            let mut url = format!(
                "{}/kindle-library/search?query=&libraryType=BOOKS&sortType=recency&querySize=50",
                self.base
            );
            if let Some(token) = &pagination_token {
                url.push_str("&paginationToken=");
                url.push_str(&urlencode(token));
            }
            let (_, _, _, body) = self.get(&url, false).await?;
            let page: LibraryPage = serde_json::from_str(&body).map_err(|error| {
                Error::Parse(format!("library response was not JSON: {error}"))
            })?;
            let items = page.items_list.len();
            books.extend(page.items_list.into_iter().map(|item| item.into_book()));
            match (fetch_all, page.pagination_token) {
                (true, Some(token)) if !token.is_empty() && items > 0 => {
                    pagination_token = Some(token);
                }
                _ => break,
            }
        }
        Ok(books)
    }

    pub async fn register_device(&mut self) -> Result<DeviceInfo, Error> {
        if !self.credentials.has_device_token() {
            return Err(Error::Parse("device token not set".into()));
        }
        let token = self.credentials.device_token.clone();
        let url = format!(
            "{}/service/web/register/getDeviceToken?serialNumber={}&deviceType={}",
            self.base,
            urlencode(&token),
            urlencode(&token)
        );
        let (_, _, _, body) = self.get(&url, false).await?;
        let info: DeviceInfo = serde_json::from_str(&body)
            .map_err(|error| Error::Parse(format!("device token response: {error}")))?;
        if info.device_session_token.is_empty() {
            return Err(Error::Parse("device token response had no session token".into()));
        }
        self.adp_session_token = info.device_session_token.clone();
        Ok(info)
    }

    pub async fn start_reading(&mut self, asin: &str) -> Result<StartReading, Error> {
        if self.adp_session_token.is_empty() {
            self.register_device().await?;
        }
        let url = format!(
            "{}/service/mobile/reader/startReading?asin={}&clientVersion=20000100",
            self.base,
            urlencode(asin)
        );
        let (_, _, _, body) = self.get(&url, true).await?;
        serde_json::from_str(&body)
            .map_err(|error| Error::Parse(format!("startReading response: {error}")))
    }

    pub async fn metadata(&mut self, url: &str) -> Result<BookMetadata, Error> {
        let (_, _, _, body) = self.get(url, true).await?;
        let start = body
            .find('(')
            .ok_or_else(|| Error::Parse("metadata JSONP wrapper missing".into()))?;
        let end = body
            .rfind(')')
            .ok_or_else(|| Error::Parse("metadata JSONP wrapper missing".into()))?;
        if end <= start {
            return Err(Error::Parse("metadata JSONP wrapper malformed".into()));
        }
        serde_json::from_str(&body[start + 1..end])
            .map_err(|error| Error::Parse(format!("metadata JSONP body: {error}")))
    }

    pub async fn progress(&mut self, asin: &str) -> Result<Progress, Error> {
        let start = self.start_reading(asin).await?;
        let progress = Progress {
            position: Some(start.last_page_read_data.position),
            device_name: Some(start.last_page_read_data.device_name),
            sync_time: Some(start.last_page_read_data.sync_time),
        };
        Ok(progress)
    }

    pub async fn percentage_read(&mut self, asin: &str) -> Result<f64, Error> {
        let start = self.start_reading(asin).await?;
        if start.metadata_url.is_empty() {
            return Ok(0.0);
        }
        let meta = self.metadata(&start.metadata_url).await?;
        if meta.end_position <= 0 {
            return Ok(0.0);
        }
        let position = start.last_page_read_data.position.max(0);
        let fraction =
            (meta.start_position + position) as f64 / meta.end_position as f64;
        Ok((fraction * 1000.0).round() / 10.0)
    }

    pub async fn annotations(&mut self, asin: &str) -> Result<Annotations, Error> {
        if self.adp_session_token.is_empty() {
            self.register_device().await?;
        }
        let start = self.start_reading(asin).await?;
        let guid = if !start.yj_format_version.trim().is_empty() {
            start.yj_format_version.trim().to_string()
        } else {
            start.format_version.trim().to_string()
        };
        let revision = start.content_version.trim().to_string();
        if guid.is_empty() {
            return Ok(Annotations {
                highlights: Highlights {
                    asin: asin.to_string(),
                    count: 0,
                    limited: false,
                    items: Vec::new(),
                },
                guid,
                revision,
            });
        }
        let highlights = match self.fetch_annotations(asin, &guid).await {
            Err(Error::Amazon { status: 500, .. }) => {
                let repeated = format!("{guid},{guid}");
                self.fetch_annotations(asin, &repeated).await?
            }
            other => other?,
        };
        Ok(Annotations {
            highlights,
            guid,
            revision,
        })
    }

    pub async fn highlights(&mut self, asin: &str) -> Result<Highlights, Error> {
        Ok(self.annotations(asin).await?.highlights)
    }

    /// Fetch the full text for a position range through the reader's copy API.
    pub async fn copy_text(
        &mut self,
        asin: &str,
        guid: &str,
        revision: &str,
        start: i64,
        end: i64,
    ) -> Result<String, Error> {
        if self.adp_session_token.is_empty() {
            self.register_device().await?;
        }
        let url = format!("{}/service/mobile/reader/copyText", self.base);
        let body = serde_json::json!({
            "asin": asin,
            "startPosition": start,
            "endPosition": end,
            "guid": guid,
            "revision": revision,
            "clientVersion": "20000100",
        });
        let (_, _, _, payload) = self.post_json(&url, &body).await?;
        let parsed: CopyTextResponse = serde_json::from_str(&payload)
            .map_err(|error| Error::Parse(format!("copyText response: {error}")))?;
        if parsed.status.as_deref() != Some("Ok") || parsed.text.trim().is_empty() {
            return Err(Error::Parse(format!(
                "copyText status: {}",
                parsed.status.unwrap_or_else(|| "missing".into())
            )));
        }
        Ok(parsed.text)
    }

    async fn fetch_annotations(&mut self, asin: &str, guid: &str) -> Result<Highlights, Error> {
        let url = format!(
            "{}/service/mobile/reader/getAnnotations?asin={}&guid={}&clientVersion=20000100",
            self.base,
            urlencode(asin),
            urlencode(guid)
        );
        let (_, _, _, body) = self.get(&url, true).await?;
        highlights::parse(asin, &body)
            .map_err(|error| Error::Parse(format!("annotations response: {error}")))
    }

    pub fn cookies(&self) -> String {
        self.credentials.cookie_header()
    }

    pub fn device_token(&self) -> &str {
        &self.credentials.device_token
    }
}

fn extract_device_token(raw: &str) -> String {
    let raw = raw.trim();
    let value = match raw.find("serialNumber=") {
        Some(index) => {
            let rest = &raw[index + "serialNumber=".len()..];
            rest.split(['&', ' ', '\t', '\n', '\r']).next().unwrap_or(rest)
        }
        None => raw,
    };
    percent_decode(value.trim())
}

fn percent_decode(value: &str) -> String {
    let bytes = value.as_bytes();
    let mut out = Vec::with_capacity(bytes.len());
    let mut index = 0;
    while index < bytes.len() {
        if bytes[index] == b'%' && index + 2 < bytes.len() {
            let hex = std::str::from_utf8(&bytes[index + 1..index + 3]).ok();
            if let Some(byte) = hex.and_then(|hex| u8::from_str_radix(hex, 16).ok()) {
                out.push(byte);
                index += 3;
                continue;
            }
        }
        out.push(bytes[index]);
        index += 1;
    }
    String::from_utf8_lossy(&out).into_owned()
}

fn parse_cookies(raw: &str) -> Vec<(String, String)> {    let mut cookies = Vec::new();
    for chunk in raw.replace(['\n', '\r'], ";").split(';') {
        let chunk = chunk.trim();
        if chunk.is_empty() {
            continue;
        }
        if let Some((name, value)) = chunk.split_once('=') {
            let name = name.trim();
            if name.is_empty() {
                continue;
            }
            cookies.push((name.to_string(), value.trim().to_string()));
        }
    }
    cookies
}

fn urlencode(value: &str) -> String {
    let mut out = String::with_capacity(value.len());
    for byte in value.bytes() {
        match byte {
            b'A'..=b'Z' | b'a'..=b'z' | b'0'..=b'9' | b'-' | b'_' | b'.' | b'~' => {
                out.push(byte as char)
            }
            _ => out.push_str(&format!("%{byte:02X}")),
        }
    }
    out
}

#[cfg(test)]
mod tests {
    use super::{extract_device_token, Credentials, Error, REGION_SUFFIXES, region_suffix};

    #[test]
    fn extracts_device_token_from_devtools_url() {
        let url = "https://read.amazon.com/service/web/register/getDeviceToken?serialNumber=A2C3D4E5F6G7H8&deviceType=A2C3D4E5F6G7H8";
        assert_eq!(extract_device_token(url), "A2C3D4E5F6G7H8");
        assert_eq!(extract_device_token("A2C3D4E5F6G7H8"), "A2C3D4E5F6G7H8");
        assert_eq!(
            extract_device_token("  serialNumber=A%2FB%20C&deviceType=A%2FB%20C  "),
            "A/B C"
        );
    }

    #[test]
    fn credentials_require_all_four_cookies() {
        let error = match Credentials::new("ubid-main=x; at-main=y", "token") {
            Ok(_) => panic!("expected missing cookies to fail"),
            Err(error) => error,
        };
        match error {
            Error::Parse(message) => assert!(message.contains("session-id")),
            other => panic!("unexpected error: {other}"),
        }
    }

    #[test]
    fn credentials_accept_url_device_token() {
        let credentials = Credentials::new(
            "ubid-main=a; at-main=b; x-main=c; session-id=d",
            "https://read.amazon.com/service/web/register/getDeviceToken?serialNumber=SERIAL123&deviceType=SERIAL123",
        )
        .unwrap();
        assert_eq!(credentials.device_token, "SERIAL123");
        assert_eq!(
            REGION_SUFFIXES[0],
            ("us", "com"),
            "region table must keep its expected shape"
        );
        assert_eq!(region_suffix("uk"), "co.uk");
        assert_eq!(region_suffix("us"), "com");
    }
}
