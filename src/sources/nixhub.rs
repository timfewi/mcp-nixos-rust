//! NixHub API source (binary cache, package metadata).

use std::collections::BTreeMap;

use chrono::DateTime;
use futures::future::join_all;
use reqwest::header::ACCEPT;
use serde_json::Value;

use crate::App;
use crate::config::{CACHE_NIXOS_ORG, NIXHUB_API};
use crate::http::{FetchError, Response};
use crate::text::{display, ellipsize, err, error, first_chars, format_size, parse_iso, str_of, strings, truthy};

/// Parsed narinfo key fields.
#[derive(Debug, Default, PartialEq, Eq)]
pub struct NarInfo {
    pub file_size: Option<u64>,
    pub nar_size: Option<u64>,
    pub compression: Option<String>,
}

pub fn parse_narinfo(text: &str) -> NarInfo {
    let mut result = NarInfo::default();
    for line in text.split('\n') {
        let Some((key, value)) = line.split_once(':') else {
            continue;
        };
        let value = value.trim();
        match key.trim().to_lowercase().as_str() {
            "filesize" => result.file_size = value.parse().ok().or(result.file_size),
            "narsize" => result.nar_size = value.parse().ok().or(result.nar_size),
            "compression" => result.compression = Some(value.to_owned()),
            _ => {}
        }
    }
    result
}

async fn fetch(app: &App, path: &str, params: &[(&str, &str)], timeout: u64) -> Result<Response, FetchError> {
    let request = app.http.get(&format!("{NIXHUB_API}{path}")).query(params).header(ACCEPT, "application/json");
    app.http.send(request, timeout).await
}

/// Map a NixHub response to its JSON or a plain-text error. `name` enables the
/// 400/404 "not found" reading used by the package-specific endpoints.
fn read(result: Result<Response, FetchError>, name: Option<&str>) -> Result<Value, String> {
    let describe = |failure: FetchError| match failure {
        FetchError::Timeout => error("NixHub API timed out", "TIMEOUT"),
        FetchError::Parse(msg) => err(msg),
        other => error(format!("NixHub API error: {other}"), "API_ERROR"),
    };
    let response = result.map_err(describe)?;
    if let (Some(name), 400 | 404) = (name, response.status()) {
        return Err(error(format!("Package '{name}' not found"), "NOT_FOUND"));
    }
    if response.status() >= 500 {
        return Err(error("NixHub API temporarily unavailable", "SERVICE_ERROR"));
    }
    response.ok().and_then(|response| response.json()).map_err(describe)
}

pub async fn fetch_pkg(app: &App, name: &str) -> Result<Value, String> {
    read(fetch(app, "/v1/pkg", &[("name", name)], 15).await, Some(name))
}

/// The output marked default, or the first one.
fn default_output_path(system: &Value) -> &str {
    let outputs = system.get("outputs").and_then(Value::as_array).map_or(&[][..], Vec::as_slice);
    let default = outputs.iter().find(|output| output.get("default").is_some_and(truthy));
    default.or(outputs.first()).map_or("", |output| str_of(output, "path"))
}

fn store_hash(store_path: &str) -> &str {
    store_path.split('/').nth(3).and_then(|name| name.split('-').next()).unwrap_or("")
}

