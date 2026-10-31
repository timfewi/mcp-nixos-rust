//! NixOS packages and options source.

use std::collections::BTreeSet;
use std::sync::LazyLock;

use regex::Regex;
use serde_json::{Value, json};

use super::base::{channel_suggestions, es_query};
use super::flakes::search_flakes;
use crate::App;
use crate::config::NIXOS_API;
use crate::text::{commas, display, err, error, first_chars, str_of, strings, strip, truthy, words};

// Field weights mirror the search.nixos.org frontend Elm source (nixos-search:
// frontend/src/Search/Query.elm). Each `field^w` is followed by `field.*^(w*0.6)`
// so multi-field sub-fields such as `package_attr_name.edge` contribute less.
const PACKAGE_SEARCH_FIELDS: [&str; 10] = [
    "package_attr_name^9.0",
    "package_attr_name.*^5.4",
    "package_programs^9.0",
    "package_programs.*^5.4",
    "package_pname^6.0",
    "package_pname.*^3.6",
    "package_description^1.3",
    "package_description.*^0.78",
    "package_longDescription^1.0",
    "package_longDescription.*^0.6",
];
const OPTION_SEARCH_FIELDS: [&str; 4] =
    ["option_name^6.0", "option_name.*^3.6", "option_description^1.0", "option_description.*^0.6"];

const PACKAGE_PHRASE_FIELDS: [&str; 2] = ["package_description^3", "package_longDescription^1"];
const OPTION_PHRASE_FIELDS: [&str; 1] = ["option_description^3"];

/// `rank_feature` popularity signals, packages only: (field, saturation pivot).
const PACKAGE_POPULARITY: [(&str, f64); 2] = [("package_repology_repos", 20.0), ("package_dep_count", 1000.0)];

/// Wildcard is a term-level scan, so an overlong query makes substring matching
/// noticeably slower. 200 chars covers any realistic name while blocking
/// LLM-generated paragraphs.
const MAX_QUERY_LEN: usize = 200;

static TAG: LazyLock<Regex> = LazyLock::new(|| crate::text::regex(r"<[^>]+>"));

/// Lower-case the query and split it into clause-safe words. ES wildcard
/// metacharacters are stripped rather than escaped: none can occur in an
/// attribute path, so `firefox*` means "firefox".
fn query_words(query: &str) -> Vec<String> {
    let cleaned: String = query.to_lowercase().chars().filter(|c| !matches!(c, '*' | '?' | '\\')).collect();
    words(&cleaned).map(str::to_owned).collect()
}

/// One case-insensitive wildcard clause per word plus its `_ ↔ -` swap variants,
/// so `musicfree` hits `musicfree-desktop` and `firefox_esr` hits `firefox-esr`.
fn wildcard_clauses(words: &[String], field: &str) -> Vec<Value> {
    let mut candidates = BTreeSet::new();
    for word in words {
        candidates.insert(word.clone());
        candidates.insert(word.replace('_', "-"));
        candidates.insert(word.replace('-', "_"));
    }
    candidates
        .into_iter()
        .map(|word| json!({"wildcard": {field: {"value": format!("*{word}*"), "case_insensitive": true}}}))
        .collect()
}

/// Relevance boosts that decide ordering. Without these the dis_max alone ranks
/// `firefox-esr-153-unwrapped` above `firefox` for the query `firefox`.
fn should_clauses(
    words: &[String],
    main_field: &str,
    phrase_fields: &[&str],
    popularity: &[(&str, f64)],
) -> Vec<Value> {
    let mut clauses = Vec::new();
    if !words.is_empty() {
        let joined = words.concat();
        clauses.push(json!({"term": {main_field: {"value": joined, "boost": 100.0}}}));
        clauses.push(json!({"prefix": {main_field: {"value": joined, "boost": 20.0, "case_insensitive": true}}}));
        if words.len() > 1 {
            clauses.push(json!({
                "constant_score": {
                    "filter": {"multi_match": {"type": "phrase", "query": words.join(" "), "fields": phrase_fields}},
                    "boost": 80.0
                }
            }));
        }
    }
    for (field, pivot) in popularity {
        clauses.push(json!({"rank_feature": {"field": field, "boost": 5.0, "saturation": {"pivot": pivot}}}));
    }
    clauses
}

