//! NVF options source.
//!
//! NVF publishes its catalogue under `vim.*`. The NixOS and Home Manager modules
//! wrap it under `programs.nvf.settings.vim.*`, and callers may also use the
//! shorter `programs.nvf.vim.*`; every wrapper normalizes to the canonical
//! `vim.*` form before lookup or prefix browsing.

use crate::App;
use crate::caches::NvfOption;
use crate::text::{casefold, char_len, collapse, commas, ellipsize, error, score_option_match, strip, tally};

const DOCS_TRACK: &str = "unstable";
const CANONICAL_ROOT: &str = "vim";
const SEARCH_DESCRIPTION_LIMIT: usize = 200;
const DEFAULT_FIELD_TEXT_LIMIT: usize = 120;
const INFO_EXAMPLE_LIMIT: usize = 500;
const BROWSE_LIMIT: usize = 100;

/// Longest aliases first, to make the intended precedence explicit.
const ROOT_ALIASES: [&str; 3] = ["config.programs.nvf.settings.vim", "programs.nvf.settings.vim", "programs.nvf.vim"];

macro_rules! options_or_return {
    ($app:expr) => {
        match $app.caches.nvf_options(&$app.http).await {
            Ok(options) => options,
            Err(failure) => return error(failure, "API_ERROR"),
        }
    };
}

/// Normalize a canonical or wrapped NVF option path. Only recognized roots are
/// rewritten; ordinary keywords and wrapper-only options such as
/// `programs.nvf.enable` pass through. Prefix comparison is case-insensitive
/// while the suffix keeps its spelling.
pub fn normalize_path(value: &str) -> String {
    let path = strip(value);
    let lower = path.to_lowercase();
    for alias in ROOT_ALIASES {
        if lower == alias {
            return CANONICAL_ROOT.to_owned();
        }
        if lower.strip_prefix(alias).is_some_and(|rest| rest.starts_with('.')) {
            let suffix: String = path.chars().skip(char_len(alias)).collect();
            return format!("{CANONICAL_ROOT}{suffix}");
        }
    }
    path.to_owned()
}

/// Browse/stats category. Nearly every option begins with `vim`, so the first
/// two components form the category; other names use their first component.
fn category(name: &str) -> String {
    let normalized = normalize_path(name);
    if normalized.is_empty() {
        return String::new();
    }
    let parts: Vec<&str> = normalized.split('.').collect();
    if parts[0].to_lowercase() == CANONICAL_ROOT && parts.len() > 1 {
        return parts[..2].join(".");
    }
    parts[0].to_owned()
}

fn compact(value: &str, limit: usize) -> String {
    ellipsize(&collapse(value), limit)
}

fn categories(options: &[NvfOption]) -> Vec<(String, i64)> {
    let names: Vec<String> = options.iter().map(|option| category(&option.name)).collect();
    let mut counts = tally(names.iter().map(String::as_str).filter(|name| !name.is_empty()));
    counts.sort_by_cached_key(|(name, count)| (std::cmp::Reverse(*count), casefold(name)));
    counts
}

