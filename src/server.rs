//! MCP tools, tool routing and the protocol surface.
//!
//! Stateless by construction: the only protocol revision is 2026-07-28, every
//! request carries its own metadata, and no state is keyed to a client. Caches
//! in [`crate::App`] hold upstream catalogues, which any request may reuse.

use std::borrow::Cow;
use std::sync::{Arc, LazyLock};

use rmcp::ErrorData;
use rmcp::handler::server::ServerHandler;
use rmcp::model::{
    CacheScope, CallToolRequestParams, CallToolResponse, CallToolResult, ContentBlock, DiscoverResult, Implementation,
    JsonObject, ListToolsResult, PaginatedRequestParams, ProtocolVersion, ServerCapabilities, ServerConfig, Tool,
};
use rmcp::service::{RequestContext, RoleServer};

use serde::{Deserialize, Deserializer};
use serde_json::Value;

use crate::App;
use crate::caches::HtmlSource::{Darwin, HomeManager};
use crate::config::{DEFAULT_LINE_LIMIT, KNOWN_SOURCES, MAX_LINE_LIMIT, VERSION};
use crate::sources::base::{browse_options, info_html_options, list_channels, search_html_options, stats_html_options};
use crate::sources::flake_inputs::{flake_inputs_list, flake_inputs_ls, flake_inputs_read};
use crate::sources::flakehub::{info_flakehub, search_flakehub, stats_flakehub};
use crate::sources::flakes::{search_flakes, stats_flakes};
use crate::sources::nixdev::{info_nixdev, search_nixdev};
use crate::sources::nixhub::{
    check_binary_cache, fetch_pkg, first_programs, format_release, info_nixhub, program_summary, search_nixhub,
};
use crate::sources::nixos::{info_nixos, search_nixos, stats_nixos};
use crate::sources::nixvim::{browse_nixvim, info_nixvim, search_nixvim, stats_nixvim};
use crate::sources::noogle::{browse_noogle, info_noogle, search_noogle, stats_noogle};
use crate::sources::nvf::{browse_nvf, info_nvf, search_nvf, stats_nvf};
use crate::sources::store::{store_ls, store_read};
use crate::sources::wiki::{info_wiki, search_wiki};
use crate::text::{err, error, str_of};

const INSTRUCTIONS: &str = include_str!("instructions.md");
const NIX_DESCRIPTION: &str = include_str!("nix_tool.md");
const NIX_SCHEMA: &str = include_str!("nix_schema.json");
const NIX_VERSIONS_SCHEMA: &str = include_str!("nix_versions_schema.json");

/// The tool catalogue is compiled in, so clients may cache it for a day.
const TOOL_LIST_TTL_MS: u64 = 86_400_000;

const SUPPORTED_VERSIONS: &[ProtocolVersion] = &[ProtocolVersion::V_2026_07_28];

fn default_source() -> String {
    "nixos".to_owned()
}
fn default_type() -> String {
    "packages".to_owned()
}
fn default_channel() -> String {
    "unstable".to_owned()
}
fn default_version() -> String {
    "latest".to_owned()
}
fn default_limit() -> i64 {
    20
}
fn default_versions_limit() -> i64 {
    10
}

/// Models regularly send `"20"` or `20.0` for integer arguments.
fn lenient_int<'de, D: Deserializer<'de>>(deserializer: D) -> Result<i64, D::Error> {
    let value = Value::deserialize(deserializer)?;
    let parsed = match &value {
        Value::Number(number) => {
            number.as_i64().or_else(|| number.as_f64().filter(|f| f.fract() == 0.0).map(|f| f as i64))
        }
        Value::String(text) => text.trim().parse().ok(),
        _ => None,
    };
    parsed.ok_or_else(|| serde::de::Error::custom(format!("expected an integer, got {value}")))
}

#[derive(Debug, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct NixParams {
    pub action: String,
    #[serde(default)]
    pub query: String,
    #[serde(default = "default_source")]
    pub source: String,
    #[serde(default = "default_type", rename = "type")]
    pub kind: String,
    #[serde(default = "default_channel")]
    pub channel: String,
    #[serde(default = "default_limit", deserialize_with = "lenient_int")]
    pub limit: i64,
    #[serde(default = "default_version")]
    pub version: String,
    #[serde(default)]
    pub system: String,
}

