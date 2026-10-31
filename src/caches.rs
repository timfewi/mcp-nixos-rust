//! Process-local caches. There is no persistent cache or database, and nothing
//! here is per-client: every entry is a catalogue any request may reuse.

use std::collections::{HashMap, HashSet};
use std::sync::Arc;
use std::time::{Duration, Instant};

use indexmap::IndexMap;
use serde_json::{Value, json};
use tokio::sync::{Mutex, OnceCell};

use crate::config::{
    DARWIN_URL, FALLBACK_CHANNELS, FLAKE_INDEX, HOME_MANAGER_URL, NIXDEV_SEARCH_INDEX, NIXOS_API,
    NIXVIM_OPTIONS_CHUNKS_BASE, NOOGLE_API, NVF_OPTIONS_URL,
};
use crate::html::{self, HtmlOption, get_text, selector, strip_html};
use crate::http::{FetchError, Http};
use crate::text::{collapse, commas, display, str_of, strings, strip, truthy};

pub type Channels = IndexMap<String, String>;

/// After a failed alias probe, serve the fallback for this long before spending
/// another round of requests: fallback results are deliberately not memoized.
const DISCOVERY_RETRY_COOLDOWN: Duration = Duration::from_secs(30);
/// An alias observed empty means Hydra is mid-publish, so a snapshot taken then
/// expires instead of pinning the process to the older generation.
const ROLLOVER_RECHECK: Duration = Duration::from_secs(300);

fn fallback_channels() -> Channels {
    FALLBACK_CHANNELS.iter().map(|(name, index)| ((*name).to_owned(), (*index).to_owned())).collect()
}

/// Split `latest-<gen>-nixos-<channel>` into generation and channel.
fn parse_channel_alias(alias: &str) -> Option<(u64, &str)> {
    let (generation, channel) = alias.strip_prefix("latest-")?.split_once("-nixos-")?;
    let digits = |text: &str| !text.is_empty() && text.bytes().all(|b| b.is_ascii_digit());
    let is_release = channel.split_once('.').is_some_and(|(major, minor)| digits(major) && digits(minor));
    (digits(generation) && (channel == "unstable" || is_release)).then(|| Some((generation.parse().ok()?, channel)))?
}

fn parse_flake_alias(alias: &str) -> Option<u64> {
    alias.strip_prefix("latest-")?.strip_suffix("-group-manual")?.parse().ok()
}

fn release_key(version: &str) -> Vec<u64> {
    version.split('.').filter_map(|part| part.parse().ok()).collect()
}

/// Discovered channels and resolved mappings. Alias *names* and their document
/// counts are cached separately: names are what resolution needs, counts are
/// display-only, and a partial `_count` failure must not shrink the set of
/// channels we can resolve.
#[derive(Default)]
struct ChannelState {
    alias_names: Option<Vec<String>>,
    flake_index: Option<String>,
    /// Aliases whose `_count` came back 200 with zero documents. Hydra publishes
    /// the alias before the index fills, so these must not win resolution. An
    /// alias whose probe merely failed stays a candidate.
    empty_aliases: HashSet<String>,
    available: Option<Channels>,
    resolved: Option<Channels>,
    using_fallback: bool,
    failed_at: Option<Instant>,
    rollover_at: Option<Instant>,
}

impl ChannelState {
    fn expire_rollover(&mut self) {
        if self.rollover_at.is_some_and(|at| at.elapsed() >= ROLLOVER_RECHECK) {
            *self = Self { failed_at: self.failed_at, using_fallback: self.using_fallback, ..Self::default() };
        }
    }