/// Build the ES query mirroring the search.nixos.org frontend:
/// `bool(filter=type, must=dis_max(multi_match + wildcards), should=boosts)`.
/// The query is lower-cased because the `whitespace` analyzer does not fold case.
fn build_search_query(
    query: &str,
    type_term: &str,
    fields: &[&str],
    main_field: &str,
    phrase_fields: &[&str],
    popularity: &[(&str, f64)],
) -> Value {
    let words = query_words(query);
    let mut queries = vec![json!({
        "multi_match": {
            "type": "cross_fields",
            "query": words.join(" "),
            "analyzer": "whitespace",
            "auto_generate_synonyms_phrase_query": false,
            "operator": "and",
            "fields": fields
        }
    })];
    queries.extend(wildcard_clauses(&words, main_field));
    json!({
        "bool": {
            "filter": [{"term": {"type": type_term}}],
            "must": [{"dis_max": {"tie_breaker": 0.7, "queries": queries}}],
            "should": should_clauses(&words, main_field, phrase_fields, popularity)
        }
    })
}

/// Upstream `rescoreQuery`: re-rank the top window by inverse attribute-name
/// length so short, canonical names outrank long derived ones.
fn package_rescore() -> Value {
    json!({
        "window_size": 100,
        "query": {
            "rescore_query": {
                "function_score": {
                    "script_score": {"script": {"source": "1.0 / doc['package_attr_name'].value.length()"}}
                }
            },
            "rescore_query_weight": 20.0
        }
    })
}

fn clean_option_description(desc: &str) -> String {
    if !desc.contains("<rendered-html>") {
        return desc.to_owned();
    }
    let inner = desc.replace("<rendered-html>", "").replace("</rendered-html>", "");
    strip(&TAG.replace_all(&inner, "")).to_owned()
}

fn source_of(hit: &Value) -> &Value {
    hit.get("_source").unwrap_or(&Value::Null)
}

/// Search NixOS packages, options, or programs via Elasticsearch.
pub async fn search_nixos(app: &App, query: &str, search_type: &str, limit: i64, channel: &str) -> String {
    if search_type == "flakes" {
        return search_flakes(app, query, limit).await;
    }
    let channels = app.caches.channels.resolved(&app.http).await;
    let Some(index) = channels.get(channel) else {
        return err(format!("Invalid channel '{channel}'. {}", channel_suggestions(&channels, channel).await));
    };

    let query = first_chars(strip(query), MAX_QUERY_LEN);
    let none_found = || format!("No {search_type} found matching '{query}'");
    if query_words(query).is_empty() {
        // An empty multi_match would just be a slow way to match nothing.
        return none_found();
    }

    let (es, rescore) = match search_type {
        "options" => (
            build_search_query(query, "option", &OPTION_SEARCH_FIELDS, "option_name", &OPTION_PHRASE_FIELDS, &[]),
            None,
        ),
        // `rg` is provided by `ripgrep`, whose attr name shares nothing with the
        // query, so programs anchors on package_programs and skips the
        // attr-name-length rescore.
        "programs" => (
            build_search_query(
                query,
                "package",
                &PACKAGE_SEARCH_FIELDS,
                "package_programs",
                &PACKAGE_PHRASE_FIELDS,
                &[],
            ),
            None,
        ),
        _ => (
            build_search_query(
                query,
                "package",
                &PACKAGE_SEARCH_FIELDS,
                "package_attr_name",
                &PACKAGE_PHRASE_FIELDS,
                &PACKAGE_POPULARITY,
            ),
            Some(package_rescore()),
        ),
    };
    let hits = match es_query(app, index, es, limit, rescore).await {
        Ok(hits) => hits,
        Err(failure) => return err(failure),
    };
    if hits.is_empty() {
        return none_found();
    }

    // Count what is actually rendered: programs drops hits without an exact
    // program match, so `hits.len()` would overstate the result count.
    let mut shown = 0;
    let mut results = Vec::new();
    for hit in &hits {
        let src = source_of(hit);
        match search_type {
            "packages" => {
                let name = str_of(src, "package_pname");
                let attr_name = str_of(src, "package_attr_name");
                let shown_name = if !attr_name.is_empty() && attr_name != name { attr_name } else { name };
                results.push(format!("* {shown_name} ({})", str_of(src, "package_pversion")));
                let desc = str_of(src, "package_description");
                if !desc.is_empty() {
                    results.push(format!("  {desc}"));
                }
                results.push(String::new());
                shown += 1;
            }
            "options" => {
                results.push(format!("* {}", str_of(src, "option_name")));
                let opt_type = str_of(src, "option_type");
                if !opt_type.is_empty() {
                    results.push(format!("  Type: {opt_type}"));
                }
                let desc = clean_option_description(str_of(src, "option_description"));
                if !desc.is_empty() {
                    results.push(format!("  {desc}"));
                }
                results.push(String::new());
                shown += 1;
            }
            _ => {
                let pkg_name = str_of(src, "package_pname");
                let query_lower = query.to_lowercase();
                for program in strings(src.get("package_programs")) {
                    if program.to_lowercase() == query_lower {
                        results.push(format!("* {program} (provided by {pkg_name})"));
                        results.push(String::new());
                        shown += 1;
                    }
                }
            }
        }
    }
    if shown == 0 {
        return none_found();
    }
    let mut lines = vec![format!("Found {shown} {search_type} matching '{query}':\n")];
    lines.append(&mut results);
    strip(&lines.join("\n")).to_owned()
}

