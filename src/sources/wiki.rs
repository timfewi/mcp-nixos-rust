//! NixOS Wiki source (wiki.nixos.org).

use std::sync::LazyLock;

use percent_encoding::{AsciiSet, NON_ALPHANUMERIC, utf8_percent_encode};
use regex::Regex;
use serde_json::Value;

use crate::App;
use crate::config::WIKI_API;
use crate::html::{fragment_text_without, strip_html};
use crate::http::FetchError;
use crate::text::{collapse, commas, ellipsize, err, error, str_of, strip};

/// Rendered-page chrome that carries no article content: the language bar,
/// maintenance notices, citation markers, references and edit-section links.
const CHROME_SELECTORS: [&str; 6] =
    [".mw-pt-languages", ".noprint", ".box", ".mw-references-wrap", "sup.reference", ".mw-editsection"];
const NOT_FOUND_CODES: [&str; 3] = ["missingtitle", "invalidtitle", "nosuchsection"];
const EXTRACT_LIMIT: usize = 1500;

/// `urllib.parse.quote(value, safe="")`.
const QUOTE: &AsciiSet = &NON_ALPHANUMERIC.remove(b'_').remove(b'.').remove(b'-').remove(b'~');

// The wiki's Translate markup is not rendered, so `<translate>` tags and
// `<!--T:123-->` unit markers survive into the page text as literal strings.
static TRANSLATE_MARKUP: LazyLock<Regex> = LazyLock::new(|| crate::text::regex(r"</?translate>|<!--T:\d+-->"));
static SPACE_BEFORE_PUNCT: LazyLock<Regex> = LazyLock::new(|| crate::text::regex(r"\s+([,.;:!?)\]])"));

fn describe(failure: &FetchError) -> String {
    match failure {
        FetchError::Timeout => error("Wiki API timed out", "TIMEOUT"),
        FetchError::Parse(msg) => err(msg),
        other => error(format!("Wiki API error: {other}"), "API_ERROR"),
    }
}

fn page_url(title: &str) -> String {
    format!("https://wiki.nixos.org/wiki/{}", utf8_percent_encode(&title.replace(' ', "_"), QUOTE))
}

/// Search NixOS Wiki via MediaWiki API.
pub async fn search_wiki(app: &App, query: &str, limit: i64) -> String {
    // "home-manager" -> "home manager" finds the "Home Manager" page.
    let normalized = query.replace('-', " ");
    let limit = limit.to_string();
    let params = [
        ("action", "query"),
        ("list", "search"),
        ("srsearch", normalized.as_str()),
        ("format", "json"),
        ("utf8", "1"),
        ("srlimit", limit.as_str()),
    ];
    let request = app.http.get(WIKI_API).query(&params);
    let data = match app.http.send(request, 15).await.and_then(|resp| resp.ok()?.json()) {
        Ok(data) => data,
        Err(failure) => return describe(&failure),
    };
    let items = data.pointer("/query/search").and_then(Value::as_array).map_or(&[][..], Vec::as_slice);
    if items.is_empty() {
        return format!("No wiki articles found matching '{query}'");
    }
    let mut results = vec![format!("Found {} wiki articles matching '{query}':\n", items.len())];
    for item in items {
        let title = str_of(item, "title");
        results.push(format!("* {title}"));
        results.push(format!("  {}", page_url(title)));
        let snippet = strip_html(str_of(item, "snippet"));
        if !snippet.is_empty() {
            results.push(format!("  {}", ellipsize(&snippet, 200)));
        }
        let wordcount = item.get("wordcount").and_then(Value::as_i64).unwrap_or(0);
        if wordcount != 0 {
            results.push(format!("  ({} words)", commas(wordcount)));
        }
        results.push(String::new());
    }
    strip(&results.join("\n")).to_owned()
}

/// Flatten the rendered intro section to plain text, dropping page chrome.
fn wiki_html_to_text(html: &str) -> String {
    let text = fragment_text_without(html, &CHROME_SELECTORS);
    let text = collapse(&TRANSLATE_MARKUP.replace_all(&text, " "));
    SPACE_BEFORE_PUNCT.replace_all(&text, "$1").into_owned()
}

/// Get the intro section of a wiki page as plain text.
///
/// wiki.nixos.org does not install the TextExtracts extension, so `action=parse`
/// on section 0 renders the intro, which is then stripped to text.
pub async fn info_wiki(app: &App, title: &str) -> String {
    let params = [
        ("action", "parse"),
        ("page", title),
        ("prop", "text"),
        ("section", "0"),
        ("redirects", "1"),
        ("disabletoc", "1"),
        ("disablelimitreport", "1"),
        ("disableeditsection", "1"),
        ("format", "json"),
    ];
    let request = app.http.get(WIKI_API).query(&params);
    let data = match app.http.send(request, 15).await.and_then(|resp| resp.ok()?.json()) {
        Ok(data) => data,
        Err(failure) => return describe(&failure),
    };
    if let Some(failure) = data.get("error") {
        let code = str_of(failure, "code");
        if NOT_FOUND_CODES.contains(&code) {
            return error(format!("Wiki page '{title}' not found"), "NOT_FOUND");
        }
        let info = failure.get("info").and_then(Value::as_str).unwrap_or(code);
        return error(format!("Wiki API error: {info}"), "API_ERROR");
    }
    let parsed = data.get("parse").unwrap_or(&Value::Null);
    let page_title = parsed.get("title").and_then(Value::as_str).unwrap_or(title);
    let extract = wiki_html_to_text(parsed.pointer("/text/*").and_then(Value::as_str).unwrap_or(""));

    let mut results = vec![format!("Wiki: {page_title}"), format!("URL: {}", page_url(page_title)), String::new()];
    if !extract.is_empty() {
        results.push(ellipsize(&extract, EXTRACT_LIMIT));
    }
    results.join("\n")
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn page_urls_quote_like_python() {
        assert_eq!(page_url("Home Manager"), "https://wiki.nixos.org/wiki/Home_Manager");
        assert_eq!(page_url("C++/Foo"), "https://wiki.nixos.org/wiki/C%2B%2B%2FFoo");
    }

    #[test]
    fn wiki_text_drops_chrome_and_translate_markup() {
        let html = r#"<div class="mw-pt-languages">English</div><p>Hello <sup class="reference">[1]</sup>, wiki
&lt;translate&gt;x&lt;/translate&gt; &lt;!--T:12--&gt; end .</p><div class="box">stub</div>"#;
        assert_eq!(wiki_html_to_text(html), "Hello, wiki x end.");
    }
}