    async fn list_aliases(&mut self, http: &Http) -> Vec<String> {
        let request = http.get(&format!("{NIXOS_API}/_cat/aliases?format=json"));
        let request = request.basic_auth(crate::config::NIXOS_AUTH.0, Some(crate::config::NIXOS_AUTH.1));
        let entries = match http.send(request, 10).await {
            Ok(resp) if resp.status() == 200 => resp.json().unwrap_or(Value::Null),
            _ => return Vec::new(),
        };
        let names: Vec<String> =
            entries.as_array().into_iter().flatten().map(|entry| str_of(entry, "alias").to_owned()).collect();
        // The flake alias comes from the same response so flake queries do not
        // need a probe of their own.
        if let Some(best) = names.iter().filter_map(|n| Some((parse_flake_alias(n)?, n))).max() {
            self.flake_index = Some(best.1.clone());
        }
        names.into_iter().filter(|name| parse_channel_alias(name).is_some()).collect()
    }

    /// Live alias names, cached once a probe succeeds. A failed probe is not
    /// cached, but it starts a cooldown so an outage does not make every caller
    /// pay for its own round of requests.
    async fn aliases(&mut self, http: &Http) -> Vec<String> {
        if let Some(names) = &self.alias_names {
            return names.clone();
        }
        if self.failed_at.is_some_and(|at| at.elapsed() < DISCOVERY_RETRY_COOLDOWN) {
            return Vec::new();
        }
        let aliases = self.list_aliases(http).await;
        if aliases.is_empty() {
            self.failed_at = Some(Instant::now());
        } else {
            self.alias_names = Some(aliases.clone());
            self.failed_at = None;
        }
        aliases
    }

    /// Map each live alias to its document count, caching only a complete
    /// listing: a partial one would report channels as Unavailable for the life
    /// of the process.
    async fn available(&mut self, http: &Http) -> Channels {
        if let Some(counted) = &self.available {
            return counted.clone();
        }
        let aliases = self.aliases(http).await;
        let mut complete = !aliases.is_empty();
        let mut counted = Channels::new();
        let mut empty = HashSet::new();
        for alias in aliases {
            let body = json!({"query": {"match_all": {}}});
            let request = http.post_es(&format!("{NIXOS_API}/{alias}/_count"), &body);
            match http
                .send(request, 10)
                .await
                .and_then(|resp| if resp.status() == 200 { resp.json().map(Some) } else { Ok(None) })
            {
                Ok(Some(data)) => match data.get("count").and_then(Value::as_i64).unwrap_or(0) {
                    count if count > 0 => {
                        counted.insert(alias, format!("{} documents", commas(count)));
                    }
                    _ => {
                        empty.insert(alias);
                    }
                },
                _ => complete = false,
            }
        }
        // Start (or clear) the rollover clock for this snapshot.
        self.rollover_at = (!empty.is_empty()).then(Instant::now);
        self.empty_aliases = empty;
        if complete {
            self.available = Some(counted.clone());
        }
        counted
    }

    /// Returns `(mapping, used_fallback, cacheable)`.
    async fn resolve(&mut self, http: &Http) -> (Channels, bool, bool) {
        let aliases = self.aliases(http).await;
        if aliases.is_empty() {
            return (fallback_channels(), true, false);
        }
        // Count probes let confirmed-empty aliases be skipped; `available` is
        // only populated from a complete probe, so it doubles as "the winning
        // alias cannot be an undetected empty rollover index".
        self.available(http).await;
        let cacheable = self.available.is_some();

        let mut by_channel: HashMap<String, (u64, String)> = HashMap::new();
        for alias in aliases.iter().filter(|alias| !self.empty_aliases.contains(*alias)) {
            if let Some((generation, channel)) = parse_channel_alias(alias) {
                let best = by_channel.entry(channel.to_owned()).or_insert((generation, alias.clone()));
                if (generation, alias) > (best.0, &best.1) {
                    *best = (generation, alias.clone());
                }
            }
        }

        let mut resolved = Channels::new();
        if let Some((_, alias)) = by_channel.get("unstable") {
            resolved.insert("unstable".to_owned(), alias.clone());
        }
        let mut releases: Vec<&String> = by_channel.keys().filter(|name| *name != "unstable").collect();
        releases.sort_by_key(|name| std::cmp::Reverse(release_key(name)));
        for version in &releases {
            resolved.insert((*version).clone(), by_channel[*version].1.clone());
        }
        if let Some(newest) = releases.first() {
            let alias = by_channel[*newest].1.clone();
            resolved.insert("stable".to_owned(), alias.clone());
            resolved.insert("beta".to_owned(), alias);
        }
        if resolved.is_empty() {
            return (fallback_channels(), true, false);
        }
        (resolved, false, cacheable)
    }
}