fn attr_of(hit: &Value) -> &str {
    str_of(source_of(hit), "package_attr_name")
}

fn pname_of(hit: &Value) -> &str {
    str_of(source_of(hit), "package_pname")
}

fn term_query(kind: &str, field: &str, name: &str) -> Value {
    json!({"bool": {"must": [{"term": {"type": kind}}, {"term": {field: name}}]}})
}

/// Note attached when several packages share the requested pname, naming the
/// alternatives so the caller can retry with the exact attribute path.
fn ambiguity_note(name: &str, chosen_attr: &str, candidates: &[Value], channel: &str) -> Option<String> {
    let alternatives: BTreeSet<&str> = candidates.iter().map(attr_of).filter(|attr| !attr.is_empty()).collect();
    let others: Vec<&str> = alternatives.into_iter().filter(|attr| *attr != chosen_attr).collect();
    let first = others.first()?;
    let canonical = candidates.iter().any(|hit| attr_of(hit) == pname_of(hit) && pname_of(hit) == name);
    let chosen_label = if canonical { "the canonical entry" } else { "a representative entry" };
    let retry =
        format!("{{\"action\": \"info\", \"query\": {}, \"channel\": {}}}", Value::from(*first), Value::from(channel));
    Some(format!(
        "Note: '{name}' is a pname shared by multiple packages. Returned {chosen_label} ({chosen_attr}). \
         Other attributes with the same pname: {}. Pass an exact attribute to disambiguate, e.g. {retry}.",
        others.join(", ")
    ))
}

async fn info_package(app: &App, index: &str, name: &str, channel: &str) -> Result<String, String> {
    // Priority 1: exact attribute path. One attr maps to one package, which also
    // disambiguates packages sharing a pname (firefox, firefox-esr, ...).
    let mut hits = es_query(app, index, term_query("package", "package_attr_name", name), 1, None).await?;
    let mut candidates = Vec::new();
    if hits.is_empty() {
        // Priority 2: pname. Fetch up to 5 to detect ambiguity; prefer the
        // canonical entry, else sort so the tie-break is deterministic (ES does
        // not guarantee a stable order for equal-score term matches).
        candidates = es_query(app, index, term_query("package", "package_pname", name), 5, None).await?;
        let canonical = candidates.iter().find(|hit| attr_of(hit) == pname_of(hit));
        let chosen = canonical.or_else(|| candidates.iter().min_by_key(|hit| (attr_of(hit), pname_of(hit))));
        hits = chosen.cloned().into_iter().collect();
    }
    let Some(hit) = hits.first() else {
        return Ok(error(format!("Package '{name}' not found"), "NOT_FOUND"));
    };

    let src = source_of(hit);
    let attr_name = str_of(src, "package_attr_name");
    let pname = str_of(src, "package_pname");
    let mut info = vec![format!("Package: {pname}")];
    if !attr_name.is_empty() && attr_name != pname {
        info.push(format!("Attribute: {attr_name}"));
    }
    info.push(format!("Version: {}", str_of(src, "package_pversion")));
    let desc = str_of(src, "package_description");
    if !desc.is_empty() {
        info.push(format!("Description: {desc}"));
    }
    if let Some(homepage) = src.get("package_homepage").filter(|homepage| truthy(homepage)) {
        let first = homepage.as_array().and_then(|list| list.first()).unwrap_or(homepage);
        info.push(format!("Homepage: {}", display(first)));
    }
    let licenses = strings(src.get("package_license_set"));
    if !licenses.is_empty() {
        info.push(format!("License: {}", licenses.join(", ")));
    }
    if candidates.len() > 1 {
        let chosen_attr = if attr_name.is_empty() { pname } else { attr_name };
        if let Some(note) = ambiguity_note(name, chosen_attr, &candidates, channel) {
            info.push(String::new());
            info.push(note);
        }
    }
    Ok(info.join("\n"))
}

