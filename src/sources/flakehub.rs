//! FlakeHub API source (Determinate Systems registry).

use reqwest::header::ACCEPT;
use serde_json::Value;

use crate::App;
use crate::config::FLAKEHUB_API;
use crate::http::{FetchError, Response};
use crate::text::{
    by_count, collapse, commas, display, ellipsize, err, error, parse_iso, str_of, strings, tally, truthy,
};

fn describe(failure: &FetchError) -> String {
    match failure {
        FetchError::Timeout => error("FlakeHub API timed out", "TIMEOUT"),
        FetchError::Parse(msg) => err(msg),
        other => error(format!("FlakeHub API error: {other}"), "API_ERROR"),
    }
}

async fn fetch(app: &App, url: &str, query: &[(&str, &str)]) -> Result<Response, FetchError> {
    app.http.send(app.http.get(url).query(query).header(ACCEPT, "application/json"), 15).await
}

/// Search FlakeHub flakes by name or description.
pub async fn search_flakehub(app: &App, query: &str, limit: i64) -> String {
    let url = format!("{FLAKEHUB_API}/search");
    let flakes = match fetch(app, &url, &[("q", query)]).await.and_then(|resp| resp.ok()?.json()) {
        Ok(flakes) => flakes,
        Err(failure) => return describe(&failure),
    };
    let flakes = flakes.as_array().map_or(&[][..], Vec::as_slice);
    if flakes.is_empty() {
        return format!("No flakes found on FlakeHub matching '{query}'");
    }
    let flakes = &flakes[..flakes.len().min(limit as usize)];
    let mut results = vec![format!("Found {} flakes on FlakeHub matching '{query}':\n", flakes.len())];
    for flake in flakes {
        let org = str_of(flake, "org");
        let project = str_of(flake, "project");
        results.push(format!("* {org}/{project}"));
        let desc = str_of(flake, "description");
        if !desc.is_empty() {
            results.push(format!("  {}", ellipsize(&collapse(desc), 200)));
        }
        let labels = strings(flake.get("labels"));
        if !labels.is_empty() {
            results.push(format!("  Labels: {}", labels[..labels.len().min(5)].join(", ")));
        }
        results.push(format!("  https://flakehub.com/flake/{org}/{project}"));
        results.push(String::new());
    }
    crate::text::strip(&results.join("\n")).to_owned()
}

/// Get detailed info for a FlakeHub flake (org/project format).
pub async fn info_flakehub(app: &App, name: &str) -> String {
    let Some((org, project)) = name.split_once('/') else {
        return err("FlakeHub flake name must be in 'org/project' format (e.g., 'NixOS/nixpkgs')");
    };
    let url = format!("{FLAKEHUB_API}/version/{org}/{project}/*");
    let not_found = || error(format!("Flake '{name}' not found on FlakeHub"), "NOT_FOUND");
    let version = match fetch(app, &url, &[]).await.and_then(|resp| resp.ok()?.json()) {
        Ok(version) => version,
        Err(FetchError::Status(404, _)) => return not_found(),
        Err(failure) => return describe(&failure),
    };

    let first_of = |keys: [&str; 2]| keys.iter().map(|key| str_of(&version, key)).find(|text| !text.is_empty());
    let mut results = vec![format!("FlakeHub Flake: {org}/{project}")];
    let mut push = |label: &str, value: &str| {
        if !value.is_empty() {
            results.push(format!("{label}: {value}"));
        }
    };
    push("Description", str_of(&version, "description"));
    push("Latest Version", first_of(["simplified_version", "version"]).unwrap_or(""));
    push("Revision", str_of(&version, "revision"));
    if let Some(count) = version.get("commit_count").filter(|count| truthy(count)) {
        push("Commits", &count.as_i64().map_or_else(|| display(count), commas));
    }
    push("Visibility", str_of(&version, "visibility"));
    if let Some(published) = parse_iso(str_of(&version, "published_at")) {
        push("Published", &published.format("%Y-%m-%d %H:%M UTC").to_string());
    }
    if version.get("mirrored").is_some_and(truthy) {
        push("Source", "Mirrored from GitHub");
    }
    push("Download", first_of(["pretty_download_url", "download_url"]).unwrap_or(""));
    results.push(format!("FlakeHub URL: https://flakehub.com/flake/{org}/{project}"));
    results.join("\n")
}

/// Get FlakeHub statistics.
pub async fn stats_flakehub(app: &App) -> String {
    let url = format!("{FLAKEHUB_API}/flakes");
    let flakes = match fetch(app, &url, &[]).await.and_then(|resp| resp.ok()?.json()) {
        Ok(flakes) => flakes,
        Err(failure) => return describe(&failure),
    };
    let flakes = flakes.as_array().map_or(&[][..], Vec::as_slice);
    let orgs = tally(flakes.iter().map(|flake| match flake.get("org") {
        Some(Value::String(org)) => org.as_str(),
        _ => "unknown",
    }));
    let all_labels: Vec<String> = flakes.iter().flat_map(|flake| strings(flake.get("labels"))).collect();
    let labels = tally(all_labels.iter().map(String::as_str));

    let mut results = vec![
        "FlakeHub Statistics:".to_owned(),
        format!("* Total flakes: {}", commas(flakes.len() as i64)),
        format!("* Organizations: {}", commas(orgs.len() as i64)),
        "* Top organizations:".to_owned(),
    ];
    for (org, count) in by_count(orgs).into_iter().take(5) {
        results.push(format!("  - {org}: {} flakes", commas(count)));
    }
    if !labels.is_empty() {
        results.push("* Top labels:".to_owned());
        for (label, count) in by_count(labels).into_iter().take(5) {
            results.push(format!("  - {label}: {} flakes", commas(count)));
        }
    }
    results.push("\nFlakeHub URL: https://flakehub.com/".to_owned());
    results.join("\n")
}