/// Search NVF options by canonical path or description text.
pub async fn search_nvf(app: &App, query: &str, limit: i64) -> String {
    let options = options_or_return!(app);
    let display_query = strip(query);
    let canonical = casefold(&normalize_path(query));
    let mut matches: Vec<(i32, String, &NvfOption)> = if canonical.is_empty() {
        Vec::new()
    } else {
        options
            .iter()
            .filter_map(|option| {
                let score = score_option_match(&option.name, &option.description, &canonical);
                (score != 0).then(|| (score, casefold(&option.name), option))
            })
            .collect()
    };
    matches.sort_by(|a, b| b.0.cmp(&a.0).then_with(|| a.1.cmp(&b.1)));
    matches.truncate(limit as usize);
    if matches.is_empty() {
        return format!("No NVF options found matching '{display_query}'");
    }
    let mut results = vec![format!("Found {} NVF options matching '{display_query}':\n", matches.len())];
    for (_, _, option) in matches {
        results.push(format!("* {}", option.name));
        if !option.r#type.is_empty() {
            results.push(format!("  Type: {}", option.r#type));
        }
        if !option.default.is_empty() {
            results.push(format!("  Default: {}", compact(&option.default, DEFAULT_FIELD_TEXT_LIMIT)));
        }
        if !option.description.is_empty() {
            results.push(format!("  {}", compact(&option.description, SEARCH_DESCRIPTION_LIMIT)));
        }
        results.push(String::new());
    }
    strip(&results.join("\n")).to_owned()
}

/// Format one option for detailed display.
fn format_option(option: &NvfOption) -> String {
    let mut lines = vec![format!("NVF Option: {}", option.name)];
    if !option.r#type.is_empty() {
        lines.push(format!("Type: {}", option.r#type));
    }
    if !option.description.is_empty() {
        lines.push(format!("Description: {}", option.description));
    }
    if !option.default.is_empty() {
        lines.push(format!("Default: {}", option.default));
    }
    if !option.example.is_empty() {
        lines.push(format!("Example: {}", ellipsize(&option.example, INFO_EXAMPLE_LIMIT)));
    }
    match option.declarations.as_slice() {
        [] => {}
        [only] => lines.push(format!("Declared in: {only}")),
        many => {
            lines.push("Declared in:".to_owned());
            lines.extend(many.iter().map(|declaration| format!("* {declaration}")));
        }
    }
    if !option.url.is_empty() {
        lines.push(format!("Documentation: {}", option.url));
    }
    lines.join("\n")
}

/// Get detailed information for an exact NVF option path.
pub async fn info_nvf(app: &App, name: &str) -> String {
    let options = options_or_return!(app);
    let display_name = strip(name);
    let canonical = normalize_path(name);
    if let Some(option) = options.iter().find(|option| option.name == canonical) {
        return format_option(option);
    }
    let canonical_cf = casefold(&canonical);
    if let Some(option) = options.iter().find(|option| casefold(&option.name) == canonical_cf) {
        return format_option(option);
    }
    let mut similar: Vec<(String, &str)> = options
        .iter()
        .filter_map(|option| {
            let folded = casefold(&option.name);
            folded.contains(&canonical_cf).then_some((folded, option.name.as_str()))
        })
        .collect();
    similar.sort();
    if similar.is_empty() {
        return error(format!("NVF option '{display_name}' not found"), "NOT_FOUND");
    }
    let names: Vec<&str> = similar.into_iter().take(5).map(|(_, name)| name).collect();
    error(format!("Option '{display_name}' not found. Similar: {}", names.join(", ")), "NOT_FOUND")
}

/// Summarize the current published NVF option catalogue.
pub async fn stats_nvf(app: &App) -> String {
    let options = options_or_return!(app);
    let counts = categories(&options);
    let mut results = vec![
        "NVF Statistics:".to_owned(),
        format!("* Documentation track: {DOCS_TRACK}"),
        format!("* Total options: {}", commas(options.len() as i64)),
        format!("* Categories: {}", counts.len()),
        "* Top categories:".to_owned(),
    ];
    for (name, count) in counts.into_iter().take(10) {
        results.push(format!("  - {name}: {}", commas(count)));
    }
    results.join("\n")
}

/// List NVF categories or options below a canonical/wrapped prefix.
pub async fn browse_nvf(app: &App, prefix: &str) -> String {
    let options = options_or_return!(app);
    let normalized = normalize_path(prefix);
    let canonical = normalized.trim_end_matches('.');
    if canonical.is_empty() {
        let counts = categories(&options);
        let mut results = vec![format!("NVF option categories ({} total):\n", counts.len())];
        for (name, count) in counts {
            let noun = if count == 1 { "option" } else { "options" };
            results.push(format!("* {name} ({} {noun})", commas(count)));
        }
        return results.join("\n");
    }
    let prefix_cf = casefold(canonical);
    let prefix_dot = format!("{prefix_cf}.");
    let mut matches: Vec<(String, &NvfOption)> = options
        .iter()
        .filter_map(|option| {
            let folded = casefold(&option.name);
            (folded == prefix_cf || folded.starts_with(&prefix_dot)).then_some((folded, option))
        })
        .collect();
    if matches.is_empty() {
        return format!("No NVF options found with prefix '{canonical}'");
    }
    matches.sort_by(|a, b| a.0.cmp(&b.0));
    let mut results =
        vec![format!("NVF options with prefix '{canonical}' ({} found):\n", commas(matches.len() as i64))];
    for (_, option) in matches.iter().take(BROWSE_LIMIT) {
        results.push(format!("* {}", option.name));
        if !option.r#type.is_empty() {
            results.push(format!("  Type: {}", option.r#type));
        }
        if !option.description.is_empty() {
            results.push(format!("  {}", compact(&option.description, SEARCH_DESCRIPTION_LIMIT)));
        }
        results.push(String::new());
    }
    if matches.len() > BROWSE_LIMIT {
        results.push(format!("... and {} more options", commas((matches.len() - BROWSE_LIMIT) as i64)));
    }
    strip(&results.join("\n")).to_owned()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn wrapped_paths_normalize() {
        assert_eq!(normalize_path("programs.nvf.settings.vim.lsp.enable"), "vim.lsp.enable");
        assert_eq!(normalize_path("config.programs.nvf.settings.vim"), "vim");
        assert_eq!(normalize_path("  PROGRAMS.nvf.vim.Theme "), "vim.Theme");
        assert_eq!(normalize_path("programs.nvf.enable"), "programs.nvf.enable");
        assert_eq!(normalize_path("lsp"), "lsp");
    }

    #[test]
    fn categories_use_two_components_under_vim() {
        assert_eq!(category("vim.languages.nix.enable"), "vim.languages");
        assert_eq!(category("vim"), "vim");
        assert_eq!(category("other.thing"), "other");
    }
}
