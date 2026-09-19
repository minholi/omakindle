use serde::{Deserialize, Serialize};

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct Book {
    pub asin: String,
    pub title: String,
    pub authors: Vec<String>,
    pub cover_url: String,
    pub web_reader_url: String,
    pub resource_type: String,
    pub origin_type: String,
}

#[derive(Debug, Clone, Serialize, Deserialize, Default)]
#[serde(rename_all = "camelCase")]
pub struct Progress {
    pub position: Option<i64>,
    pub device_name: Option<String>,
    pub sync_time: Option<i64>,
}


#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct Highlight {
    #[serde(default)]
    pub id: Option<String>,
    #[serde(default)]
    pub text: String,
    #[serde(default)]
    pub note: Option<String>,
    #[serde(default)]
    pub color: Option<String>,
    #[serde(default)]
    pub location: Option<String>,
    #[serde(default)]
    pub page: Option<i64>,
    #[serde(default)]
    pub position_type: Option<String>,
    #[serde(default)]
    pub start: Option<i64>,
    #[serde(default)]
    pub end: Option<i64>,
    #[serde(default)]
    pub truncated: bool,
    #[serde(default)]
    pub verified: bool,
    #[serde(default)]
    pub modified_at: Option<i64>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct Highlights {
    pub asin: String,
    pub count: usize,
    pub limited: bool,
    pub items: Vec<Highlight>,
}

#[derive(Debug, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct LibraryPage {
    #[serde(default)]
    pub items_list: Vec<LibraryItem>,
    #[serde(default)]
    pub pagination_token: Option<String>,
}

#[derive(Debug, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct LibraryItem {
    pub title: String,
    pub asin: String,
    #[serde(default)]
    pub authors: Vec<String>,
    #[serde(default)]
    pub product_url: String,
    #[serde(default)]
    pub web_reader_url: String,
    #[serde(default)]
    pub resource_type: String,
    #[serde(default)]
    pub origin_type: String,
}

impl LibraryItem {
    pub fn into_book(self) -> Book {
        Book {
            asin: self.asin,
            title: self.title,
            authors: normalize_authors(&self.authors),
            cover_url: large_cover(&self.product_url),
            web_reader_url: self.web_reader_url,
            resource_type: self.resource_type,
            origin_type: self.origin_type,
        }
    }
}

#[allow(dead_code)]
#[derive(Debug, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct DeviceInfo {
    #[serde(default)]
    pub client_hash_id: String,
    #[serde(default)]
    pub device_name: String,
    #[serde(default)]
    pub device_session_token: String,
    #[serde(default)]
    pub eid: String,
}

#[allow(dead_code)]
#[derive(Debug, Deserialize, Default)]
#[serde(rename_all = "camelCase")]
pub struct StartReading {
    #[serde(default)]
    pub is_owned: bool,
    #[serde(default)]
    pub is_sample: bool,
    #[serde(default)]
    pub format_version: String,
    #[serde(default, rename = "YJFormatVersion")]
    pub yj_format_version: String,
    #[serde(default)]
    pub metadata_url: String,
    #[serde(default)]
    pub content_version: String,
    #[serde(default)]
    pub srl: Option<i64>,
    #[serde(default)]
    pub last_page_read_data: LastPageRead,
}

#[derive(Debug, Deserialize, Default)]
#[serde(rename_all = "camelCase")]
pub struct LastPageRead {
    #[serde(default)]
    pub device_name: String,
    #[serde(default)]
    pub position: i64,
    #[serde(default)]
    pub sync_time: i64,
}

#[allow(dead_code)]
#[derive(Debug, Deserialize, Default)]
#[serde(rename_all = "camelCase")]
pub struct BookMetadata {
    #[serde(default)]
    pub start_position: i64,
    #[serde(default)]
    pub end_position: i64,
    #[serde(default)]
    pub release_date: Option<String>,
    #[serde(default)]
    pub publisher: Option<String>,
    #[serde(default, alias = "authorList")]
    pub author_list: Vec<String>,
}

pub fn normalize_authors(raw: &[String]) -> Vec<String> {
    let mut out: Vec<String> = Vec::new();
    for group in raw {
        for entry in group.split(':') {
            let entry = entry.trim();
            if entry.is_empty() {
                continue;
            }
            let parts: Vec<&str> = entry.split(',').map(str::trim).collect();
            let name = if parts.len() >= 2 {
                let mut reversed = parts.clone();
                reversed.reverse();
                reversed.join(" ")
            } else {
                parts.join(" ")
            };
            if !out.contains(&name) {
                out.push(name);
            }
        }
    }
    out
}

fn large_cover(url: &str) -> String {
    let mut out = String::with_capacity(url.len());
    let mut rest = url;
    while let Some(start) = rest.find("._SY") {
        out.push_str(&rest[..start]);
        let after = &rest[start + 4..];
        if let Some(end) = after.find("_.") {
            if after[..end].chars().all(|c| c.is_ascii_digit()) {
                out.push('.');
                rest = &after[end + 2..];
                continue;
            }
        }
        out.push_str("._SY");
        rest = after;
    }
    out.push_str(rest);
    out
}

#[cfg(test)]
mod tests {
    use super::{large_cover, normalize_authors};

    #[test]
    fn normalizes_amazon_author_format() {
        assert_eq!(
            normalize_authors(&["Azevedo, Rodrigo:".to_string()]),
            vec!["Rodrigo Azevedo"]
        );
        assert_eq!(
            normalize_authors(&["King, Stephen:Straub, Peter".to_string()]),
            vec!["Stephen King", "Peter Straub"]
        );
        assert_eq!(normalize_authors(&["Maddox".to_string()]), vec!["Maddox"]);
        assert!(normalize_authors(&[]).is_empty());
    }

    #[test]
    fn strips_cover_size_marker() {
        assert_eq!(
            large_cover("https://m.media-amazon.com/images/I/51QDpG6S6pL._SY400_.jpg"),
            "https://m.media-amazon.com/images/I/51QDpG6S6pL.jpg"
        );
        assert_eq!(
            large_cover("https://m.media-amazon.com/images/I/51QDpG6S6pL.jpg"),
            "https://m.media-amazon.com/images/I/51QDpG6S6pL.jpg"
        );
    }
}