#[derive(Default)]
pub struct ChannelCache {
    state: Mutex<ChannelState>,
}

impl ChannelCache {
    pub async fn resolved(&self, http: &Http) -> Channels {
        let mut state = self.state.lock().await;
        state.expire_rollover();
        if let Some(resolved) = &state.resolved {
            return resolved.clone();
        }
        let (resolved, used_fallback, cacheable) = state.resolve(http).await;
        state.using_fallback = used_fallback;
        // Never memoize a fallback: its generations go stale and a retired alias
        // 404s on every query.
        if !used_fallback && cacheable {
            state.resolved = Some(resolved.clone());
        }
        resolved
    }

    pub async fn available(&self, http: &Http) -> Channels {
        let mut state = self.state.lock().await;
        state.expire_rollover();
        state.available(http).await
    }

    pub async fn using_fallback(&self) -> bool {
        self.state.lock().await.using_fallback
    }

    /// Live `latest-<gen>-group-manual` alias, or the `FLAKE_INDEX` fallback.
    pub async fn flake_index(&self, http: &Http) -> String {
        let mut state = self.state.lock().await;
        state.expire_rollover();
        if state.flake_index.is_none() {
            state.aliases(http).await;
        }
        state.flake_index.clone().unwrap_or_else(|| FLAKE_INDEX.to_owned())
    }
}

/// Rendered Nixvim option. HTML fields are flattened once at load, so searches
/// scan plain strings instead of re-parsing ~17k fragments per call.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct NixvimOption {
    pub name: String,
    pub r#type: String,
    pub description: String,
    pub default: String,
    pub example: String,
    pub declaration: String,
}

impl NixvimOption {
    fn from_json(option: &Value) -> Self {
        Self {
            name: str_of(option, "name").to_owned(),
            r#type: str_of(option, "type").to_owned(),
            description: strip_html(str_of(option, "description")),
            default: strip_html(str_of(option, "default")),
            example: strip_html(str_of(option, "example")),
            declaration: strings(option.get("declarations")).into_iter().next().unwrap_or_default(),
        }
    }
}

#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct NvfOption {
    pub name: String,
    pub r#type: String,
    pub description: String,
    pub default: String,
    pub example: String,
    pub declarations: Vec<String>,
    pub url: String,
}

/// Extract a field as readable plain text, preserving code blocks.
fn nvf_field(option: scraper::ElementRef<'_>, css: &str, label: &str) -> String {
    let Some(field) = option.select(&selector(css)).next() else {
        return String::new();
    };
    let preserve_lines = field.select(&selector("pre")).next().is_some();
    let joined = get_text(field, if preserve_lines { "\n" } else { " " }, true);
    let value = match joined.strip_prefix(label).filter(|_| !label.is_empty()) {
        Some(rest) => rest.trim_start(),
        None => joined.as_str(),
    };
    if preserve_lines {
        let lines: Vec<&str> = value.lines().map(str::trim_end).collect();
        strip(&lines.join("\n")).to_owned()
    } else {
        collapse(value)
    }
}

