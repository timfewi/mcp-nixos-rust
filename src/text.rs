//! Plain-text formatting helpers shared by every source.
//!
//! Tool output is a compatibility surface, so these keep the semantics callers
//! already depend on: lengths count characters, whitespace is Unicode-aware,
//! and case-insensitive ordering uses full case folding.

use chrono::{DateTime, NaiveDate, NaiveDateTime};
use serde_json::Value;

pub fn error(msg: impl std::fmt::Display, code: &str) -> String {
    format!("Error ({code}): {msg}")
}

/// `error` with the default `ERROR` code.
pub fn err(msg: impl std::fmt::Display) -> String {
    error(msg, "ERROR")
}

pub fn casefold(value: &str) -> String {
    caseless::default_case_fold_str(value)
}

fn is_space(c: char) -> bool {
    c.is_whitespace() || ('\x1c'..='\x1f').contains(&c)
}

/// Compile a static pattern.
pub fn regex(pattern: &str) -> regex::Regex {
    #[allow(clippy::expect_used)]
    regex::Regex::new(pattern).expect("static regex is valid")
}

pub fn strip(value: &str) -> &str {
    value.trim_matches(is_space)
}

pub fn words(value: &str) -> impl Iterator<Item = &str> {
    value.split(is_space).filter(|word| !word.is_empty())
}

pub fn collapse(value: &str) -> String {
    words(value).collect::<Vec<_>>().join(" ")
}

pub fn char_len(value: &str) -> usize {
    value.chars().count()
}

pub fn first_chars(value: &str, limit: usize) -> &str {
    value.char_indices().nth(limit).map_or(value, |(end, _)| &value[..end])
}

/// `value[:limit] + "..."` when longer than `limit` characters.
pub fn ellipsize(value: &str, limit: usize) -> String {
    let head = first_chars(value, limit);
    if head.len() < value.len() { format!("{head}...") } else { value.to_owned() }
}

pub fn commas(value: i64) -> String {
    let digits = value.unsigned_abs().to_string();
    let mut out = String::with_capacity(digits.len() + digits.len() / 3 + 1);
    if value < 0 {
        out.push('-');
    }
    for (index, digit) in digits.chars().enumerate() {
        if index > 0 && (digits.len() - index).is_multiple_of(3) {
            out.push(',');
        }
        out.push(digit);
    }
    out
}

pub fn format_size(size: u64) -> String {
    const KB: f64 = 1024.0;
    let bytes = size as f64;
    match size {
        0..1024 => format!("{size} B"),
        1024..1_048_576 => format!("{:.1} KB", bytes / KB),
        1_048_576..1_073_741_824 => format!("{:.1} MB", bytes / (KB * KB)),
        _ => format!("{:.1} GB", bytes / (KB * KB * KB)),
    }
}

/// Python `repr()` of a string, as echoed by path error messages.
pub fn py_repr(value: &str) -> String {
    let quote = if value.contains('\'') && !value.contains('"') { '"' } else { '\'' };
    let mut out = String::from(quote);
    for c in value.chars() {
        match c {
            '\\' => out.push_str("\\\\"),
            '\n' => out.push_str("\\n"),
            '\r' => out.push_str("\\r"),
            '\t' => out.push_str("\\t"),
            c if c == quote => {
                out.push('\\');
                out.push(c);
            }
            c if (c as u32) < 0x20 || c as u32 == 0x7f => out.push_str(&format!("\\x{:02x}", c as u32)),
            c => out.push(c),
        }
    }
    out.push(quote);
    out
}

/// Field as `&str`; absent, null and non-string values read as empty.
pub fn str_of<'a>(value: &'a Value, key: &str) -> &'a str {
    value.get(key).and_then(Value::as_str).unwrap_or("")
}

/// `str(value)` for JSON scalars that are interpolated unconditionally.
pub fn display(value: &Value) -> String {
    match value {
        Value::String(text) => text.clone(),
        Value::Null => "None".to_owned(),
        Value::Bool(true) => "True".to_owned(),
        Value::Bool(false) => "False".to_owned(),
        other => other.to_string(),
    }
}

/// JSON truthiness: null, false, 0, "" and empty containers are false.
pub fn truthy(value: &Value) -> bool {
    match value {
        Value::Null => false,
        Value::Bool(flag) => *flag,
        Value::Number(number) => number.as_f64() != Some(0.0),
        Value::String(text) => !text.is_empty(),
        Value::Array(items) => !items.is_empty(),
        Value::Object(map) => !map.is_empty(),
    }
}

pub fn strings(value: Option<&Value>) -> Vec<String> {
    value.and_then(Value::as_array).map_or_else(Vec::new, |items| items.iter().map(display).collect())
}