async fn info_option(app: &App, index: &str, name: &str) -> Result<String, String> {
    let hits = es_query(app, index, term_query("option", "option_name", name), 1, None).await?;
    let Some(hit) = hits.first() else {
        return Ok(error(format!("Option '{name}' not found"), "NOT_FOUND"));
    };
    let src = source_of(hit);
    let mut info = vec![format!("Option: {}", str_of(src, "option_name"))];
    let opt_type = str_of(src, "option_type");
    if !opt_type.is_empty() {
        info.push(format!("Type: {opt_type}"));
    }
    let desc = clean_option_description(str_of(src, "option_description"));
    if !desc.is_empty() {
        info.push(format!("Description: {desc}"));
    }
    for (label, key) in [("Default", "option_default"), ("Example", "option_example")] {
        let value = str_of(src, key);
        if !value.is_empty() {
            info.push(format!("{label}: {value}"));
        }
    }
    Ok(info.join("\n"))
}

/// Get detailed info for a NixOS package or option.
pub async fn info_nixos(app: &App, name: &str, info_type: &str, channel: &str) -> String {
    let channels = app.caches.channels.resolved(&app.http).await;
    let Some(index) = channels.get(channel) else {
        return err(format!("Invalid channel '{channel}'. {}", channel_suggestions(&channels, channel).await));
    };
    let result = if info_type == "package" {
        info_package(app, index, name, channel).await
    } else {
        info_option(app, index, name).await
    };
    result.unwrap_or_else(err)
}

async fn count_type(app: &App, url: &str, kind: &str) -> i64 {
    let request = app.http.post_es(url, &json!({"query": {"term": {"type": kind}}}));
    let data = app.http.send(request, 10).await.and_then(|resp| resp.json());
    data.ok().and_then(|data| data.get("count").and_then(Value::as_i64)).unwrap_or(0)
}

/// Get NixOS package and option counts for a channel.
pub async fn stats_nixos(app: &App, channel: &str) -> String {
    let channels = app.caches.channels.resolved(&app.http).await;
    let Some(index) = channels.get(channel) else {
        return err(format!("Invalid channel '{channel}'. {}", channel_suggestions(&channels, channel).await));
    };
    let url = format!("{NIXOS_API}/{index}/_count");
    let pkg_count = count_type(app, &url, "package").await;
    let opt_count = count_type(app, &url, "option").await;
    if pkg_count == 0 && opt_count == 0 {
        return err("Failed to retrieve statistics");
    }
    format!("NixOS Statistics ({channel}):\n* Packages: {}\n* Options: {}", commas(pkg_count), commas(opt_count))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn query_words_strip_metacharacters() {
        assert_eq!(query_words("Fire*FOX? a\\b"), ["firefox", "ab"]);
        assert!(query_words("*?").is_empty());
    }

    #[test]
    fn wildcards_cover_hyphen_variants() {
        let clauses = wildcard_clauses(&["firefox_esr".to_owned()], "package_attr_name");
        let text = Value::from(clauses).to_string();
        assert!(text.contains("*firefox-esr*") && text.contains("*firefox_esr*"));
    }

    #[test]
    fn should_clauses_add_phrase_only_for_multiple_words() {
        let one = should_clauses(&["a".to_owned()], "f", &[], &[]);
        let two = should_clauses(&["a".to_owned(), "b".to_owned()], "f", &[], &[]);
        assert_eq!((one.len(), two.len()), (2, 3));
    }

    #[test]
    fn option_descriptions_drop_rendered_html() {
        assert_eq!(clean_option_description("<rendered-html><p>Hi <b>x</b></p></rendered-html>"), "Hi x");
        assert_eq!(clean_option_description("a < b"), "a < b");
    }
}