/// Parse normalized NVF option records from `options.html`.
pub fn parse_nvf_options(html: &str) -> Vec<NvfOption> {
    let doc = scraper::Html::parse_document(html);
    let base = reqwest::Url::parse(NVF_OPTIONS_URL).ok();
    let join = |href: &str| {
        base.as_ref().and_then(|base| base.join(href).ok()).map_or_else(|| href.to_owned(), |url| url.to_string())
    };
    let anchor_css = selector(".option-name .option-anchor");
    let declared_css = selector(".option-declared a[href]");
    let mut options = Vec::new();
    for option in doc.select(&selector(".options-container .option")) {
        let Some(anchor) = option.select(&anchor_css).next() else {
            continue;
        };
        let name = get_text(anchor, " ", true);
        if name != "vim" && !name.starts_with("vim.") {
            continue;
        }
        let url = match anchor.value().attr("href") {
            Some(href) => join(href),
            None => match option.value().attr("id").filter(|id| !id.is_empty()) {
                Some(id) => format!("{NVF_OPTIONS_URL}#{id}"),
                None => NVF_OPTIONS_URL.to_owned(),
            },
        };
        options.push(NvfOption {
            name,
            r#type: nvf_field(option, ".option-type", "Type:"),
            description: nvf_field(option, ".option-description", ""),
            default: nvf_field(option, ".option-default", "Default:"),
            example: nvf_field(option, ".option-example", "Example:"),
            declarations: option
                .select(&declared_css)
                .filter_map(|declared| declared.value().attr("href"))
                .map(join)
                .collect(),
            url,
        });
    }
    options
}

/// The Sphinx search index, reduced to the fields search reads. A term seen in
/// a single document is stored upstream as a bare int; here it is always a list.
#[derive(Debug, Default)]
pub struct NixDevIndex {
    pub docnames: Vec<String>,
    pub titles: Vec<String>,
    pub terms: HashMap<String, Vec<usize>>,
    pub titleterms: HashMap<String, Vec<usize>>,
}

fn term_map(value: Option<&Value>) -> HashMap<String, Vec<usize>> {
    let id = |value: &Value| value.as_u64().map(|id| id as usize);
    value
        .and_then(Value::as_object)
        .into_iter()
        .flatten()
        .map(|(term, raw)| {
            let ids = match raw {
                Value::Array(items) => items.iter().filter_map(id).collect(),
                other => id(other).into_iter().collect(),
            };
            (term.clone(), ids)
        })
        .collect()
}

/// Parse `Search.setIndex({...})`.
pub fn parse_nixdev_index(script: &str) -> Result<NixDevIndex, String> {
    let body = strip(script)
        .strip_prefix("Search.setIndex(")
        .and_then(|rest| rest.strip_suffix(')'))
        .ok_or("Unexpected search index format")?;
    let index: Value = serde_json::from_str(body).map_err(|err| err.to_string())?;
    Ok(NixDevIndex {
        docnames: strings(index.get("docnames")),
        titles: strings(index.get("titles")),
        terms: term_map(index.get("terms")),
        titleterms: term_map(index.get("titleterms")),
    })
}

/// One Noogle function, with its derived text computed once at load.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct NoogleDoc {
    pub path: String,
    pub signature: String,
    pub aliases: Vec<String>,
    pub description: String,
    pub example: String,
    /// Primop arity and argument names, for builtins.
    pub primop: Option<(String, Vec<String>)>,
    pub position_file: String,
    pub position_line: Option<String>,
}

fn dotted(parts: &Value) -> String {
    match parts {
        Value::Array(items) => items.iter().map(display).collect::<Vec<_>>().join("."),
        other => display(other),
    }
}