/// `datetime.fromisoformat` for the timestamps upstream APIs emit. Offsets are
/// kept, not converted: the printed date is the one the upstream wrote.
pub fn parse_iso(value: &str) -> Option<NaiveDateTime> {
    let normalized = value.replace('Z', "+00:00");
    DateTime::parse_from_rfc3339(&normalized)
        .map(|parsed| parsed.naive_local())
        .ok()
        .or_else(|| NaiveDateTime::parse_from_str(&normalized, "%Y-%m-%dT%H:%M:%S%.f").ok())
        .or_else(|| NaiveDateTime::parse_from_str(&normalized, "%Y-%m-%d %H:%M:%S%.f").ok())
        .or_else(|| NaiveDate::parse_from_str(&normalized, "%Y-%m-%d").ok().and_then(|day| day.and_hms_opt(0, 0, 0)))
}

/// Score how well an option matches a query, prioritizing path matches.
///
/// Tiers: exact path (100), leading path prefix (80), complete dot-separated
/// path segment(s) (70 minus the segment position, floored at 62), name
/// substring (60), description substring (20). The segment tier ranks
/// `programs.git.enable` above `programs.bun.enableGitIntegration` for the
/// query `git`, and the position penalty ranks it above deeper segment matches
/// such as `programs.difftastic.git.enable`.
pub fn score_option_match(name: &str, description: &str, query: &str) -> i32 {
    let query = casefold(query);
    if query.is_empty() {
        return 0;
    }
    let name = casefold(name);
    if name == query {
        return 100;
    }
    if name.strip_prefix(query.as_str()).is_some_and(|rest| rest.starts_with('.')) {
        return 80;
    }
    let name_parts: Vec<&str> = name.split('.').collect();
    let query_parts: Vec<&str> = query.split('.').collect();
    if let Some(start) = name_parts.windows(query_parts.len()).position(|window| window == query_parts) {
        return 70 - start.min(8) as i32;
    }
    if name.contains(&query) {
        return 60;
    }
    if casefold(description).contains(&query) {
        return 20;
    }
    0
}

/// Count occurrences, keeping first-seen order for stable tie-breaking.
pub fn tally<'a>(items: impl Iterator<Item = &'a str>) -> Vec<(String, i64)> {
    let mut counts = indexmap::IndexMap::<String, i64>::new();
    for item in items {
        *counts.entry(item.to_owned()).or_insert(0) += 1;
    }
    counts.into_iter().collect()
}

/// Sort descending by count; equal counts keep their existing order.
pub fn by_count(mut counts: Vec<(String, i64)>) -> Vec<(String, i64)> {
    counts.sort_by_key(|entry| std::cmp::Reverse(entry.1));
    counts
}

/// Sort descending by count, then ascending by key.
pub fn by_count_then_key(mut counts: Vec<(String, i64)>) -> Vec<(String, i64)> {
    counts.sort_by(|a, b| b.1.cmp(&a.1).then_with(|| a.0.cmp(&b.0)));
    counts
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn commas_group_thousands() {
        assert_eq!(commas(0), "0");
        assert_eq!(commas(999), "999");
        assert_eq!(commas(1000), "1,000");
        assert_eq!(commas(1234567), "1,234,567");
    }

    #[test]
    fn format_size_tiers() {
        assert_eq!(format_size(512), "512 B");
        assert_eq!(format_size(2048), "2.0 KB");
        assert_eq!(format_size(5 * 1024 * 1024), "5.0 MB");
        assert_eq!(format_size(3 * 1024 * 1024 * 1024), "3.0 GB");
    }

    #[test]
    fn ellipsize_counts_characters() {
        assert_eq!(ellipsize("äöü", 3), "äöü");
        assert_eq!(ellipsize("äöüx", 3), "äöü...");
    }

    #[test]
    fn py_repr_matches_python_quoting() {
        assert_eq!(py_repr("abc"), "'abc'");
        assert_eq!(py_repr("it's"), "\"it's\"");
        assert_eq!(py_repr("a'b\"c\n"), "'a\\'b\"c\\n'");
    }

    #[test]
    fn score_tiers() {
        assert_eq!(score_option_match("programs.git.enable", "", "programs.git.enable"), 100);
        assert_eq!(score_option_match("programs.git.enable", "", "programs"), 80);
        assert_eq!(score_option_match("programs.git.enable", "", "git"), 69);
        assert_eq!(score_option_match("programs.difftastic.git.enable", "", "git"), 68);
        assert_eq!(score_option_match("programs.bun.enableGitIntegration", "", "git"), 60);
        assert_eq!(score_option_match("programs.foo", "uses git", "git"), 20);
        assert_eq!(score_option_match("programs.foo", "", "git"), 0);
        assert_eq!(score_option_match("programs.foo", "", ""), 0);
    }

    #[test]
    fn parse_iso_keeps_written_date() {
        assert_eq!(parse_iso("2026-01-02T23:30:00Z").map(|d| d.to_string()).as_deref(), Some("2026-01-02 23:30:00"));
        assert_eq!(parse_iso("2026-01-02").map(|d| d.date().to_string()).as_deref(), Some("2026-01-02"));
        assert!(parse_iso("not a date").is_none());
    }
}