#[derive(Debug, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct NixVersionsParams {
    pub package: String,
    #[serde(default)]
    pub version: String,
    #[serde(default = "default_versions_limit", deserialize_with = "lenient_int")]
    pub limit: i64,
}

/// Run blocking filesystem work off the async workers.
async fn blocking(work: impl FnOnce() -> String + Send + 'static) -> String {
    tokio::task::spawn_blocking(work).await.unwrap_or_else(err)
}

async fn search(app: &App, params: &NixParams) -> String {
    let NixParams { query, source, kind, channel, limit, .. } = params;
    let limit = *limit;
    if query.is_empty() {
        return err(r#"Query required for search. Example: {"action": "search", "query": "firefox"}"#);
    }
    match source.as_str() {
        "nixos" => {
            if !["packages", "options", "programs", "flakes"].contains(&kind.as_str()) {
                return err("For source=nixos, type must be one of: packages, options, programs, flakes. \
                     Example: {\"action\": \"search\", \"query\": \"nginx\", \"type\": \"options\"}");
            }
            search_nixos(app, query, kind, limit, channel).await
        }
        "home-manager" => search_html_options(app, HomeManager, query, limit).await,
        "darwin" => search_html_options(app, Darwin, query, limit).await,
        "flakes" => search_flakes(app, query, limit).await,
        "flakehub" => search_flakehub(app, query, limit).await,
        "nixvim" => search_nixvim(app, query, limit).await,
        "nvf" => search_nvf(app, query, limit).await,
        "wiki" => search_wiki(app, query, limit).await,
        "nix-dev" => search_nixdev(app, query, limit).await,
        "noogle" => search_noogle(app, query, limit).await,
        "nixhub" => search_nixhub(app, query, limit).await,
        other => err(format!(
            "Unknown source: '{other}'. Must be one of: \
             nixos, home-manager, darwin, flakes, flakehub, nixvim, nvf, wiki, nix-dev, noogle, nixhub."
        )),
    }
}

async fn info(app: &App, params: &NixParams) -> String {
    let NixParams { query, source, kind, channel, .. } = params;
    if query.is_empty() {
        return err(r#"Name required for info. Example: {"action": "info", "query": "firefox"}"#);
    }
    match source.as_str() {
        "flakes" => {
            let example = format!(
                "{{\"action\": \"search\", \"source\": \"flakes\", \"query\": {}}}",
                Value::from(query.as_str())
            );
            err(format!(
                "action=info is not supported for source=flakes. Use action=search instead. Example: {example}."
            ))
        }
        "nixos" => match kind.as_str() {
            "package" | "packages" => info_nixos(app, query, "package", channel).await,
            "option" | "options" => info_nixos(app, query, "option", channel).await,
            _ => err("For source=nixos, type must be 'package' or 'option'. \
                 Example: {\"action\": \"info\", \"query\": \"services.nginx.enable\", \"type\": \"option\"}"),
        },
        "home-manager" => info_html_options(app, HomeManager, query).await,
        "darwin" => info_html_options(app, Darwin, query).await,
        "flakehub" => info_flakehub(app, query).await,
        "nixvim" => info_nixvim(app, query).await,
        "nvf" => info_nvf(app, query).await,
        "wiki" => info_wiki(app, query).await,
        "nix-dev" => info_nixdev(app, query).await,
        "noogle" => info_noogle(app, query).await,
        "nixhub" => info_nixhub(app, query).await,
        other => err(format!(
            "Unknown source: '{other}'. For action=info, must be one of: \
             nixos, home-manager, darwin, flakehub, nixvim, nvf, wiki, nix-dev, noogle, nixhub."
        )),
    }
}

async fn stats(app: &App, params: &NixParams) -> String {
    match params.source.as_str() {
        "nixos" => stats_nixos(app, &params.channel).await,
        "home-manager" => stats_html_options(app, HomeManager).await,
        "darwin" => stats_html_options(app, Darwin).await,
        "flakes" => stats_flakes(app).await,
        "flakehub" => stats_flakehub(app).await,
        "nixvim" => stats_nixvim(app).await,
        "nvf" => stats_nvf(app).await,
        "noogle" => stats_noogle(app).await,
        source @ ("wiki" | "nix-dev" | "nixhub") => err(format!("Stats not available for source={source}.")),
        other => err(format!(
            "Unknown source: '{other}'. For action=stats, must be one of: \
             nixos, home-manager, darwin, flakes, flakehub, nixvim, nvf, noogle."
        )),
    }
}

async fn browse(app: &App, params: &NixParams) -> String {
    let query = &params.query;
    match params.source.as_str() {
        "nixos" => err("action=browse is not for NixOS. To search NixOS options, use: \
             {\"action\": \"search\", \"query\": \"nginx\", \"type\": \"options\"}. \
             To get a specific option's details, use: \
             {\"action\": \"info\", \"query\": \"services.nginx.enable\", \"type\": \"option\"}."),
        "home-manager" => browse_options(app, HomeManager, query).await,
        "darwin" => browse_options(app, Darwin, query).await,
        "nixvim" => browse_nixvim(app, query).await,
        "nvf" => browse_nvf(app, query).await,
        "noogle" => browse_noogle(app, query).await,
        _ => err("action=browse only supports source in: home-manager, darwin, nixvim, nvf, noogle. \
             Example: {\"action\": \"browse\", \"query\": \"programs\", \"source\": \"home-manager\"}"),
    }
}

/// `limit` for line-oriented reads: the schema default widens to a useful
/// window, and nothing exceeds the line cap.
fn line_limit(limit: i64) -> usize {
    (if limit == default_limit() { DEFAULT_LINE_LIMIT } else { limit }).min(MAX_LINE_LIMIT) as usize
}

async fn flake_inputs(params: &NixParams) -> String {
    let NixParams { query, source, kind, limit, .. } = params;
    let flake_dir = if KNOWN_SOURCES.contains(&source.as_str()) { "." } else { source.as_str() };
    // "packages" is accepted as an alias for "list" (the default type).
    match kind.as_str() {
        "list" | "packages" => flake_inputs_list(flake_dir).await,
        "ls" if query.is_empty() => err("Query required for ls (input name or input:path)"),
        "ls" => flake_inputs_ls(flake_dir, query).await,
        "read" if query.is_empty() => err("Query required for read (input:path format)"),
        "read" => flake_inputs_read(flake_dir, query, line_limit(*limit)).await,
        _ => err("Type must be one of: list, ls, read for flake-inputs"),
    }
}

async fn store(params: &NixParams) -> String {
    let NixParams { query, kind, limit, .. } = params;
    if kind != "ls" && kind != "read" {
        return err("Type must be one of: ls, read for store. \
             Example: {\"action\": \"store\", \"type\": \"ls\", \"query\": \"/nix/store/<hash>-<name>\"}");
    }
    if query.is_empty() {
        return err("Query required for store (absolute /nix/store/ path). \
             Example: {\"action\": \"store\", \"type\": \"ls\", \"query\": \"/nix/store/<hash>-<name>\"}");
    }
    let (query, limit) = (query.clone(), line_limit(*limit));
    if kind == "ls" {
        blocking(move || store_ls(&query, limit)).await
    } else {
        blocking(move || store_read(&query, limit)).await
    }
}

/// Route a `nix` tool call.
pub async fn nix(app: &App, params: NixParams) -> String {
    let (action, kind, limit) = (params.action.as_str(), params.kind.as_str(), params.limit);
    let is_line_read = (action == "flake-inputs" || action == "store") && kind == "read";
    if is_line_read {
        if !(1..=MAX_LINE_LIMIT).contains(&limit) {
            return err(format!("Limit must be 1-{MAX_LINE_LIMIT} for {action} read"));
        }
    } else if !(1..=100).contains(&limit) {
        return err("Limit must be 1-100");
    }
    match action {
        "search" => search(app, &params).await,
        "info" => info(app, &params).await,
        "stats" => stats(app, &params).await,
        // `options` is the legacy alias: the name confused small models.
        "browse" | "options" => browse(app, &params).await,
        "channels" => list_channels(app).await,
        "flake-inputs" => flake_inputs(&params).await,
        "cache" if params.query.is_empty() => err("Package name required for cache action"),
        "cache" => check_binary_cache(app, &params.query, &params.version, &params.system).await,
        "store" => store(&params).await,
        other => err(format!(
            "Unknown action: '{other}'. Must be one of: \
             search, info, stats, browse, channels, flake-inputs, cache, store. \
             Example: {{\"action\": \"search\", \"query\": \"firefox\"}}"
        )),
    }
}

/// Get package version history from NixHub.io.
pub async fn nix_versions(app: &App, params: NixVersionsParams) -> String {
    let NixVersionsParams { package, version, limit } = params;
    if crate::text::strip(&package).is_empty() {
        return err("Package name required");
    }
    if !package.chars().all(|c| c.is_ascii_alphanumeric() || matches!(c, '-' | '_' | '.')) {
        return err("Invalid package name");
    }
    if !(1..=50).contains(&limit) {
        return err("Limit must be 1-50");
    }
    let data = match fetch_pkg(app, &package).await {
        Ok(data) => data,
        Err(failure) => return failure,
    };
    let Some(releases) = data.as_array().filter(|releases| !releases.is_empty()) else {
        return error(format!("Package '{package}' not found"), "NOT_FOUND");
    };
    let limit = limit as usize;

    if !version.is_empty() {
        if let Some(release) = releases.iter().find(|release| str_of(release, "version") == version) {
            let mut lines = vec![format!("Found {package} version {version}\n")];
            lines.extend(format_release(release));
            return lines.join("\n");
        }
        let available: Vec<&str> = releases.iter().take(limit).map(|release| str_of(release, "version")).collect();
        return format!("Version {version} not found for {package}\nAvailable: {}", available.join(", "));
    }

    let latest = &releases[0];
    let mut results = vec![format!("Package: {package}")];
    for (label, key) in [("License", "license"), ("Homepage", "homepage")] {
        let value = str_of(latest, key);
        if !value.is_empty() {
            results.push(format!("{label}: {value}"));
        }
    }
    let programs = first_programs(latest);
    if !programs.is_empty() {
        results.push(format!("Programs: {}", program_summary(&programs)));
    }
    results.push(format!("Total versions: {}", releases.len()));
    results.push(String::new());

    let shown = &releases[..releases.len().min(limit)];
    results.push(format!("Recent versions ({} of {}):\n", shown.len(), releases.len()));
    for release in shown {
        results.extend(format_release(release));
        results.push(String::new());
    }
    crate::text::strip(&results.join("\n")).to_owned()
}

fn schema(source: &str) -> Arc<JsonObject> {
    #[allow(clippy::expect_used)]
    Arc::new(serde_json::from_str(source).expect("embedded tool schema is a JSON object"))
}

static TOOLS: LazyLock<Vec<Tool>> = LazyLock::new(|| {
    vec![
        Tool::new("nix", NIX_DESCRIPTION, schema(NIX_SCHEMA)).with_title("Nix"),
        Tool::new("nix_versions", "Get package version history from NixHub.io.", schema(NIX_VERSIONS_SCHEMA))
            .with_title("Nix Versions"),
    ]
});

#[derive(Clone)]
pub struct Server {
    app: Arc<App>,
}

impl Server {
    pub fn new(app: Arc<App>) -> Self {
        Self { app }
    }

    async fn dispatch(app: Arc<App>, name: String, arguments: Value) -> Result<String, ErrorData> {
        let invalid = |failure: serde_json::Error| format!("Invalid arguments for tool '{name}': {failure}");
        Ok(match name.as_str() {
            "nix" => match serde_json::from_value(arguments) {
                Ok(params) => nix(&app, params).await,
                Err(failure) => return Ok(err(invalid(failure))),
            },
            "nix_versions" => match serde_json::from_value(arguments) {
                Ok(params) => nix_versions(&app, params).await,
                Err(failure) => return Ok(err(invalid(failure))),
            },
            other => return Err(ErrorData::invalid_params(format!("Unknown tool: {other}"), None)),
        })
    }
}

impl ServerHandler for Server {
    fn get_info(&self) -> ServerConfig {
        ServerConfig::new(ServerCapabilities::builder().enable_tools().build())
            .with_server_info(Implementation::new("mcp-nixos", VERSION))
            .with_instructions(INSTRUCTIONS)
    }

    fn supported_protocol_versions(&self) -> Cow<'static, [ProtocolVersion]> {
        Cow::Borrowed(SUPPORTED_VERSIONS)
    }

    async fn discover(&self, _context: RequestContext<RoleServer>) -> Result<DiscoverResult, ErrorData> {
        let mut result = DiscoverResult::from_server_info(SUPPORTED_VERSIONS.to_vec(), self.get_info());
        result.ttl_ms = TOOL_LIST_TTL_MS;
        result.cache_scope = CacheScope::Public;
        Ok(result)
    }

    async fn list_tools(
        &self,
        _request: Option<PaginatedRequestParams>,
        _context: RequestContext<RoleServer>,
    ) -> Result<ListToolsResult, ErrorData> {
        Ok(ListToolsResult::with_all_items(TOOLS.clone())
            .with_ttl_ms(TOOL_LIST_TTL_MS)
            .with_cache_scope(CacheScope::Public))
    }

    /// Tool work runs in its own task: a cancellation aborts it (dropping any
    /// in-flight HTTP request or `nix` child), and a bug that panics becomes a
    /// tool error instead of taking the request down silently.
    async fn call_tool(
        &self,
        request: CallToolRequestParams,
        context: RequestContext<RoleServer>,
    ) -> Result<CallToolResponse, ErrorData> {
        let name = request.name.into_owned();
        let arguments = Value::Object(request.arguments.unwrap_or_default());
        let work = tokio::spawn(Self::dispatch(self.app.clone(), name, arguments));
        let abort = work.abort_handle();
        let text = tokio::select! {
            joined = work => joined.unwrap_or_else(|failure| Ok(err(format!("Tool failed: {failure}"))))?,
            () = context.ct.cancelled() => {
                abort.abort();
                return Err(ErrorData::internal_error("Request cancelled", None));
            }
        };
        Ok(CallToolResult::success(vec![ContentBlock::text(text)]).into())
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    fn params(value: Value) -> Result<NixParams, serde_json::Error> {
        serde_json::from_value(value)
    }

    #[test]
    fn defaults_match_the_schema() {
        let schema: Value = serde_json::from_str(NIX_SCHEMA).unwrap_or_default();
        let parsed = params(json!({"action": "search"})).ok();
        let parsed = parsed.as_ref();
        let default = |key: &str| schema["properties"][key]["default"].clone();
        assert_eq!(default("source"), json!(parsed.map(|p| p.source.as_str())));
        assert_eq!(default("type"), json!(parsed.map(|p| p.kind.as_str())));
        assert_eq!(default("channel"), json!(parsed.map(|p| p.channel.as_str())));
        assert_eq!(default("limit"), json!(parsed.map(|p| p.limit)));
        assert_eq!(default("version"), json!(parsed.map(|p| p.version.as_str())));
    }

    #[test]
    fn schema_properties_are_exactly_the_struct_fields() {
        let schema: Value = serde_json::from_str(NIX_SCHEMA).unwrap_or_default();
        let mut sample = serde_json::Map::new();
        for key in schema["properties"].as_object().into_iter().flatten().map(|(key, _)| key) {
            sample.insert(key.clone(), if key == "limit" { json!(1) } else { json!("x") });
        }
        assert!(params(Value::Object(sample)).is_ok());
    }

    #[test]
    fn integers_accept_numeric_strings_and_whole_floats() {
        let limit = |value: Value| params(json!({"action": "x", "limit": value})).map(|p| p.limit).ok();
        assert_eq!(limit(json!("5")), Some(5));
        assert_eq!(limit(json!(5.0)), Some(5));
        assert_eq!(limit(json!(5.5)), None);
        assert_eq!(limit(json!("many")), None);
    }

    #[test]
    fn unknown_arguments_are_rejected() {
        assert!(params(json!({"action": "search", "bogus": 1})).is_err());
    }

    #[test]
    fn line_limit_promotes_only_the_default() {
        assert_eq!(line_limit(20), 500);
        assert_eq!(line_limit(7), 7);
        assert_eq!(line_limit(5000), 2000);
    }
}