impl NoogleDoc {
    pub fn from_json(doc: &Value) -> Self {
        let meta = doc.get("meta").filter(|meta| meta.is_object()).unwrap_or(&Value::Null);
        let content = doc.get("content").filter(|content| content.is_object()).unwrap_or(&Value::Null);

        let path = match meta.get("path").filter(|path| truthy(path)) {
            Some(path) => dotted(path),
            None => meta.get("title").filter(|title| truthy(title)).map(display).unwrap_or_default(),
        };
        // `meta.signature` is the live field; `content.signature`/`content.type`
        // are kept for documents shaped differently.
        let signature = [meta.get("signature"), content.get("signature"), content.get("type")]
            .into_iter()
            .flatten()
            .find(|value| truthy(value))
            .map(|value| collapse(&display(value)))
            .unwrap_or_default();
        let aliases = meta.get("aliases").and_then(Value::as_array).into_iter().flatten().map(dotted).collect();
        let description = [content.get("content"), content.get("lambda").and_then(|lambda| lambda.get("content"))]
            .into_iter()
            .flatten()
            .find(|value| truthy(value))
            .map(|value| strip_html(&display(value)))
            .unwrap_or_default();
        let primop = meta
            .get("primop_meta")
            .and_then(|primop| Some((primop.get("arity").filter(|arity| !arity.is_null())?, primop)))
            .map(|(arity, primop)| (display(arity), strings(primop.get("args"))));
        let position = meta.get("position").unwrap_or(&Value::Null);
        Self {
            path,
            signature,
            aliases,
            description,
            example: strip_html(str_of(content, "example")),
            primop,
            position_file: str_of(position, "file").to_owned(),
            position_line: position.get("line").filter(|line| truthy(line)).map(display),
        }
    }
}

fn describe(err: &FetchError, what: &str, index: &str) -> String {
    match err {
        FetchError::Timeout => format!("Timeout fetching {what}"),
        FetchError::Parse(msg) => format!("Failed to parse {index}: {msg}"),
        other => format!("Failed to fetch {index}: {other}"),
    }
}

type Shared<T> = OnceCell<Arc<T>>;

#[derive(Default)]
pub struct Caches {
    pub channels: ChannelCache,
    pub branch_revs: Mutex<HashMap<String, (String, Instant)>>,
    home_manager: Shared<Vec<HtmlOption>>,
    darwin: Shared<Vec<HtmlOption>>,
    nixvim: Shared<Vec<NixvimOption>>,
    nvf: Shared<Vec<NvfOption>>,
    nixdev: Shared<NixDevIndex>,
    noogle: Shared<Vec<NoogleDoc>>,
}

/// Which HTML-parsed catalogue a shared option handler serves.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum HtmlSource {
    HomeManager,
    Darwin,
}

impl HtmlSource {
    pub fn display_name(self) -> &'static str {
        match self {
            Self::HomeManager => "Home Manager",
            Self::Darwin => "nix-darwin",
        }
    }

    fn url(self) -> &'static str {
        match self {
            Self::HomeManager => HOME_MANAGER_URL,
            Self::Darwin => DARWIN_URL,
        }
    }
}

async fn off_thread<T: Send + 'static>(work: impl FnOnce() -> T + Send + 'static) -> Result<T, String> {
    tokio::task::spawn_blocking(work).await.map_err(|err| err.to_string())
}

impl Caches {
    /// Fetch, parse, and cache a full HTML option catalogue. Parsed once per
    /// process, off the async workers: Home Manager's document is ~4 MB.
    pub async fn html_options(&self, http: &Http, source: HtmlSource) -> Result<Arc<Vec<HtmlOption>>, String> {
        let cell = match source {
            HtmlSource::HomeManager => &self.home_manager,
            HtmlSource::Darwin => &self.darwin,
        };
        cell.get_or_try_init(|| async {
            let url = source.url();
            let fetch = |err: FetchError| format!("Failed to fetch docs: {err}");
            let resp = http.send(http.get(url), 30).await.and_then(|resp| resp.ok()).map_err(fetch)?;
            let page = resp.text();
            let options = off_thread(move || html::parse_options(&page, url)).await?;
            if options.is_empty() {
                return Err(format!("Failed to parse {} options: no options found", source.display_name()));
            }
            Ok(Arc::new(options))
        })
        .await
        .cloned()
    }

