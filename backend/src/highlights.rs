use scraper::{Html, Selector};

use crate::models::{Highlight, Highlights};

pub fn parse(asin: &str, html: &str) -> Highlights {
    let document = Html::parse_fragment(html);
    let row_selector = selector("#kp-notebook-annotations > .a-row.a-spacing-base");
    let highlight_selector = selector(".kp-notebook-highlight");
    let note_selector = selector(".kp-notebook-note");
    let note_text_selector = selector("#note");
    let location_selector = selector("input#kp-annotation-location");
    let id_selector = selector("input#deleteHighlightAnnotationId");
    let metadata_selector = selector(".kp-notebook-metadata");
    let limit_selector = selector("input.kp-notebook-content-limit-state");

    let limited = document
        .select(&limit_selector)
        .next()
        .and_then(|el| el.value().attr("value"))
        .map(|value| !value.trim().is_empty())
        .unwrap_or(false);

    let mut items = Vec::new();
    for row in document.select(&row_selector) {
        let Some(highlight) = row.select(&highlight_selector).next() else {
            continue;
        };
        let text = collect_text(highlight);
        if text.is_empty() {
            continue;
        }

        let color = highlight
            .value()
            .classes()
            .find_map(|class| class.strip_prefix("kp-notebook-highlight-"))
            .map(str::to_string);

        let note = row.select(&note_selector).next().and_then(|element| {
            if element.value().classes().any(|class| class == "aok-hidden") {
                return None;
            }
            let text = element
                .select(&note_text_selector)
                .next()
                .map(collect_text)
                .filter(|text| !text.is_empty())
                .unwrap_or_else(|| collect_text(element));
            let text = strip_label(&text);
            (!text.is_empty()).then_some(text)
        });

        let location = row
            .select(&location_selector)
            .next()
            .and_then(|el| el.value().attr("value"))
            .map(str::trim)
            .filter(|value| !value.is_empty())
            .map(str::to_string);

        let id = row
            .select(&id_selector)
            .next()
            .and_then(|el| el.value().attr("value"))
            .map(str::to_string)
            .or_else(|| {
                row.value()
                    .id()
                    .and_then(|id| id.strip_prefix("highlight-"))
                    .map(str::to_string)
            });

        let page = row
            .select(&metadata_selector)
            .next()
            .and_then(|el| page_from_metadata(&collect_text(el)));

        items.push(Highlight {
            id,
            text,
            note,
            color,
            location,
            page,
        });
    }

    Highlights {
        asin: asin.to_string(),
        count: items.len(),
        limited,
        items,
    }
}

fn selector(value: &str) -> Selector {
    Selector::parse(value).expect("static selector")
}

fn collect_text(element: scraper::ElementRef) -> String {
    element
        .text()
        .map(str::trim)
        .filter(|part| !part.is_empty())
        .collect::<Vec<_>>()
        .join(" ")
        .trim()
        .to_string()
}

fn strip_label(text: &str) -> String {
    let lower = text.to_ascii_lowercase();
    for label in ["note:", "nota:", "note :", "nota :"] {
        if lower.starts_with(label) {
            return text[label.len()..].trim().to_string();
        }
    }
    text.trim().to_string()
}

fn page_from_metadata(text: &str) -> Option<i64> {
    for part in text.split('|') {
        let lower = part.to_ascii_lowercase();
        if lower.contains("page") || lower.contains("página") || lower.contains("pagina") {
            let digits: String = part
                .chars()
                .filter(|c| c.is_ascii_digit() || *c == ',')
                .collect();
            let cleaned = digits.replace(',', "");
            if let Ok(value) = cleaned.parse::<i64>() {
                return Some(value);
            }
        }
    }
    None
}

#[cfg(test)]
mod tests {
    use super::parse;

    const FIXTURE: &str = r#"
<div id="kp-notebook-annotations">
  <input class="kp-notebook-content-limit-state" value="token">
  <div class="a-row a-spacing-base">
    <div class="a-column a-span10 kp-notebook-row-separator">
      <div class="a-row">
        <div class="a-column a-span8">
          <span class="a-size-small a-color-secondary kp-notebook-metadata">Yellow highlight | Page: 12 | Location: 146</span>
        </div>
      </div>
      <div class="a-row a-spacing-top-medium">
        <div class="a-column a-span10 kp-notebook-print-override">
          <div class="a-row kp-notebook-highlight kp-notebook-selectable kp-notebook-highlight-blue">First sample highlight</div>
          <div class="a-row a-spacing-top-base kp-notebook-note kp-notebook-selectable">
            <span class="a-color-secondary">Note:</span>
            <span id="note">A private note</span>
          </div>
        </div>
      </div>
    </div>
    <input id="kp-annotation-location" value="146">
    <input id="deleteHighlightAnnotationId" value="QUJD">
  </div>
  <div class="a-row a-spacing-base">
    <div class="a-column a-span10 kp-notebook-row-separator">
      <div class="a-row">
        <div class="a-column a-span8">
          <span class="kp-notebook-metadata">Yellow highlight | Location: 905</span>
        </div>
      </div>
      <div class="a-row a-spacing-top-medium">
        <div class="a-column a-span10 kp-notebook-print-override">
          <div class="a-row kp-notebook-highlight kp-notebook-selectable kp-notebook-highlight-yellow">Second sample highlight</div>
          <div class="a-row a-spacing-top-base kp-notebook-note aok-hidden kp-notebook-selectable">
            <span class="a-color-secondary">Note:</span>
            <span id="note"></span>
          </div>
        </div>
      </div>
    </div>
    <input id="kp-annotation-location" value="905">
  </div>
</div>
"#;

    #[test]
    fn parses_rows_with_color_note_location_and_page() {
        let parsed = parse("B000TEST", FIXTURE);
        assert_eq!(parsed.count, 2);
        assert!(parsed.limited);

        let first = &parsed.items[0];
        assert_eq!(first.text, "First sample highlight");
        assert_eq!(first.color.as_deref(), Some("blue"));
        assert_eq!(first.note.as_deref(), Some("A private note"));
        assert_eq!(first.location.as_deref(), Some("146"));
        assert_eq!(first.page, Some(12));
        assert_eq!(first.id.as_deref(), Some("QUJD"));

        let second = &parsed.items[1];
        assert_eq!(second.color.as_deref(), Some("yellow"));
        assert_eq!(second.note, None);
        assert_eq!(second.location.as_deref(), Some("905"));
        assert_eq!(second.page, None);
    }

    #[test]
    fn reports_no_highlights_for_empty_fragment() {
        let parsed = parse("B000TEST", "<div id=\"kp-notebook-annotations\"></div>");
        assert_eq!(parsed.count, 0);
        assert!(!parsed.limited);
    }
}