/// Check binary cache status for a single system.
async fn check_system_cache(app: &App, system: String, store_path: String) -> Vec<String> {
    let mut results = vec![format!("System: {system}")];
    if store_path.is_empty() {
        results.extend(["  Store path: Not available".to_owned(), "  Status: UNKNOWN".to_owned(), String::new()]);
        return results;
    }
    results.push(format!("  Store path: {store_path}"));
    // The hash is the 32-character base32 string after /nix/store/.
    let hash = store_hash(&store_path);
    if hash.chars().count() != 32 {
        results.extend(["  Status: UNKNOWN (invalid store path)".to_owned(), String::new()]);
        return results;
    }

    let url = format!("{CACHE_NIXOS_ORG}/{hash}.narinfo");
    match app.http.send(app.http.head(&url), 5).await.map(|head| head.status()) {
        Ok(200) => match app.http.send(app.http.get(&url), 5).await {
            Ok(resp) if resp.status() == 200 => {
                let narinfo = parse_narinfo(&resp.text());
                results.push("  Status: CACHED".to_owned());
                if let Some(size) = narinfo.file_size {
                    results.push(format!("  Download size: {}", format_size(size)));
                }
                if let Some(size) = narinfo.nar_size {
                    results.push(format!("  Unpacked size: {}", format_size(size)));
                }
                if let Some(compression) = narinfo.compression.filter(|value| !value.is_empty()) {
                    results.push(format!("  Compression: {compression}"));
                }
            }
            Ok(_) => results.push("  Status: CACHED".to_owned()),
            Err(_) => results.push("  Status: UNKNOWN (cache check failed)".to_owned()),
        },
        Ok(404) => results.push("  Status: NOT CACHED".to_owned()),
        Ok(status) => results.push(format!("  Status: UNKNOWN (HTTP {status})")),
        Err(_) => results.push("  Status: UNKNOWN (cache check failed)".to_owned()),
    }
    results.push(String::new());
    results
}

/// Check binary cache status for a package, per system, concurrently.
pub async fn check_binary_cache(app: &App, name: &str, version: &str, system: &str) -> String {
    let wanted = if version.is_empty() { "latest" } else { version };
    let data = match read(fetch(app, "/v2/resolve", &[("name", name), ("version", wanted)], 15).await, Some(name)) {
        Ok(data) => data,
        Err(failure) => return failure,
    };
    let pkg_name = data.get("name").map_or_else(|| name.to_owned(), display);
    let pkg_version = data.get("version").map_or_else(|| version.to_owned(), display);
    let empty = serde_json::Map::new();
    let systems_data = match data.get("systems") {
        None => &empty,
        Some(Value::Object(systems)) => systems,
        Some(_) => return error("Invalid systems data from NixHub", "API_ERROR"),
    };

    let mut systems: Vec<(String, String)> = systems_data
        .iter()
        .filter(|(_, info)| info.is_object())
        .map(|(sys_name, info)| (sys_name.clone(), default_output_path(info).to_owned()))
        .collect();
    if systems.is_empty() {
        return error(format!("No systems found for {name}@{pkg_version}"), "NOT_FOUND");
    }
    if !system.is_empty() {
        systems.retain(|(sys_name, _)| sys_name == system);
        if systems.is_empty() {
            let mut available: Vec<&str> = systems_data.keys().map(String::as_str).collect();
            available.sort_unstable();
            return error(format!("System '{system}' not available. Available: {}", available.join(", ")), "NOT_FOUND");
        }
    }

    let mut results = vec![format!("Binary Cache Status: {pkg_name}@{pkg_version}"), String::new()];
    let checks = systems.into_iter().map(|(sys_name, path)| check_system_cache(app, sys_name, path));
    results.extend(join_all(checks).await.into_iter().flatten());
    crate::text::strip(&results.join("\n")).to_owned()
}

/// Search packages via the NixHub API.
pub async fn search_nixhub(app: &App, query: &str, limit: i64) -> String {
    let data = match read(fetch(app, "/v2/search", &[("q", query)], 15).await, None) {
        Ok(data) => data,
        Err(failure) => return failure,
    };
    // v2/search returns {"query": "...", "total_results": N, "results": [...]}.
    let packages = data.get("results").unwrap_or(&data).as_array().map_or(&[][..], Vec::as_slice);
    if packages.is_empty() {
        return format!("No packages found on NixHub matching '{query}'");
    }
    let packages = &packages[..packages.len().min(limit as usize)];
    let total = data.get("total_results").map_or_else(|| packages.len().to_string(), display);
    let mut results = vec![format!("Found {} of {total} packages on NixHub matching '{query}':\n", packages.len())];
    for pkg in packages {
        results.push(format!("* {}", str_of(pkg, "name")));
        let version = str_of(pkg, "version");
        if !version.is_empty() {
            results.push(format!("  Version: {version}"));
        }
        let summary = match str_of(pkg, "summary") {
            "" => str_of(pkg, "description"),
            summary => summary,
        };
        if !summary.is_empty() {
            results.push(format!("  {}", ellipsize(summary, 200)));
        }
        if let Some(updated) = parse_iso(str_of(pkg, "last_updated")) {
            results.push(format!("  Updated: {}", updated.format("%Y-%m-%d")));
        }
        results.push(String::new());
    }
    crate::text::strip(&results.join("\n")).to_owned()
}

