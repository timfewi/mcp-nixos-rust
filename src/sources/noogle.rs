//! Noogle source (noogle.dev - Nix function API search).

use crate::App;
use crate::caches::NoogleDoc;
use crate::text::{by_count, by_count_then_key, commas, ellipsize, error, strip, tally};

macro_rules! docs_or_return {
    ($app:expr) => {
        match $app.caches.noogle(&$app.http).await {
            Ok(docs) => docs,
            Err(failure) => return error(failure, "API_ERROR"),
        }
    };
}

fn score(doc: &NoogleDoc, query: &str) -> i32 {
    let path = doc.path.to_lowercase();
    if path == query {
        100
    } else if path.rsplit('.').next() == Some(query) {
        // Exact function name: `map` must outrank `concatMap`.
        80
    } else if path.contains(query) {
        if path.ends_with(query) { 50 } else { 30 }
    } else if doc.aliases.iter().any(|alias| alias.to_lowercase().contains(query)) {
        40
    } else if doc.description.to_lowercase().contains(query) {
        10
    } else {
        0
    }
}

/// Search Noogle functions by name, path, or documentation content.
pub async fn search_noogle(app: &App, query: &str, limit: i64) -> String {
    let docs = docs_or_return!(app);
    let query_lower = query.to_lowercase();
    let mut matches: Vec<(i32, &NoogleDoc)> =
        docs.iter().map(|doc| (score(doc, &query_lower), doc)).filter(|(score, _)| *score > 0).collect();
    if matches.is_empty() {
        return format!("No Noogle functions found matching '{query}'");
    }
    matches.sort_by(|a, b| b.0.cmp(&a.0).then_with(|| a.1.path.cmp(&b.1.path)));
    matches.truncate(limit as usize);

    let mut results = vec![format!("Found {} Noogle functions matching '{query}':\n", matches.len())];
    for (_, doc) in matches {
        results.push(format!("* {}", doc.path));
        if !doc.signature.is_empty() {
            results.push(format!("  Type: {}", ellipsize(&doc.signature, 100)));
        }
        if !doc.description.is_empty() {
            results.push(format!("  {}", ellipsize(&doc.description, 200)));
        }
        if !doc.aliases.is_empty() {
            results.push(format!("  Aliases: {}", doc.aliases[..doc.aliases.len().min(3)].join(", ")));
        }
        results.push(String::new());
    }
    strip(&results.join("\n")).to_owned()
}

/// Prefer the document whose canonical path is the queried name, then one that
/// lists it as an alias. Alias groups can be large, and the first document in
/// index order is not the one the user asked about.
fn find_doc<'a>(docs: &'a [NoogleDoc], name_lower: &str) -> (Option<&'a NoogleDoc>, Vec<&'a str>) {
    let mut alias_match = None;
    let mut partial = Vec::new();
    for doc in docs {
        let path = doc.path.to_lowercase();
        if path == name_lower {
            return (Some(doc), partial);
        }
        if alias_match.is_none() && doc.aliases.iter().any(|alias| alias.to_lowercase() == name_lower) {
            alias_match = Some(doc);
        } else if path.contains(name_lower) {
            partial.push(doc.path.as_str());
        }
    }
    (alias_match, partial)
}

/// Get detailed info for a specific Noogle function.
pub async fn info_noogle(app: &App, name: &str) -> String {
    let docs = docs_or_return!(app);
    let (found, partial) = find_doc(&docs, &name.to_lowercase());
    let Some(doc) = found else {
        if partial.is_empty() {
            return error(format!("Noogle function '{name}' not found"), "NOT_FOUND");
        }
        let suggestions = partial[..partial.len().min(5)].join(", ");
        return error(format!("Function '{name}' not found. Similar: {suggestions}"), "NOT_FOUND");
    };

    let mut results = vec![format!("Noogle Function: {}", doc.path)];
    if !doc.signature.is_empty() {
        results.push(format!("Type: {}", doc.signature));
    }
    results.push(format!("Path: {}", doc.path));
    if !doc.aliases.is_empty() {
        results.push(format!("Aliases: {}", doc.aliases.join(", ")));
    }
    if let Some((arity, args)) = &doc.primop {
        results.push(if args.is_empty() {
            format!("Primop: Yes (arity: {arity})")
        } else {
            format!("Primop: Yes (arity: {arity}, args: {})", args.join(", "))
        });
    }
    results.push(String::new());
    if !doc.description.is_empty() {
        results.push("Description:".to_owned());
        results.push(doc.description.clone());
        results.push(String::new());
    }
    if !doc.example.is_empty() {
        results.push("Example:".to_owned());
        results.push(ellipsize(&doc.example, 500));
        results.push(String::new());
    }
    if !doc.position_file.is_empty() {
        results.push(match &doc.position_line {
            Some(line) => format!("Source: {}:{line}", doc.position_file),
            None => format!("Source: {}", doc.position_file),
        });
    }
    strip(&results.join("\n")).to_owned()
}

