//! Functionality shared across data sources.

use std::time::{Duration, Instant};

use reqwest::header::{ACCEPT, USER_AGENT};
use serde_json::{Value, json};

use crate::App;
use crate::caches::{Channels, HtmlSource};
use crate::config::{GITHUB_API, NIXOS_API};
use crate::html::HtmlOption;
use crate::http::FetchError;
use crate::text::{by_count, by_count_then_key, casefold, commas, err, error, score_option_match, str_of, tally};

/// Per-process lifetime of a GitHub branch HEAD lookup, so a long-running server
/// does not keep reporting a stale HEAD after upstream advances.
const BRANCH_REV_TTL: Duration = Duration::from_secs(600);

/// Maximum options listed by a prefix browse before truncating.
const BROWSE_DISPLAY_LIMIT: usize = 100;

pub async fn channel_suggestions(channels: &Channels, invalid: &str) -> String {
    let available: Vec<&str> = channels.keys().map(String::as_str).collect();
    let invalid_lower = invalid.to_lowercase();
    let mut suggestions: Vec<&str> = available
        .iter()
        .copied()
        .filter(|channel| {
            let channel = channel.to_lowercase();
            channel.contains(&invalid_lower) || invalid_lower.contains(&channel)
        })
        .collect();
    if suggestions.is_empty() {
        let is_version = |ch: &&&str| ch.contains('.') && ch.replace('.', "").chars().all(|c| c.is_ascii_digit());
        let mut common = vec!["unstable", "stable", "beta"];
        common.extend(available.iter().filter(is_version).copied().take(2));
        suggestions = common.into_iter().filter(|ch| available.contains(ch)).collect();
        if suggestions.is_empty() {
            suggestions = available.into_iter().take(4).collect();
        }
    }
    format!("Available channels: {}", suggestions.join(", "))
}

/// Run a search against the NixOS Elasticsearch index. `rescore` is a top-level
/// search-body key, used to re-rank the top window after the main query.
pub async fn es_query(
    app: &App,
    index: &str,
    query: Value,
    size: i64,
    rescore: Option<Value>,
) -> Result<Vec<Value>, String> {
    let mut body = json!({"query": query, "size": size});
    if let Some(rescore) = rescore {
        body["rescore"] = rescore;
    }
    let request = app.http.post_es(&format!("{NIXOS_API}/{index}/_search"), &body);
    let describe = |failure: FetchError| format!("API error: {failure}");
    let data = app.http.send(request, 10).await.and_then(|resp| resp.ok()?.json()).map_err(describe)?;
    Ok(data.pointer("/hits/hits").and_then(Value::as_array).cloned().unwrap_or_default())
}

fn is_release(name: &str) -> bool {
    let digits = |text: &str| !text.is_empty() && text.chars().all(|c| c.is_ascii_digit());
    name.split_once('.').is_some_and(|(major, minor)| digits(major) && digits(minor))
}

/// Map a channel name to its nixpkgs branch for commit lookups.
fn channel_to_branch(name: &str, index: &str) -> String {
    if name.contains("unstable") || index.contains("unstable") {
        return "nixos-unstable".to_owned();
    }
    if name == "stable" || name == "beta" {
        // Resolve to the release version indirectly via the ES index name.
        return match index.split('-').nth(3) {
            Some(version) if is_release(version) => format!("nixos-{version}"),
            _ => String::new(),
        };
    }
    if is_release(name) { format!("nixos-{name}") } else { String::new() }
}

/// The commit embedded in an unstable ES index name, if any.
fn embedded_commit(index: &str) -> Option<&str> {
    let (_, commit) = index.rsplit_once('-')?;
    (commit.len() == 40 && commit.bytes().all(|b| matches!(b, b'0'..=b'9' | b'a'..=b'f'))).then_some(commit)
}

#[derive(Debug, PartialEq, Eq)]
enum Revision {
    /// Embedded in the ES index name: the data was built from exactly this commit.
    Indexed(String),
    /// Best-effort branch HEAD: a pointer to the channel, not a claim about the
    /// indexed data.
    BranchHead(String),
    Unknown,
}

async fn channel_revision(app: &App, name: &str, index: &str) -> Revision {
    if let Some(commit) = embedded_commit(index) {
        return Revision::Indexed(commit.to_owned());
    }
    let branch = channel_to_branch(name, index);
    if branch.is_empty() {
        return Revision::Unknown;
    }
    let cached = app.caches.branch_revs.lock().await.get(&branch).cloned();
    if let Some((rev, at)) = &cached
        && at.elapsed() < BRANCH_REV_TTL
    {
        return Revision::BranchHead(rev.clone());
    }

    let request = app
        .http
        .get(&format!("{GITHUB_API}/repos/NixOS/nixpkgs/commits/{branch}"))
        .header(ACCEPT, "application/vnd.github+json")
        .header(USER_AGENT, crate::config::USER_AGENT);
    if let Ok(resp) = app.http.send(request, 4).await {
        let data = if resp.status() == 200 { resp.json().ok() } else { None };
        let rev = data.as_ref().map_or("", |data| str_of(data, "sha"));
        if !rev.is_empty() {
            app.caches.branch_revs.lock().await.insert(branch, (rev.to_owned(), Instant::now()));
            return Revision::BranchHead(rev.to_owned());
        }
    }
    // Better to surface last-known HEAD than nothing.
    cached.map_or(Revision::Unknown, |(rev, _)| Revision::BranchHead(rev))
}