    /// Walk NuschtOS chunks until a 404. A 404 on the *first* chunk almost
    /// always means a layout change, and a non-list mid-sequence fails loud so a
    /// partial option set is never cached.
    pub async fn nixvim_options(&self, http: &Http) -> Result<Arc<Vec<NixvimOption>>, String> {
        self.nixvim
            .get_or_try_init(|| async {
                let fetch = |err: FetchError| describe(&err, "Nixvim options", "Nixvim options");
                let mut chunks = Vec::new();
                for chunk_id in 0.. {
                    let url = format!("{NIXVIM_OPTIONS_CHUNKS_BASE}/{chunk_id}.json");
                    let resp = http.send(http.get(&url), 30).await.map_err(fetch)?;
                    if resp.status() == 404 {
                        if chunk_id == 0 {
                            return Err(format!(
                                "First Nixvim options chunk returned 404 at {url}; \
                                 the NuschtOS data layout may have changed again."
                            ));
                        }
                        break;
                    }
                    match resp.ok().and_then(|resp| resp.json()).map_err(fetch)? {
                        Value::Array(chunk) => chunks.push(chunk),
                        _ => {
                            return Err(format!("Unexpected Nixvim options payload at {url}: expected JSON list."));
                        }
                    }
                }
                let options =
                    off_thread(move || chunks.iter().flatten().map(NixvimOption::from_json).collect::<Vec<_>>())
                        .await?;
                Ok(Arc::new(options))
            })
            .await
            .cloned()
    }

    pub async fn nvf_options(&self, http: &Http) -> Result<Arc<Vec<NvfOption>>, String> {
        self.nvf
            .get_or_try_init(|| async {
                let fetch = |err: FetchError| describe(&err, "NVF options", "NVF options");
                let resp = http.send(http.get(NVF_OPTIONS_URL), 30).await.and_then(|r| r.ok()).map_err(fetch)?;
                let page = resp.text();
                let options = off_thread(move || parse_nvf_options(&page)).await?;
                if options.is_empty() {
                    return Err("Failed to parse NVF options: no canonical vim.* options found".to_owned());
                }
                Ok(Arc::new(options))
            })
            .await
            .cloned()
    }

    pub async fn nixdev_index(&self, http: &Http) -> Result<Arc<NixDevIndex>, String> {
        self.nixdev
            .get_or_try_init(|| async {
                let fetch = |err: FetchError| describe(&err, "nix.dev search index", "nix.dev index");
                let resp = http.send(http.get(NIXDEV_SEARCH_INDEX), 30).await.and_then(|r| r.ok()).map_err(fetch)?;
                let script = resp.text();
                let index = off_thread(move || parse_nixdev_index(&script)).await?;
                Ok(Arc::new(index.map_err(|err| format!("Failed to parse nix.dev index: {err}"))?))
            })
            .await
            .cloned()
    }

