use serde::Deserialize;

use crate::models::{Highlight, Highlights};

const PREVIEW_CAP_HINT: usize = 91;
const TRUNCATION_MISMATCH: i64 = 40;

#[derive(Deserialize, Default)]
struct AnnotationsResponse {
    #[serde(default)]
    annotations: Vec<Annotation>,
}

#[derive(Deserialize, Default)]
#[serde(rename_all = "camelCase")]
struct Annotation {
    #[serde(default, rename = "type")]
    kind: String,
    #[serde(default)]
    context: String,
    #[serde(default)]
    note: Option<String>,
    #[serde(default)]
    highlight_color: Option<String>,
    #[serde(default)]
    position: Option<i64>,
    #[serde(default)]
    start: Option<i64>,
    #[serde(default)]
    end: Option<i64>,
    #[serde(default)]
    position_type: Option<String>,
    #[serde(default)]
    guid: Option<String>,
    #[serde(default)]
    modified_timestamp: Option<i64>,
}

/// Parse the JSON returned by `/service/mobile/reader/getAnnotations`.
///
/// Amazon returns `context` for highlights as a short preview (roughly 100
/// characters) even though `start`/`end` describe the full passage, so items
/// carry a `truncated` flag computed from the range length.
pub fn parse(asin: &str, body: &str) -> Result<Highlights, serde_json::Error> {
    let response: AnnotationsResponse = serde_json::from_str(body)?;
    let mut items = Vec::new();
    let mut limited = false;
    for annotation in response.annotations {
        if annotation.kind == "kindle.bookmark" {
            continue;
        }
        let text = annotation.context.trim().to_string();
        let note = annotation
            .note
            .map(|value| value.trim().to_string())
            .filter(|value| !value.is_empty());
        if text.is_empty() && note.is_none() {
            continue;
        }
        let length = text.chars().count() as i64;
        let truncated = if annotation.kind == "kindle.highlight" {
            let range = match (annotation.start, annotation.end) {
                (Some(start), Some(end)) if end > start => end - start,
                _ => 0,
            };
            length as usize >= PREVIEW_CAP_HINT || range - length > TRUNCATION_MISMATCH
        } else {
            false
        };
        limited |= truncated;
        let location = annotation
            .position
            .or(annotation.start)
            .map(|value| value.to_string());
        items.push(Highlight {
            id: annotation.guid.map(|guid| match annotation.position {
                Some(position) => format!("{guid}:{position}"),
                None => guid,
            }),
            text,
            note,
            color: annotation
                .highlight_color
                .filter(|value| !value.trim().is_empty()),
            location,
            page: None,
            position_type: annotation.position_type,
            start: annotation.start,
            end: annotation.end,
            truncated,
            verified: false,
            modified_at: annotation.modified_timestamp,
        });
    }
    Ok(Highlights {
        asin: asin.to_string(),
        count: items.len(),
        limited,
        items,
    })
}

#[cfg(test)]
mod tests {
    use super::parse;

    const FIXTURE: &str = r#"{
      "annotations": [
        {
          "type": "kindle.highlight",
          "asin": "B000TEST",
          "context": "First sample highlight",
          "highlightColor": "yellow",
          "position": 146,
          "start": 140,
          "end": 162,
          "positionType": "Mobi7",
          "guid": "CR!TESTGUID",
          "modifiedTimestamp": 1779929553000,
          "note": "A private note"
        },
        {
          "type": "kindle.highlight",
          "asin": "B000TEST",
          "context": "Truncated preview of a much longer passage that continues beyond the preview window Amazon returns here",
          "highlightColor": null,
          "position": 905,
          "start": 900,
          "end": 1100,
          "positionType": "Mobi7",
          "guid": "CR!TESTGUID",
          "modifiedTimestamp": 1779929689000,
          "note": null
        },
        {
          "type": "kindle.note",
          "asin": "B000TEST",
          "context": "",
          "position": 1200,
          "start": 1195,
          "end": 1195,
          "positionType": "Mobi7",
          "guid": "CR!TESTGUID",
          "modifiedTimestamp": 1779929795000,
          "note": "A standalone note"
        },
        {
          "type": "kindle.bookmark",
          "asin": "B000TEST",
          "context": "Bookmarked sentence text",
          "position": 2000,
          "start": 2000,
          "end": -1,
          "guid": "CR!TESTGUID",
          "modifiedTimestamp": 1779929800000
        }
      ]
    }"#;

    #[test]
    fn parses_highlights_notes_and_skips_bookmarks() {
        let parsed = parse("B000TEST", FIXTURE).unwrap();
        assert_eq!(parsed.count, 3);
        assert!(parsed.limited, "truncated preview must mark the book as limited");

        let first = &parsed.items[0];
        assert_eq!(first.text, "First sample highlight");
        assert_eq!(first.color.as_deref(), Some("yellow"));
        assert_eq!(first.note.as_deref(), Some("A private note"));
        assert_eq!(first.location.as_deref(), Some("146"));
        assert_eq!(first.start, Some(140));
        assert_eq!(first.end, Some(162));
        assert_eq!(first.position_type.as_deref(), Some("Mobi7"));
        assert!(!first.truncated);
        assert_eq!(first.id.as_deref(), Some("CR!TESTGUID:146"));

        let second = &parsed.items[1];
        assert!(second.truncated);
        assert_eq!(second.color, None);

        let third = &parsed.items[2];
        assert_eq!(third.text, "");
        assert_eq!(third.note.as_deref(), Some("A standalone note"));
        assert!(!third.truncated);
    }

    #[test]
    fn reports_no_highlights_for_empty_annotations() {
        let parsed = parse("B000TEST", r#"{"annotations": []}"#).unwrap();
        assert_eq!(parsed.count, 0);
        assert!(!parsed.limited);
    }

    #[test]
    fn flags_preview_capped_highlights_as_truncated() {
        let preview = "d".repeat(100);
        let body = format!(
            r#"{{"annotations": [{{"type": "kindle.highlight", "context": "{preview}", "start": 141841, "end": 141957}}]}}"#
        );
        let parsed = parse("B000TEST", &body).unwrap();
        assert!(parsed.items[0].truncated);
        assert!(parsed.limited);
    }

    #[test]
    fn keeps_short_highlights_untruncated() {
        let body = r#"{"annotations": [{"type": "kindle.highlight", "context": "Sem ésteres, as cervejas seriam bastante insossas.", "start": 96520, "end": 96569}]}"#;
        let parsed = parse("B000TEST", body).unwrap();
        assert!(!parsed.items[0].truncated);
        assert!(!parsed.limited);
    }

    #[test]
    fn rejects_bodies_that_are_not_annotation_json() {
        assert!(parse("B000TEST", "<html>sign in</html>").is_err());
    }
}