/// Programs from the first system that lists any.
pub fn first_programs(release: &Value) -> Vec<String> {
    let systems = release.get("systems").and_then(Value::as_object);
    systems
        .into_iter()
        .flat_map(|systems| systems.values())
        .map(|info| strings(info.get("programs")))
        .find(|programs| !programs.is_empty())
        .unwrap_or_default()
}

pub fn program_summary(programs: &[String]) -> String {
    let shown = programs[..programs.len().min(10)].join(", ");
    if programs.len() > 10 { format!("{shown} ... ({} total)", programs.len()) } else { shown }
}

/// Flake reference of a resolved system: `github:owner/repo/rev8#attr`.
fn flake_reference(system: &Value) -> Option<String> {
    let installable = system.get("flake_installable").filter(|value| truthy(value))?;
    let reference = installable.get("ref")?;
    if str_of(reference, "type") != "github" {
        return None;
    }
    let (owner, repo) = (str_of(reference, "owner"), str_of(reference, "repo"));
    if owner.is_empty() || repo.is_empty() {
        return None;
    }
    let rev = first_chars(str_of(reference, "rev"), 8);
    Some(format!("github:{owner}/{repo}/{rev}#{}", str_of(installable, "attr_path")))
}

/// Get detailed package info, combining v1/pkg (rich metadata) with v2/resolve
/// (flake reference and store paths; best effort).
pub async fn info_nixhub(app: &App, name: &str) -> String {
    let pkg_array = match fetch_pkg(app, name).await {
        Ok(data) => data,
        Err(failure) => return failure,
    };
    let Some(pkg) = pkg_array.as_array().and_then(|array| array.first()) else {
        return error(format!("Package '{name}' not found"), "NOT_FOUND");
    };

    let version = pkg.get("version").map_or_else(|| "latest".to_owned(), display);
    let resolve = fetch(app, "/v2/resolve", &[("name", name), ("version", &version)], 10).await;
    let resolve = resolve.ok().filter(|resp| resp.status() == 200).and_then(|resp| resp.json().ok());
    let mut flake_ref = None;
    let mut store_paths = BTreeMap::new();
    if let Some(systems) = resolve.as_ref().and_then(|data| data.get("systems")).and_then(Value::as_object) {
        for (sys_name, info) in systems {
            flake_ref = flake_ref.or_else(|| flake_reference(info));
            let path = default_output_path(info);
            if !path.is_empty() {
                store_paths.insert(sys_name.clone(), path.to_owned());
            }
        }
    }

    let mut results = vec![format!("Package: {}", pkg.get("name").map_or_else(|| name.to_owned(), display))];
    if !version.is_empty() {
        results.push(format!("Version: {version}"));
    }
    let summary = str_of(pkg, "summary");
    if !summary.is_empty() {
        results.push(format!("Summary: {summary}"));
    }
    let description = str_of(pkg, "description");
    if !description.is_empty() && description != summary {
        results.push(format!("Description: {}", ellipsize(description, 500)));
    }
    results.push(String::new());
    for (label, key) in [("License", "license"), ("Homepage", "homepage")] {
        let value = str_of(pkg, key);
        if !value.is_empty() {
            results.push(format!("{label}: {value}"));
        }
    }
    let programs = first_programs(pkg);
    if !programs.is_empty() {
        results.push(format!("Programs: {}", program_summary(&programs)));
    }
    let mut platforms = strings(pkg.get("platforms"));
    if !platforms.is_empty() {
        platforms.sort();
        results.push(format!("Platforms: {}", platforms.join(", ")));
    }
    if let Some(flake_ref) = flake_ref {
        results.extend([String::new(), "Flake Reference:".to_owned(), format!("  {flake_ref}")]);
    }
    if !store_paths.is_empty() {
        results.extend([String::new(), "Store Paths:".to_owned()]);
        results.extend(store_paths.iter().map(|(sys_name, path)| format!("  {sys_name}: {path}")));
    }
    results.join("\n")
}

