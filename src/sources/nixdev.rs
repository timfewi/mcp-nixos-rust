//! nix.dev documentation source.

use std::collections::{HashMap, HashSet};

use percent_encoding::percent_decode_str;

use crate::App;
use crate::caches::NixDevIndex;
use crate::config::NIXDEV_BASE_URL;
use crate::http::FetchError;
use crate::text::{err, error, strip};

/// Cap the markdown body at 200KB (~50k tokens) to avoid swamping the LLM.
const MAX_MD_BYTES: usize = 200 * 1024;

/// Shortest index stem allowed to match by prefix; below this prefix matches
/// are noise rather than stems.
const MIN_STEM_LEN: usize = 3;

/// Score documents by term matches. Sphinx stores Porter-stemmed terms and the
/// stem is a prefix of the word in nearly every case, so a query word that
/// *starts with* an index term counts as an exact match. A `titleterms` hit
/// outweighs a body hit, ranking dedicated pages above passing mentions.
fn score_documents(index: &NixDevIndex, query: &str) -> Vec<(usize, i64)> {
    let mut scores = HashMap::<usize, i64>::new();
    let mut add = |ids: &[usize], weight: i64| {
        for id in ids {
            *scores.entry(*id).or_insert(0) += weight;
        }
    };
    let query_lower = query.to_lowercase();
    for term in crate::text::words(&query_lower) {
        let chars: Vec<char> = term.chars().collect();
        let mut stems: HashSet<String> = (MIN_STEM_LEN..chars.len()).map(|k| chars[..k].iter().collect()).collect();
        stems.insert(term.to_owned());
        for (stem_index, exact_weight, partial_weight) in [(&index.terms, 2, 1), (&index.titleterms, 5, 0)] {
            for stem in &stems {
                if let Some(ids) = stem_index.get(stem) {
                    add(ids, exact_weight);
                }
            }
            if partial_weight == 0 {
                continue;
            }
            // Partial matches (query word inside a longer index term) still need
            // one pass over the body index.
            for (index_term, ids) in stem_index {
                if !stems.contains(index_term) && index_term.contains(term) {
                    add(ids, partial_weight);
                }
            }
        }
    }
    for (id, title) in index.titles.iter().enumerate() {
        if title.to_lowercase().contains(&query_lower) {
            add(&[id], 5);
        }
    }
    let mut sorted: Vec<(usize, i64)> = scores.into_iter().collect();
    sorted.sort_by(|a, b| b.1.cmp(&a.1).then(a.0.cmp(&b.0)));
    sorted
}

/// Search nix.dev documentation via the cached Sphinx index.
pub async fn search_nixdev(app: &App, query: &str, limit: i64) -> String {
    let index = match app.caches.nixdev_index(&app.http).await {
        Ok(index) => index,
        Err(failure) => return error(failure, "API_ERROR"),
    };
    let mut docs = score_documents(&index, query);
    if docs.is_empty() {
        return format!("No nix.dev documentation found matching '{query}'");
    }
    docs.truncate(limit as usize);
    let mut results = vec![format!("Found {} nix.dev docs matching '{query}':\n", docs.len())];
    for (id, _) in docs {
        if let (Some(title), Some(docname)) = (index.titles.get(id), index.docnames.get(id)) {
            results.push(format!("* {title}"));
            results.push(format!("  {NIXDEV_BASE_URL}/{docname}"));
            results.push(String::new());
        }
    }
    strip(&results.join("\n")).to_owned()
}

/// Normalize a nix.dev query into a docname. Accepts a bare docname, a nix.dev
/// URL, or a rendered `.html` URL; percent-decodes first so the traversal guard
/// sees real path segments instead of `%2e%2e` escapes.
fn normalize_docname(query: &str) -> String {
    let decoded = percent_decode_str(strip(query)).decode_utf8_lossy();
    let name = decoded.strip_prefix(NIXDEV_BASE_URL).unwrap_or(&decoded);
    let name = name.split(['#', '?']).next().unwrap_or("").trim_start_matches('/');
    name.strip_suffix(".html").unwrap_or(name).to_owned()
}

/// Pull the first markdown H1 from a page body.
fn extract_title<'a>(body: &'a str, fallback: &'a str) -> &'a str {
    for line in body.lines().map(strip) {
        if let Some(title) = line.strip_prefix("# ") {
            let title = strip(title);
            return if title.is_empty() { fallback } else { title };
        }
    }
    fallback
}

/// Fetch a nix.dev page as markdown.
pub async fn info_nixdev(app: &App, query: &str) -> String {
    if strip(query).is_empty() {
        return err("Query required for nix-dev info (docname or URL)");
    }
    let docname = normalize_docname(query);
    if docname.is_empty() {
        return err("Empty docname after normalization");
    }
    // The docname is interpolated into a URL path.
    if docname.split('/').any(|part| part == "..") {
        return err("Invalid docname: path traversal not allowed");
    }

    let url = format!("{NIXDEV_BASE_URL}/_sources/{docname}.md");
    let request = app.http.get(&url);
    let response = match app.http.send_truncated(request, 15, MAX_MD_BYTES).await {
        Ok(response) if response.status() == 404 => {
            return error(format!("nix.dev page not found: {docname}"), "NOT_FOUND");
        }
        Ok(response) => response.ok(),
        Err(failure) => Err(failure),
    };
    let response = match response {
        Ok(response) => response,
        Err(FetchError::Timeout) => return error("nix.dev request timed out", "TIMEOUT"),
        Err(failure) => return error(format!("nix.dev request failed: {failure}"), "API_ERROR"),
    };

    let truncated = response.body.len() > MAX_MD_BYTES;
    let bytes = &response.body[..response.body.len().min(MAX_MD_BYTES)];
    // Truncating on a byte boundary can split a multi-byte codepoint.
    let body = String::from_utf8_lossy(bytes).replace('\u{fffd}', "");
    let mut lines = vec![
        format!("Title: {}", extract_title(&body, &docname)),
        format!("Source: {NIXDEV_BASE_URL}/{docname}.html"),
        format!("Docname: {docname}"),
        String::new(),
        body.trim_end().to_owned(),
    ];
    if truncated {
        lines.push(String::new());
        lines.push("[truncated]".to_owned());
    }
    lines.join("\n")
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn docnames_normalize_from_urls() {
        assert_eq!(normalize_docname("tutorials/nix-language"), "tutorials/nix-language");
        assert_eq!(normalize_docname("https://nix.dev/tutorials/nix-language"), "tutorials/nix-language");
        assert_eq!(normalize_docname("https://nix.dev/a/b.html#frag"), "a/b");
        assert_eq!(normalize_docname("/a?x=1"), "a");
        assert_eq!(normalize_docname("%2e%2e/etc"), "../etc");
    }

    #[test]
    fn title_falls_back_to_docname() {
        assert_eq!(extract_title("text\n# Hello \n", "x"), "Hello");
        assert_eq!(extract_title("no heading", "x"), "x");
    }

    #[test]
    fn stems_and_titles_rank_documents() {
        let index = NixDevIndex {
            docnames: vec!["a".into(), "b".into()],
            titles: vec!["Packaging".into(), "Other".into()],
            terms: HashMap::from([("deriv".to_owned(), vec![1]), ("xderivationx".to_owned(), vec![0])]),
            titleterms: HashMap::from([("deriv".to_owned(), vec![0])]),
        };
        // doc 0: titleterm 5 + partial 1; doc 1: body stem 2.
        assert_eq!(score_documents(&index, "derivation"), vec![(0, 6), (1, 2)]);
    }
}