/// List available NixOS channels with status, document counts, and HEAD commit.
pub async fn list_channels(app: &App) -> String {
    let channels = &app.caches.channels;
    let configured = channels.resolved(&app.http).await;
    let available = channels.available(&app.http).await;
    let mut results = Vec::new();
    if channels.using_fallback().await {
        results.push("WARNING: Using fallback channels (API discovery failed)\n".to_owned());
    }
    results.push("NixOS Channels:\n".to_owned());

    let mut names: Vec<_> = configured.iter().collect();
    names.sort();
    for (name, index) in names {
        let doc_count = available.get(index);
        let status = if doc_count.is_some() { "Available" } else { "Unavailable" };
        let mut label = format!("* {name}");
        if name == "stable"
            && let Some(version) = index.split('-').nth(3)
        {
            label = format!("* {name} (current: {version})");
        }
        results.push(format!("{label} -> {index}"));
        results.push(format!("  Status: {status} ({})", doc_count.map_or("Unknown", String::as_str)));
        let revision = channel_revision(app, name, index).await;
        let branch = channel_to_branch(name, index);
        if !branch.is_empty() {
            results.push(format!("  Branch: {branch}"));
        }
        match revision {
            Revision::Indexed(rev) => results.push(format!("  Revision (indexed): {rev}")),
            Revision::BranchHead(rev) => {
                results.push(format!("  Branch HEAD: {rev} (upstream; may be ahead of indexed data)"));
            }
            Revision::Unknown => {}
        }
        results.push(String::new());
    }
    results.push(
        "Note: 'stable' always points to current stable release. \
         'Revision (indexed)' is the exact commit the search index was built from \
         (safe to compare against `nix_versions`). 'Branch HEAD' is the upstream \
         branch tip, fetched best-effort from GitHub and cached for up to \
         10 minutes per process — it may be a few commits ahead of the \
         indexed data or a few minutes stale from the upstream ref."
            .to_owned(),
    );
    crate::text::strip(&results.join("\n")).to_owned()
}