    pub async fn noogle(&self, http: &Http) -> Result<Arc<Vec<NoogleDoc>>, String> {
        self.noogle
            .get_or_try_init(|| async {
                let fetch = |err: FetchError| describe(&err, "Noogle data", "Noogle data");
                let resp = http.send(http.get(NOOGLE_API), 60).await.and_then(|r| r.ok()).map_err(fetch)?;
                let payload = resp.json().map_err(fetch)?;
                let docs = off_thread(move || {
                    let data = payload.get("data").and_then(Value::as_array);
                    data.into_iter().flatten().map(NoogleDoc::from_json).collect::<Vec<_>>()
                })
                .await?;
                Ok(Arc::new(docs))
            })
            .await
            .cloned()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn rollover_snapshots_expire() {
        let mut state = ChannelState {
            alias_names: Some(vec!["latest-50-nixos-unstable".to_owned()]),
            rollover_at: Some(Instant::now()),
            ..ChannelState::default()
        };
        state.expire_rollover();
        assert!(state.alias_names.is_some(), "a fresh snapshot stays");
        state.rollover_at = Instant::now().checked_sub(ROLLOVER_RECHECK);
        state.expire_rollover();
        assert!(state.alias_names.is_none() && state.rollover_at.is_none());
    }

    #[test]
    fn channel_alias_parsing() {
        assert_eq!(parse_channel_alias("latest-46-nixos-unstable"), Some((46, "unstable")));
        assert_eq!(parse_channel_alias("latest-50-nixos-26.05"), Some((50, "26.05")));
        assert_eq!(parse_channel_alias("latest-50-nixos-foo"), None);
        assert_eq!(parse_channel_alias("latest-x-nixos-unstable"), None);
        assert_eq!(parse_flake_alias("latest-51-group-manual"), Some(51));
    }

    #[test]
    fn nixdev_index_normalizes_single_ids() {
        let index = parse_nixdev_index(
            r#"Search.setIndex({"docnames":["a"],"titles":["A"],"terms":{"x":0,"y":[0,2]},"titleterms":{}})"#,
        );
        let index = index.ok().unwrap_or_default();
        assert_eq!(index.terms["x"], vec![0]);
        assert_eq!(index.terms["y"], vec![0, 2]);
        assert!(parse_nixdev_index("nope").is_err());
    }

    #[test]
    fn noogle_doc_extracts_fields() {
        let doc = NoogleDoc::from_json(&json!({
            "meta": {
                "path": ["lib", "strings", "concat"],
                "signature": "concat  ::\n a -> a",
                "aliases": [["lib", "concat"], "other"],
                "primop_meta": {"arity": 2, "args": ["a", "b"]},
                "position": {"file": "lib/strings.nix", "line": 12}
            },
            "content": {"content": "<p>Joins &amp; more</p>", "example": "<code>x</code>"}
        }));
        assert_eq!(doc.path, "lib.strings.concat");
        assert_eq!(doc.signature, "concat :: a -> a");
        assert_eq!(doc.aliases, ["lib.concat", "other"]);
        assert_eq!(doc.description, "Joins & more");
        assert_eq!(doc.example, "x");
        assert_eq!(doc.primop, Some(("2".to_owned(), vec!["a".to_owned(), "b".to_owned()])));
        assert_eq!(doc.position_line.as_deref(), Some("12"));
    }

    #[test]
    fn noogle_falls_back_to_title_and_lambda() {
        let doc = NoogleDoc::from_json(&json!({
            "meta": {"title": "builtins.map"},
            "content": {"type": "a -> b", "lambda": {"content": "doc"}}
        }));
        assert_eq!(doc.path, "builtins.map");
        assert_eq!(doc.signature, "a -> b");
        assert_eq!(doc.description, "doc");
    }

    #[test]
    fn nvf_parser_reads_canonical_options_only() {
        let html = r##"<div class="options-container">
<div class="option" id="o1"><div class="option-name"><a class="option-anchor" href="#opt-vim.lsp">vim.lsp</a></div>
<div class="option-type">Type: boolean</div><div class="option-description">Enable   LSP</div>
<div class="option-default">Default: <pre>false
</pre></div><div class="option-declared"><a href="https://x.dev/a.nix">a.nix</a></div></div>
<div class="option"><div class="option-name"><a class="option-anchor">programs.other</a></div></div></div>"##;
        let options = parse_nvf_options(html);
        assert_eq!(options.len(), 1);
        assert_eq!(options[0].name, "vim.lsp");
        assert_eq!(options[0].r#type, "boolean");
        assert_eq!(options[0].description, "Enable LSP");
        assert_eq!(options[0].default, "false");
        assert_eq!(options[0].declarations, ["https://x.dev/a.nix"]);
        assert_eq!(options[0].url, "https://nvf.notashelf.dev/options.html#opt-vim.lsp");
    }
}
