//! Nixvim options source.

use crate::App;
use crate::caches::NixvimOption;
use crate::text::{by_count, by_count_then_key, commas, ellipsize, error, tally};

macro_rules! options_or_return {
    ($app:expr) => {
        match $app.caches.nixvim_options(&$app.http).await {
            Ok(options) => options,
            Err(failure) => return error(failure, "API_ERROR"),
        }
    };
}

/// Search Nixvim options by name or description.
pub async fn search_nixvim(app: &App, query: &str, limit: i64) -> String {
    let options = options_or_return!(app);
    let query_lower = query.to_lowercase();
    let matches: Vec<&NixvimOption> = options
        .iter()
        .filter(|opt| {
            opt.name.to_lowercase().contains(&query_lower) || opt.description.to_lowercase().contains(&query_lower)
        })
        .take(limit as usize)
        .collect();
    if matches.is_empty() {
        return format!("No Nixvim options found matching '{query}'");
    }
    let mut results = vec![format!("Found {} Nixvim options matching '{query}':\n", matches.len())];
    for opt in matches {
        results.push(format!("* {}", opt.name));
        if !opt.r#type.is_empty() {
            results.push(format!("  Type: {}", opt.r#type));
        }
        if !opt.description.is_empty() {
            results.push(format!("  {}", ellipsize(&opt.description, 200)));
        }
        results.push(String::new());
    }
    crate::text::strip(&results.join("\n")).to_owned()
}

/// Format a Nixvim option for detailed display.
fn format_option(opt: &NixvimOption) -> String {
    let mut lines = vec![format!("Nixvim Option: {}", opt.name)];
    if !opt.r#type.is_empty() {
        lines.push(format!("Type: {}", opt.r#type));
    }
    if !opt.description.is_empty() {
        lines.push(format!("Description: {}", opt.description));
    }
    if !opt.default.is_empty() {
        lines.push(format!("Default: {}", opt.default));
    }
    if !opt.example.is_empty() {
        lines.push(format!("Example: {}", ellipsize(&opt.example, 500)));
    }
    if !opt.declaration.is_empty() {
        lines.push(format!("Declared in: {}", opt.declaration));
    }
    lines.join("\n")
}

/// Get detailed info for a Nixvim option.
pub async fn info_nixvim(app: &App, name: &str) -> String {
    let options = options_or_return!(app);
    if let Some(opt) = options.iter().find(|opt| opt.name == name) {
        return format_option(opt);
    }
    let name_lower = name.to_lowercase();
    if let Some(opt) = options.iter().find(|opt| opt.name.to_lowercase() == name_lower) {
        return format_option(opt);
    }
    let similar: Vec<&str> = options
        .iter()
        .filter(|opt| opt.name.to_lowercase().contains(&name_lower))
        .take(5)
        .map(|opt| opt.name.as_str())
        .collect();
    if similar.is_empty() {
        return error(format!("Nixvim option '{name}' not found"), "NOT_FOUND");
    }
    error(format!("Option '{name}' not found. Similar: {}", similar.join(", ")), "NOT_FOUND")
}

fn categories(options: &[NixvimOption]) -> Vec<(String, i64)> {
    tally(options.iter().map(|opt| opt.name.split('.').next().unwrap_or("")))
}

/// Get Nixvim option statistics.
pub async fn stats_nixvim(app: &App) -> String {
    let options = options_or_return!(app);
    let counts = categories(&options);
    let mut result = vec![
        "Nixvim Statistics:".to_owned(),
        format!("* Total options: {}", commas(options.len() as i64)),
        format!("* Categories: {}", counts.len()),
        "* Top categories:".to_owned(),
    ];
    for (cat, count) in by_count(counts).into_iter().take(5) {
        result.push(format!("  - {cat}: {}", commas(count)));
    }
    result.join("\n")
}

/// Browse Nixvim options by prefix, or list categories if no prefix.
pub async fn browse_nixvim(app: &App, prefix: &str) -> String {
    let options = options_or_return!(app);
    if prefix.is_empty() {
        let counts = categories(&options);
        let mut results = vec![format!("Nixvim option categories ({} total):\n", counts.len())];
        for (cat, count) in by_count_then_key(counts) {
            results.push(format!("* {cat} ({count} options)"));
        }
        return results.join("\n");
    }
    let prefix_dot = if prefix.ends_with('.') { prefix.to_owned() } else { format!("{prefix}.") };
    let mut matches: Vec<&NixvimOption> =
        options.iter().filter(|opt| opt.name.starts_with(&prefix_dot) || opt.name == prefix).collect();
    if matches.is_empty() {
        return format!("No Nixvim options found with prefix '{prefix}'");
    }
    matches.sort_by(|a, b| a.name.cmp(&b.name));
    let mut results = vec![format!("Nixvim options with prefix '{prefix}' ({} found):\n", matches.len())];
    for opt in matches.iter().take(100) {
        results.push(format!("* {}", opt.name));
        if !opt.r#type.is_empty() {
            results.push(format!("  Type: {}", opt.r#type));
        }
        if !opt.description.is_empty() {
            results.push(format!("  {}", ellipsize(&opt.description, 150)));
        }
        results.push(String::new());
    }
    if matches.len() > 100 {
        results.push(format!("... and {} more options", matches.len() - 100));
    }
    crate::text::strip(&results.join("\n")).to_owned()
}
