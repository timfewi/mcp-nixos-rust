//! Source behavior against canned upstream responses.

#![allow(clippy::unwrap_used)]

mod common;

use common::*;
use serde_json::{Value, json};

fn with_es(hits: impl Fn(&Request) -> Reply + Send + Sync + 'static) -> Fixture {
    serve(move |request| search_backend(request).unwrap_or_else(|| hits(request)))
}

#[tokio::test]
async fn package_search_shows_attr_names_and_counts_rendered_hits() {
    let fixture = with_es(|_| {
        es_hits(vec![
            json!({"package_pname": "firefox", "package_attr_name": "firefox", "package_pversion": "1",
                   "package_description": "Browser"}),
            json!({"package_pname": "firefox", "package_attr_name": "firefox-esr", "package_pversion": "2"}),
        ])
    });
    let text = call(&fixture.app(), json!({"action": "search", "query": "firefox"})).await;
    assert_eq!(text, "Found 2 packages matching 'firefox':\n\n* firefox (1)\n  Browser\n\n* firefox-esr (2)");
    let sent = fixture.request_bodies();
    assert!(sent.contains("package_attr_name.*^5.4") && sent.contains("rescore_query"), "{sent}");
}

#[tokio::test]
async fn programs_search_keeps_only_exact_programs_and_skips_rescore() {
    let fixture = with_es(|_| {
        es_hits(vec![
            json!({"package_pname": "ripgrep", "package_programs": ["rg"]}),
            json!({"package_pname": "other", "package_programs": ["rgx"]}),
        ])
    });
    let text = call(&fixture.app(), json!({"action": "search", "query": "RG", "type": "programs"})).await;
    assert_eq!(text, "Found 1 programs matching 'RG':\n\n* rg (provided by ripgrep)");
    assert!(!fixture.request_bodies().contains("rescore"));
}

#[tokio::test]
async fn option_search_strips_rendered_html() {
    let fixture = with_es(|_| {
        es_hits(vec![json!({"option_name": "a.b", "option_type": "boolean",
            "option_description": "<rendered-html><p>Hi <code>x</code></p></rendered-html>"})])
    });
    let text = call(&fixture.app(), json!({"action": "search", "query": "a.b", "type": "options"})).await;
    assert_eq!(text, "Found 1 options matching 'a.b':\n\n* a.b\n  Type: boolean\n  Hi x");
}

#[tokio::test]
async fn metacharacter_only_queries_never_reach_elasticsearch() {
    let fixture = with_es(|_| panic!("no search expected"));
    let text = call(&fixture.app(), json!({"action": "search", "query": "*?"})).await;
    assert_eq!(text, "No packages found matching '*?'");
    assert_eq!(fixture.count("_search"), 0);
}