/// Search a cached HTML option catalogue, ranked by match quality.
pub async fn search_html_options(app: &App, source: HtmlSource, query: &str, limit: i64) -> String {
    let options = match app.caches.html_options(&app.http, source).await {
        Ok(options) => options,
        Err(failure) => return err(failure),
    };
    let display_name = source.display_name();
    let mut matches: Vec<(i32, String, &HtmlOption)> = options
        .iter()
        .filter_map(|opt| {
            let score = score_option_match(&opt.name, &opt.description, query);
            (score != 0).then(|| (score, casefold(&opt.name), opt))
        })
        .collect();
    matches.sort_by(|a, b| b.0.cmp(&a.0).then_with(|| a.1.cmp(&b.1)));
    matches.truncate(limit as usize);
    if matches.is_empty() {
        return format!("No {display_name} options found matching '{query}'");
    }
    let mut results = vec![format!("Found {} {display_name} options matching '{query}':\n", matches.len())];
    for (_, _, opt) in matches {
        results.push(format!("* {}", opt.name));
        if !opt.r#type.is_empty() {
            results.push(format!("  Type: {}", opt.r#type));
        }
        if !opt.description.is_empty() {
            results.push(format!("  {}", opt.description));
        }
        results.push(String::new());
    }
    crate::text::strip(&results.join("\n")).to_owned()
}

/// Get detailed info for an option in a cached HTML catalogue.
pub async fn info_html_options(app: &App, source: HtmlSource, name: &str) -> String {
    let options = match app.caches.html_options(&app.http, source).await {
        Ok(options) => options,
        Err(failure) => return err(failure),
    };
    if let Some(opt) = options.iter().find(|opt| opt.name == name) {
        let mut info = vec![format!("Option: {name}")];
        if !opt.r#type.is_empty() {
            info.push(format!("Type: {}", opt.r#type));
        }
        if !opt.description.is_empty() {
            info.push(format!("Description: {}", opt.description));
        }
        return info.join("\n");
    }
    let name_cf = casefold(name);
    let mut suggestions: Vec<(String, &str)> = options
        .iter()
        .filter_map(|opt| {
            let folded = casefold(&opt.name);
            folded.contains(&name_cf).then_some((folded, opt.name.as_str()))
        })
        .collect();
    suggestions.sort();
    let similar: Vec<&str> = suggestions.into_iter().take(5).map(|(_, name)| name).collect();
    if similar.is_empty() {
        return error(format!("Option '{name}' not found"), "NOT_FOUND");
    }
    error(format!("Option '{name}' not found. Similar: {}", similar.join(", ")), "NOT_FOUND")
}

/// Top-level category of a dotted option path, or `None` for options that are
/// not part of a category (dot-less names, or attribute names that are not
/// plain lowercase identifiers). Shared by stats and browse.
fn option_category(name: &str) -> Option<&str> {
    let (cat, _) = name.split_once('.')?;
    let mut chars = cat.chars();
    let valid_start = chars.next().is_some_and(|c| c.is_alphabetic() || c == '_');
    let identifier = valid_start && chars.all(|c| c.is_alphanumeric() || c == '_');
    let lower = cat.chars().any(char::is_lowercase) && !cat.chars().any(char::is_uppercase);
    (cat.chars().count() > 1 && identifier && lower).then_some(cat)
}

fn count_categories(options: &[HtmlOption]) -> Vec<(String, i64)> {
    tally(options.iter().filter_map(|opt| option_category(&opt.name)))
}

/// Get option counts and top categories for a cached HTML catalogue.
pub async fn stats_html_options(app: &App, source: HtmlSource) -> String {
    let options = match app.caches.html_options(&app.http, source).await {
        Ok(options) => options,
        Err(failure) => return err(failure),
    };
    let categories = count_categories(&options);
    let mut result = vec![
        format!("{} Statistics:", source.display_name()),
        format!("* Total options: {}", commas(options.len() as i64)),
        format!("* Categories: {}", categories.len()),
        "* Top categories:".to_owned(),
    ];
    for (cat, count) in by_count(categories).into_iter().take(5) {
        result.push(format!("  - {cat}: {}", commas(count)));
    }
    result.join("\n")
}

/// Browse Home Manager or nix-darwin options by prefix, or list categories.
pub async fn browse_options(app: &App, source: HtmlSource, prefix: &str) -> String {
    let options = match app.caches.html_options(&app.http, source).await {
        Ok(options) => options,
        Err(failure) => return err(failure),
    };
    let source_name = source.display_name();
    if prefix.is_empty() {
        let categories = count_categories(&options);
        let mut results = vec![format!("{source_name} categories ({} total):\n", categories.len())];
        for (cat, count) in by_count_then_key(categories) {
            results.push(format!("* {cat} ({count} options)"));
        }
        return results.join("\n");
    }
    let dotted = format!("{prefix}.");
    let mut matches: Vec<&HtmlOption> =
        options.iter().filter(|opt| opt.name == prefix || opt.name.starts_with(&dotted)).collect();
    if matches.is_empty() {
        return format!("No {source_name} options found with prefix '{prefix}'");
    }
    let mut results =
        vec![format!("{source_name} options with prefix '{prefix}' ({} found):\n", commas(matches.len() as i64))];
    matches.sort_by(|a, b| a.name.cmp(&b.name));
    for opt in matches.iter().take(BROWSE_DISPLAY_LIMIT) {
        results.push(format!("* {}", opt.name));
        if !opt.description.is_empty() {
            results.push(format!("  {}", opt.description));
        }
        results.push(String::new());
    }
    if matches.len() > BROWSE_DISPLAY_LIMIT {
        results.push(format!("... and {} more options", commas((matches.len() - BROWSE_DISPLAY_LIMIT) as i64)));
    }
    crate::text::strip(&results.join("\n")).to_owned()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn branches_follow_channels() {
        assert_eq!(channel_to_branch("unstable", "latest-1-nixos-unstable"), "nixos-unstable");
        assert_eq!(channel_to_branch("stable", "latest-50-nixos-26.05"), "nixos-26.05");
        assert_eq!(channel_to_branch("25.11", "latest-48-nixos-25.11"), "nixos-25.11");
        assert_eq!(channel_to_branch("stable", "weird"), "");
    }

    #[test]
    fn embedded_commit_requires_40_hex() {
        let commit = "b12141ef619e0a9c1c84dc8c684040326f27cdcc";
        assert_eq!(embedded_commit(&format!("nixos-46-unstable-{commit}")), Some(commit));
        assert_eq!(embedded_commit("latest-46-nixos-unstable"), None);
    }

    #[test]
    fn categories_require_lowercase_identifiers() {
        assert_eq!(option_category("programs.git.enable"), Some("programs"));
        assert_eq!(option_category("uninstall"), None);
        assert_eq!(option_category("Foo.bar"), None);
        assert_eq!(option_category("x.bar"), None);
        assert_eq!(option_category("<name>.bar"), None);
    }
}