/// Format a v1/pkg release: `platforms` is a list of system names (or of dicts
/// with a `system` key in the old format), `commit_hash` sits at release level,
/// `last_updated` is an ISO string or an epoch timestamp.
pub fn format_release(release: &Value) -> Vec<String> {
    let version = release.get("version").map_or_else(|| "unknown".to_owned(), display);
    let mut results = vec![format!("* {version}")];

    let updated = release.get("last_updated").filter(|value| truthy(value)).and_then(|value| match value {
        Value::Number(epoch) => DateTime::from_timestamp(epoch.as_f64()? as i64, 0).map(|dt| dt.naive_utc()),
        other => parse_iso(&display(other)),
    });
    if let Some(updated) = updated {
        results.push(format!("  Updated: {}", updated.format("%Y-%m-%d")));
    }

    let platforms = release.get("platforms").and_then(Value::as_array).map_or(&[][..], Vec::as_slice);
    let mut systems: Vec<&str> = platforms
        .iter()
        .map(|platform| platform.as_str().unwrap_or_else(|| str_of(platform, "system")))
        .filter(|system| !system.is_empty())
        .collect();
    systems.sort_unstable();
    systems.dedup();
    if !systems.is_empty() {
        let has_linux = systems.iter().any(|system| system.contains("linux"));
        let has_darwin = systems.iter().any(|system| system.contains("darwin"));
        results.push(match (has_linux, has_darwin) {
            (true, true) => "  Platforms: Linux and macOS".to_owned(),
            (true, false) => "  Platforms: Linux".to_owned(),
            (false, true) => "  Platforms: macOS".to_owned(),
            (false, false) => format!("  Platforms: {}", systems.join(", ")),
        });
    }

    let commit = str_of(release, "commit_hash");
    if commit.len() == 40 && commit.bytes().all(|byte| byte.is_ascii_hexdigit()) {
        results.push(format!("  Nixpkgs commit: {commit}"));
        let systems = release.get("systems").and_then(Value::as_object);
        let attr = systems
            .into_iter()
            .flat_map(|systems| systems.values())
            .find_map(|info| strings(info.get("attr_paths")).into_iter().next());
        if let Some(attr) = attr {
            results.push(format!("  Attribute: {attr}"));
        }
    }
    results
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    #[test]
    fn narinfo_skips_malformed_values() {
        let parsed = parse_narinfo("StorePath: /nix/store/x\nFileSize: 10\nNarSize: nope\nCompression: zstd\njunk\n");
        assert_eq!(parsed, NarInfo { file_size: Some(10), nar_size: None, compression: Some("zstd".to_owned()) });
    }

    #[test]
    fn release_lists_platforms_commit_and_attribute() {
        let commit = "a".repeat(40);
        let lines = format_release(&json!({
            "version": "1.2", "last_updated": 1700000000,
            "platforms": ["x86_64-linux", {"system": "aarch64-darwin"}], "commit_hash": commit,
            "systems": {"x86_64-linux": {"attr_paths": ["hello"]}}
        }));
        assert_eq!(
            lines,
            vec![
                "* 1.2".to_owned(),
                "  Updated: 2023-11-14".to_owned(),
                "  Platforms: Linux and macOS".to_owned(),
                format!("  Nixpkgs commit: {commit}"),
                "  Attribute: hello".to_owned(),
            ]
        );
    }

    #[test]
    fn malformed_commit_and_date_are_omitted() {
        assert_eq!(format_release(&json!({"last_updated": "garbage", "commit_hash": "xyz"})), vec!["* unknown"]);
    }

    #[test]
    fn default_output_is_preferred() {
        let system = json!({"outputs": [{"path": "/a"}, {"path": "/b", "default": true}]});
        assert_eq!(default_output_path(&system), "/b");
        assert_eq!(default_output_path(&json!({"outputs": [{"path": "/a"}]})), "/a");
    }

    #[test]
    fn programs_summarize_past_ten() {
        let many: Vec<String> = (0..12).map(|i| i.to_string()).collect();
        assert_eq!(program_summary(&many), "0, 1, 2, 3, 4, 5, 6, 7, 8, 9 ... (12 total)");
    }
}