#[tokio::test]
async fn info_prefers_attribute_match_then_flags_pname_ambiguity() {
    let fixture = with_es(|request| {
        if request.body.contains("package_attr_name") {
            return es_hits(vec![]);
        }
        es_hits(vec![
            json!({"package_pname": "firefox", "package_attr_name": "firefox-mobile", "package_pversion": "1"}),
            json!({"package_pname": "firefox", "package_attr_name": "firefox", "package_pversion": "2"}),
            json!({"package_pname": "firefox", "package_attr_name": "firefox-esr", "package_pversion": "3"}),
        ])
    });
    let text = call(&fixture.app(), json!({"action": "info", "query": "firefox"})).await;
    assert!(text.starts_with("Package: firefox\nVersion: 2\n"), "{text}");
    assert!(text.contains("Returned the canonical entry (firefox)."), "{text}");
    assert!(text.contains("pname: firefox-esr, firefox-mobile."), "{text}");
    assert!(text.contains(r#"{"action": "info", "query": "firefox-esr", "channel": "unstable"}"#), "{text}");
}

#[tokio::test]
async fn info_ambiguity_tiebreak_is_deterministic() {
    let fixture = with_es(|request| {
        if request.body.contains("package_attr_name") {
            return es_hits(vec![]);
        }
        es_hits(vec![
            json!({"package_pname": "p", "package_attr_name": "z-p", "package_pversion": "1"}),
            json!({"package_pname": "p", "package_attr_name": "a-p", "package_pversion": "2"}),
        ])
    });
    let text = call(&fixture.app(), json!({"action": "info", "query": "p"})).await;
    assert!(text.contains("Attribute: a-p"), "{text}");
    assert!(text.contains("Returned a representative entry (a-p)"), "{text}");
}

#[tokio::test]
async fn info_reports_homepage_license_and_missing_packages() {
    let fixture = with_es(|request| {
        if request.body.contains("nope") {
            return es_hits(vec![]);
        }
        es_hits(vec![json!({"package_pname": "git", "package_attr_name": "git", "package_pversion": "2",
            "package_homepage": ["https://git-scm.com"], "package_license_set": ["GPL-2.0", "LGPL"]})])
    });
    let app = fixture.app();
    let text = call(&app, json!({"action": "info", "query": "git"})).await;
    assert_eq!(text, "Package: git\nVersion: 2\nHomepage: https://git-scm.com\nLicense: GPL-2.0, LGPL");
    let text = call(&app, json!({"action": "info", "query": "nope"})).await;
    assert_eq!(text, "Error (NOT_FOUND): Package 'nope' not found");
}

#[tokio::test]
async fn elasticsearch_failures_surface_as_api_errors() {
    let fixture = with_es(|_| status(502));
    let text = call(&fixture.app(), json!({"action": "search", "query": "x"})).await;
    assert!(text.starts_with("Error (ERROR): API error: 502 Server Error: Bad Gateway for url: "), "{text}");
}

#[tokio::test]
async fn stats_format_thousands_and_fail_when_both_counts_are_zero() {
    let fixture = serve(|request| {
        if request.path.ends_with("/_count") && request.body.contains("package") {
            return json_reply(json!({"count": 123456}));
        }
        search_backend(request).unwrap_or_else(|| status(404))
    });
    let text = call(&fixture.app(), json!({"action": "stats"})).await;
    assert_eq!(text, "NixOS Statistics (unstable):\n* Packages: 123,456\n* Options: 1,234");
}

fn catalogue(path: &str) -> Option<Reply> {
    if path.contains("/home-manager/print.html") {
        return Some(ok(r##"<main>
<h2 id="opt-programs.git.enable"><a class="header" href="#a">programs.git.enable</a></h2>
<p>Whether to enable Git.</p><p><em>Type:</em> boolean</p>
<h2 id="opt-programs.bun.enableGitIntegration"><a class="header" href="#b">programs.bun.enableGitIntegration</a></h2>
<p>Bun git integration.</p>
<h2 id="opt-programs.difftastic.git.enable"><a class="header" href="#c">programs.difftastic.git.enable</a></h2>
<h2 id="opt-uninstall"><a class="header" href="#d">uninstall</a></h2><p>Remove.</p></main>"##));
    }
    None
}

#[tokio::test]
async fn html_search_ranks_path_segments_and_parses_once() {
    let fixture = serve(|request| catalogue(&request.path).unwrap_or_else(|| status(404)));
    let app = fixture.app();
    let text = call(&app, json!({"action": "search", "source": "home-manager", "query": "git"})).await;
    let order: Vec<&str> = text.lines().filter(|line| line.starts_with("* ")).collect();
    assert_eq!(
        order,
        ["* programs.git.enable", "* programs.difftastic.git.enable", "* programs.bun.enableGitIntegration"]
    );
    call(&app, json!({"action": "search", "source": "home-manager", "query": "bun"})).await;
    assert_eq!(fixture.count("print.html"), 1);
}

#[tokio::test]
async fn html_info_stats_and_browse() {
    let fixture = serve(|request| catalogue(&request.path).unwrap_or_else(|| status(404)));
    let app = fixture.app();
    let hm = |extra: Value| {
        let mut args = json!({"source": "home-manager"});
        args.as_object_mut().unwrap().extend(extra.as_object().unwrap().clone());
        args
    };
    let info = call(&app, hm(json!({"action": "info", "query": "programs.git.enable"}))).await;
    assert_eq!(info, "Option: programs.git.enable\nType: boolean\nDescription: Whether to enable Git.");
    let missing = call(&app, hm(json!({"action": "info", "query": "programs.git"}))).await;
    assert_eq!(missing, "Error (NOT_FOUND): Option 'programs.git' not found. Similar: programs.git.enable");
    let stats = call(&app, hm(json!({"action": "stats"}))).await;
    assert_eq!(
        stats,
        "Home Manager Statistics:\n* Total options: 4\n* Categories: 1\n* Top categories:\n  - programs: 3"
    );
    let top = call(&app, hm(json!({"action": "browse"}))).await;
    assert_eq!(top, "Home Manager categories (1 total):\n\n* programs (3 options)");
    let prefix = call(&app, hm(json!({"action": "options", "query": "programs.git"}))).await;
    assert!(prefix.starts_with("Home Manager options with prefix 'programs.git' (1 found):"), "{prefix}");
}

#[tokio::test]
async fn html_fetch_failures_and_empty_documents_are_reported() {
    let fixture = serve(|_| status(500));
    let text = call(&fixture.app(), json!({"action": "stats", "source": "darwin"})).await;
    assert!(text.starts_with("Error (ERROR): Failed to fetch docs: 500 Server Error"), "{text}");

    let fixture = serve(|_| ok("<html><body>no options</body></html>"));
    let text = call(&fixture.app(), json!({"action": "stats", "source": "darwin"})).await;
    assert_eq!(text, "Error (ERROR): Failed to parse nix-darwin options: no options found");
}

#[tokio::test]
async fn failed_catalogues_are_not_cached() {
    let attempts = std::sync::Arc::new(std::sync::atomic::AtomicUsize::new(0));
    let seen = attempts.clone();
    let fixture = serve(move |request| {
        if seen.fetch_add(1, std::sync::atomic::Ordering::SeqCst) == 0 {
            status(500)
        } else {
            catalogue(&request.path).unwrap_or_else(|| status(404))
        }
    });
    let app = fixture.app();
    let args = json!({"action": "stats", "source": "home-manager"});
    assert!(call(&app, args.clone()).await.starts_with("Error"));
    assert!(call(&app, args).await.starts_with("Home Manager Statistics"));
}

fn nixvim(path: &str) -> Reply {
    match path.rsplit('/').next() {
        Some("0.json") => json_reply(json!([
            {"name": "plugins.a.enable", "type": "boolean", "description": "<p>Enable <b>a</b></p>",
             "default": "<code>false</code>", "declarations": ["a.nix"]},
            {"name": "plugins.b.enable", "type": "boolean", "description": "Enable b"}
        ])),
        Some("1.json") => json_reply(json!([{"name": "colorscheme", "description": "Theme"}])),
        _ => status(404),
    }
}

#[tokio::test]
async fn nixvim_walks_chunks_until_404_and_caches() {
    let fixture = serve(|request| nixvim(&request.path));
    let app = fixture.app();
    let info = call(&app, json!({"action": "info", "source": "nixvim", "query": "PLUGINS.A.ENABLE"})).await;
    assert_eq!(
        info,
        "Nixvim Option: plugins.a.enable\nType: boolean\nDescription: Enable a\nDefault: false\nDeclared in: a.nix"
    );
    let stats = call(&app, json!({"action": "stats", "source": "nixvim"})).await;
    assert!(stats.contains("* Total options: 3\n* Categories: 2"), "{stats}");
    assert_eq!(fixture.count("0.json"), 1);
    let browse = call(&app, json!({"action": "browse", "source": "nixvim", "query": "plugins"})).await;
    assert!(browse.starts_with("Nixvim options with prefix 'plugins' (2 found):"), "{browse}");
}

#[tokio::test]
async fn nixvim_layout_changes_fail_loudly() {
    let fixture = serve(|_| status(404));
    let text = call(&fixture.app(), json!({"action": "stats", "source": "nixvim"})).await;
    assert!(text.contains("First Nixvim options chunk returned 404") && text.contains("layout may have changed"));

    let fixture = serve(|_| json_reply(json!({"not": "a list"})));
    let text = call(&fixture.app(), json!({"action": "stats", "source": "nixvim"})).await;
    assert!(text.contains("Unexpected Nixvim options payload"), "{text}");
}

fn noogle() -> Reply {
    json_reply(json!({"data": [
        {"meta": {"path": ["lib", "lists", "concatMap"], "signature": "concatMap :: f"},
         "content": {"content": "<p>Map and concat</p>"}},
        {"meta": {"path": ["lib", "lists", "map"], "aliases": [["builtins", "map"]]},
         "content": {"content": "Maps", "example": "<code>map f xs</code>"}},
        {"meta": {"path": ["builtins", "add"], "primop_meta": {"arity": 2, "args": ["a", "b"]},
                  "position": {"file": "x.nix", "line": 3}}, "content": null}
    ]}))
}

#[tokio::test]
async fn noogle_ranks_exact_names_and_survives_null_content() {
    let fixture = serve(|_| noogle());
    let app = fixture.app();
    let text = call(&app, json!({"action": "search", "source": "noogle", "query": "map"})).await;
    let order: Vec<&str> = text.lines().filter(|line| line.starts_with("* ")).collect();
    assert_eq!(order, ["* lib.lists.map", "* lib.lists.concatMap"]);

    // Upstream documents can carry `"content": null`; these must still render.
    let text = call(&app, json!({"action": "info", "source": "noogle", "query": "builtins.add"})).await;
    assert_eq!(
        text,
        "Noogle Function: builtins.add\nPath: builtins.add\nPrimop: Yes (arity: 2, args: a, b)\n\nSource: x.nix:3"
    );
    let alias = call(&app, json!({"action": "info", "source": "noogle", "query": "builtins.map"})).await;
    assert!(alias.starts_with("Noogle Function: lib.lists.map"), "{alias}");
    let stats = call(&app, json!({"action": "stats", "source": "noogle"})).await;
    assert!(stats.contains("* Total functions: 3\n* With type signatures: 1\n* With documentation: 2"), "{stats}");
}

#[tokio::test]
async fn nvf_normalizes_wrapped_paths() {
    let page = r##"<div class="options-container">
<div class="option"><div class="option-name"><a class="option-anchor" href="#o">vim.lsp.enable</a></div>
<div class="option-type">Type: boolean</div><div class="option-description">Enable LSP</div></div></div>"##;
    let fixture = serve(move |_| ok(page));
    let app = fixture.app();
    let text =
        call(&app, json!({"action": "info", "source": "nvf", "query": "programs.nvf.settings.vim.lsp.enable"})).await;
    assert!(text.starts_with("NVF Option: vim.lsp.enable\nType: boolean\nDescription: Enable LSP"), "{text}");
    let browse = call(&app, json!({"action": "browse", "source": "nvf", "query": "programs.nvf.vim.lsp"})).await;
    assert!(browse.starts_with("NVF options with prefix 'vim.lsp' (1 found):"), "{browse}");
    let stats = call(&app, json!({"action": "stats", "source": "nvf"})).await;
    assert!(stats.contains("* Documentation track: unstable"), "{stats}");
}

#[tokio::test]
async fn wiki_search_and_info() {
    let fixture = serve(|request| {
        if request.path.contains("action=parse") {
            if request.path.contains("Missing") {
                return json_reply(json!({"error": {"code": "missingtitle", "info": "x"}}));
            }
            if request.path.contains("Broken") {
                return json_reply(json!({"error": {"code": "other", "info": "boom"}}));
            }
            let body = format!(r#"<div class="box">stub</div><p>{}</p>"#, "w ".repeat(1000));
            return json_reply(json!({"parse": {"title": "Home Manager", "text": {"*": body}}}));
        }
        assert!(request.path.contains("srsearch=home+manager"), "{}", request.path);
        json_reply(json!({"query": {"search": [
            {"title": "Home Manager", "snippet": "<span>a</span> b", "wordcount": 1234}
        ]}}))
    });
    let app = fixture.app();
    let text = call(&app, json!({"action": "search", "source": "wiki", "query": "home-manager"})).await;
    assert_eq!(
        text,
        "Found 1 wiki articles matching 'home-manager':\n\n* Home Manager\n  \
         https://wiki.nixos.org/wiki/Home_Manager\n  a b\n  (1,234 words)"
    );
    let info = call(&app, json!({"action": "info", "source": "wiki", "query": "x"})).await;
    assert!(info.starts_with("Wiki: Home Manager\nURL: https://wiki.nixos.org/wiki/Home_Manager\n\nw w"), "{info}");
    assert!(info.ends_with("...") && !info.contains("stub"), "{info}");
    let missing = call(&app, json!({"action": "info", "source": "wiki", "query": "Missing"})).await;
    assert_eq!(missing, "Error (NOT_FOUND): Wiki page 'Missing' not found");
    let broken = call(&app, json!({"action": "info", "source": "wiki", "query": "Broken"})).await;
    assert_eq!(broken, "Error (API_ERROR): Wiki API error: boom");
}

#[tokio::test]
async fn nixdev_search_and_page_fetch() {
    let fixture = serve(|request| {
        if request.path.ends_with("searchindex.js") {
            return ok(
                r#"Search.setIndex({"docnames":["tutorials/flakes","reference"],"titles":["Flakes","Reference"],
                "terms":{"flake":[0,1],"flakeref":1},"titleterms":{"flake":0}})"#,
            );
        }
        if request.path.contains("big.md") {
            return ok(format!("# Big\n{}", "é".repeat(150_000)));
        }
        if request.path.contains("tutorials/x.md") {
            return ok("intro\n# The Title\nbody\n");
        }
        status(404)
    });
    let app = fixture.app();
    let text = call(&app, json!({"action": "search", "source": "nix-dev", "query": "flakes"})).await;
    let first = text.lines().nth(2);
    assert_eq!(first, Some("* Flakes"), "{text}");
    let page =
        call(&app, json!({"action": "info", "source": "nix-dev", "query": "https://nix.dev/tutorials/x.html"})).await;
    assert_eq!(
        page,
        "Title: The Title\nSource: https://nix.dev/tutorials/x.html\nDocname: tutorials/x\n\nintro\n# The Title\nbody"
    );
    let big = call(&app, json!({"action": "info", "source": "nix-dev", "query": "big"})).await;
    assert!(big.ends_with("\n\n[truncated]") && !big.contains('\u{fffd}'), "truncation must not split a codepoint");
    let missing = call(&app, json!({"action": "info", "source": "nix-dev", "query": "nope"})).await;
    assert_eq!(missing, "Error (NOT_FOUND): nix.dev page not found: nope");
    for traversal in ["../x", "%2e%2e/x", "a/../../x"] {
        let text = call(&app, json!({"action": "info", "source": "nix-dev", "query": traversal})).await;
        assert_eq!(text, "Error (ERROR): Invalid docname: path traversal not allowed");
    }
}

#[tokio::test]
async fn flakes_group_by_repository() {
    let fixture = with_es(|request| {
        assert!(request.path.contains("latest-51-group-manual"), "{}", request.path);
        json_reply(json!({"hits": {"total": {"value": 9}, "hits": [
            {"_source": {"flake_name": "a", "package_attr_name": "x", "flake_description": "D",
                         "flake_resolved": {"owner": "o", "repo": "r"}}},
            {"_source": {"flake_name": "a", "package_attr_name": "y", "flake_resolved": {"owner": "o", "repo": "r"}}}
        ]}}))
    });
    let text = call(&fixture.app(), json!({"action": "search", "source": "flakes", "query": "a"})).await;
    assert_eq!(text, "Found 9 matches (1 unique flakes) for 'a':\n\n* a\n  Repository: o/r\n  D\n  Packages: x, y");
}

#[tokio::test]
async fn flakehub_search_info_and_stats() {
    let fixture = serve(|request| {
        if request.path.contains("/search") {
            return json_reply(json!([{"org": "NixOS", "project": "nixpkgs",
                "description": "Nix   Packages\ncollection", "labels": ["nix"]}]));
        }
        if request.path.contains("/version/NixOS/nixpkgs/") {
            return json_reply(json!({"simplified_version": "0.1", "commit_count": 12345,
                "published_at": "2026-01-02T03:04:05Z", "mirrored": true}));
        }
        if request.path.contains("/flakes") {
            return json_reply(json!([{"org": "a", "labels": ["l"]}, {"org": "a"}, {"org": "b"}]));
        }
        status(404)
    });
    let app = fixture.app();
    let text = call(&app, json!({"action": "search", "source": "flakehub", "query": "nix"})).await;
    assert!(text.contains("* NixOS/nixpkgs\n  Nix Packages collection\n  Labels: nix\n"), "{text}");
    let info = call(&app, json!({"action": "info", "source": "flakehub", "query": "NixOS/nixpkgs"})).await;
    assert_eq!(
        info,
        "FlakeHub Flake: NixOS/nixpkgs\nLatest Version: 0.1\nCommits: 12,345\nPublished: 2026-01-02 03:04 UTC\n\
         Source: Mirrored from GitHub\nFlakeHub URL: https://flakehub.com/flake/NixOS/nixpkgs"
    );
    let missing = call(&app, json!({"action": "info", "source": "flakehub", "query": "no/one"})).await;
    assert_eq!(missing, "Error (NOT_FOUND): Flake 'no/one' not found on FlakeHub");
    let stats = call(&app, json!({"action": "stats", "source": "flakehub"})).await;
    assert!(stats.contains("* Total flakes: 3\n* Organizations: 2\n* Top organizations:\n  - a: 2 flakes"), "{stats}");
}

fn resolve_reply() -> Reply {
    let hash = "a".repeat(32);
    json_reply(json!({"name": "hello", "version": "2.12", "systems": {
        "x86_64-linux": {
            "flake_installable": {"attr_path": "hello",
                "ref": {"type": "github", "owner": "NixOS", "repo": "nixpkgs", "rev": "0123456789abcdef"}},
            "outputs": [{"path": format!("/nix/store/{hash}-hello"), "default": true}]},
        "aarch64-darwin": {"outputs": []}
    }}))
}

#[tokio::test]
async fn binary_cache_reports_per_system_status() {
    let fixture = serve(|request| {
        if request.path.contains("/v2/resolve") {
            return if request.path.contains("nope") { status(404) } else { resolve_reply() };
        }
        if request.path.ends_with(".narinfo") {
            return if request.method == "HEAD" {
                status(200)
            } else {
                ok("FileSize: 2048\nNarSize: 4096\nCompression: zstd\n")
            };
        }
        status(404)
    });
    let app = fixture.app();
    let hash = "a".repeat(32);
    let text = call(&app, json!({"action": "cache", "query": "hello"})).await;
    assert_eq!(
        text,
        format!(
            "Binary Cache Status: hello@2.12\n\nSystem: x86_64-linux\n  Store path: /nix/store/{hash}-hello\n  \
             Status: CACHED\n  Download size: 2.0 KB\n  Unpacked size: 4.0 KB\n  Compression: zstd\n\n\
             System: aarch64-darwin\n  Store path: Not available\n  Status: UNKNOWN"
        )
    );
    let filtered = call(&app, json!({"action": "cache", "query": "hello", "system": "riscv"})).await;
    assert_eq!(filtered, "Error (NOT_FOUND): System 'riscv' not available. Available: aarch64-darwin, x86_64-linux");
    let missing = call(&app, json!({"action": "cache", "query": "nope"})).await;
    assert_eq!(missing, "Error (NOT_FOUND): Package 'nope' not found");
}

#[tokio::test]
async fn binary_cache_treats_404_as_not_cached() {
    let fixture = serve(|request| if request.path.contains("/v2/resolve") { resolve_reply() } else { status(404) });
    let text = call(&fixture.app(), json!({"action": "cache", "query": "hello", "system": "x86_64-linux"})).await;
    assert!(text.ends_with("Status: NOT CACHED"), "{text}");
}

fn nixhub_pkg() -> Reply {
    let commit = "f".repeat(40);
    json_reply(json!([
        {"name": "hello", "version": "2.12", "summary": "Greets", "license": "GPL", "homepage": "https://h",
         "platforms": ["x86_64-linux"], "commit_hash": commit, "last_updated": "2026-01-02T00:00:00Z",
         "systems": {"x86_64-linux": {"programs": ["hello"], "attr_paths": ["hello"]}}},
        {"name": "hello", "version": "2.11"}
    ]))
}

#[tokio::test]
async fn nixhub_info_merges_pkg_and_resolve() {
    let fixture = serve(|request| {
        if request.path.contains("/v1/pkg") {
            nixhub_pkg()
        } else if request.path.contains("/v2/resolve") {
            resolve_reply()
        } else {
            status(404)
        }
    });
    let text = call(&fixture.app(), json!({"action": "info", "source": "nixhub", "query": "hello"})).await;
    assert!(text.starts_with("Package: hello\nVersion: 2.12\nSummary: Greets\n\nLicense: GPL\nHomepage: https://h\n"));
    assert!(text.contains("Flake Reference:\n  github:NixOS/nixpkgs/01234567#hello"), "{text}");
    assert!(text.contains("Store Paths:\n  x86_64-linux: /nix/store/"), "{text}");
}

#[tokio::test]
async fn nixhub_status_codes_map_to_plain_errors() {
    let fixture = serve(|request| if request.path.contains("name=gone") { status(404) } else { status(503) });
    let app = fixture.app();
    let missing = call(&app, json!({"action": "info", "source": "nixhub", "query": "gone"})).await;
    assert_eq!(missing, "Error (NOT_FOUND): Package 'gone' not found");
    let down = call(&app, json!({"action": "search", "source": "nixhub", "query": "x"})).await;
    assert_eq!(down, "Error (SERVICE_ERROR): NixHub API temporarily unavailable");
}

#[tokio::test]
async fn nix_versions_lists_and_finds_versions() {
    use mcp_nixos::server::{NixVersionsParams, nix_versions};
    let fixture = serve(|_| nixhub_pkg());
    let app = fixture.app();
    let versions = |args: Value| {
        let params: NixVersionsParams = serde_json::from_value(args).unwrap();
        nix_versions(&app, params)
    };
    let text = versions(json!({"package": "hello", "limit": 1})).await;
    assert!(text.starts_with("Package: hello\nLicense: GPL\nHomepage: https://h\nPrograms: hello\nTotal versions: 2"));
    assert!(text.contains("Recent versions (1 of 2):\n\n* 2.12\n  Updated: 2026-01-02\n  Platforms: Linux"), "{text}");
    let found = versions(json!({"package": "hello", "version": "2.11"})).await;
    assert_eq!(found, "Found hello version 2.11\n\n* 2.11");
    let absent = versions(json!({"package": "hello", "version": "9"})).await;
    assert_eq!(absent, "Version 9 not found for hello\nAvailable: 2.12, 2.11");
    assert_eq!(versions(json!({"package": " "})).await, "Error (ERROR): Package name required");
    assert_eq!(versions(json!({"package": "a b"})).await, "Error (ERROR): Invalid package name");
    assert_eq!(versions(json!({"package": "a", "limit": 51})).await, "Error (ERROR): Limit must be 1-50");
}

#[tokio::test]
async fn routing_rejects_bad_arguments_with_guidance() {
    let fixture = serve(|_| status(500));
    let app = fixture.app();
    for (args, expected) in [
        (json!({"action": "search"}), "Query required for search"),
        (json!({"action": "info"}), "Name required for info"),
        (json!({"action": "bogus"}), "Unknown action: 'bogus'"),
        (json!({"action": "search", "query": "x", "source": "bogus"}), "Unknown source: 'bogus'"),
        (json!({"action": "search", "query": "x", "limit": 0}), "Limit must be 1-100"),
        (json!({"action": "search", "query": "x", "limit": 101}), "Limit must be 1-100"),
        (json!({"action": "search", "query": "x", "type": "bogus"}), "type must be one of"),
        (json!({"action": "browse", "source": "nixos"}), "action=browse is not for NixOS"),
        (json!({"action": "options", "source": "nixos"}), "action=browse is not for NixOS"),
        (json!({"action": "browse", "source": "wiki"}), "home-manager, darwin, nixvim, nvf, noogle"),
        (json!({"action": "stats", "source": "wiki"}), "Stats not available for source=wiki."),
        (json!({"action": "info", "source": "flakes", "query": "x"}), "not supported for source=flakes"),
        (json!({"action": "cache"}), "Package name required for cache action"),
        (json!({"action": "store", "type": "read", "query": "/nix/store/x", "limit": 2001}), "Limit must be 1-2000"),
        (json!({"action": "store", "type": "bogus", "query": "x"}), "Type must be one of: ls, read for store"),
        (json!({"action": "flake-inputs", "type": "bogus"}), "Type must be one of: list, ls, read"),
    ] {
        let text = call(&app, args.clone()).await;
        assert!(text.starts_with("Error (") && text.contains(expected), "{args}: {text}");
    }
}

#[tokio::test]
async fn output_is_plain_text() {
    let fixture = with_es(|_| es_hits(vec![json!({"package_pname": "a", "package_pversion": "1"})]));
    let text = call(&fixture.app(), json!({"action": "search", "query": "a"})).await;
    assert!(!text.starts_with('{') && !text.contains("<?xml"), "{text}");
}