/// `lib.strings.concat` -> `lib.strings`; dot-less paths are their own category.
fn category(path: &str) -> &str {
    path.match_indices('.').nth(1).map_or(path, |(end, _)| &path[..end])
}

fn categories(docs: &[NoogleDoc]) -> Vec<(String, i64)> {
    tally(docs.iter().map(|doc| category(&doc.path)))
}

/// Get Noogle statistics.
pub async fn stats_noogle(app: &App) -> String {
    let docs = docs_or_return!(app);
    let counts = categories(&docs);
    let with_signatures = docs.iter().filter(|doc| !doc.signature.is_empty()).count();
    let with_docs = docs.iter().filter(|doc| !doc.description.is_empty()).count();
    let mut results = vec![
        "Noogle Statistics:".to_owned(),
        format!("* Total functions: {}", commas(docs.len() as i64)),
        format!("* With type signatures: {}", commas(with_signatures as i64)),
        format!("* With documentation: {}", commas(with_docs as i64)),
        format!("* Categories: {}", counts.len()),
        "* Top categories:".to_owned(),
    ];
    for (cat, count) in by_count(counts).into_iter().take(10) {
        results.push(format!("  - {cat}: {count}"));
    }
    results.push(String::new());
    results.push("Data source: noogle.dev (updated daily)".to_owned());
    results.join("\n")
}

/// Browse Noogle functions by prefix, or list categories if no prefix.
pub async fn browse_noogle(app: &App, prefix: &str) -> String {
    let docs = docs_or_return!(app);
    if prefix.is_empty() {
        let counts = categories(&docs);
        let mut results = vec![format!("Noogle function categories ({} total):\n", counts.len())];
        for (cat, count) in by_count_then_key(counts) {
            results.push(format!("* {cat} ({count} functions)"));
        }
        return results.join("\n");
    }
    let prefix_lower = prefix.to_lowercase();
    let prefix_dot = if prefix_lower.ends_with('.') { prefix_lower.clone() } else { format!("{prefix_lower}.") };
    let mut matches: Vec<&NoogleDoc> = docs
        .iter()
        .filter(|doc| {
            let path = doc.path.to_lowercase();
            path.starts_with(&prefix_dot) || path == prefix_lower
        })
        .collect();
    if matches.is_empty() {
        return format!("No Noogle functions found with prefix '{prefix}'");
    }
    matches.sort_by(|a, b| a.path.cmp(&b.path));
    let mut results = vec![format!("Noogle functions with prefix '{prefix}' ({} found):\n", matches.len())];
    for doc in matches.iter().take(100) {
        results.push(format!("* {}", doc.path));
        if !doc.signature.is_empty() {
            results.push(format!("  Type: {}", ellipsize(&doc.signature, 80)));
        }
        if !doc.description.is_empty() {
            results.push(format!("  {}", ellipsize(&doc.description, 150)));
        }
        results.push(String::new());
    }
    if matches.len() > 100 {
        results.push(format!("... and {} more functions", matches.len() - 100));
    }
    strip(&results.join("\n")).to_owned()
}

#[cfg(test)]
mod tests {
    use super::*;

    fn doc(path: &str) -> NoogleDoc {
        NoogleDoc { path: path.to_owned(), ..NoogleDoc::default() }
    }

    #[test]
    fn exact_function_name_outranks_substring() {
        assert_eq!(score(&doc("lib.lists.map"), "map"), 80);
        assert_eq!(score(&doc("lib.lists.concatmap"), "map"), 50);
        assert_eq!(score(&doc("lib.mapattrs"), "map"), 30);
        assert_eq!(score(&doc("map"), "map"), 100);
    }

    #[test]
    fn exact_path_beats_alias() {
        let mut aliased = doc("lib.trivial.id");
        aliased.aliases = vec!["id".to_owned()];
        let docs = vec![aliased, doc("id")];
        assert_eq!(find_doc(&docs, "id").0.map(|d| d.path.as_str()), Some("id"));
    }

    #[test]
    fn categories_take_two_segments() {
        assert_eq!(category("lib.strings.concat"), "lib.strings");
        assert_eq!(category("lib.id"), "lib.id");
        assert_eq!(category("map"), "map");
    }
}
