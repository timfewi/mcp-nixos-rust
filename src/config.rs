//! Configuration constants for the MCP-NixOS server.

pub const VERSION: &str = env!("CARGO_PKG_VERSION");
pub const USER_AGENT: &str = concat!("mcp-nixos/", env!("CARGO_PKG_VERSION"));

pub const NIXOS_API: &str = "https://search.nixos.org/backend";
pub const NIXOS_AUTH: (&str, &str) = ("aWVSALXpZv", "X8gPHnzL52wFEekuxsfQ9cSh");

/// Last-resort channel map, used only when alias discovery fails outright.
/// Hydra retires old `latest-<gen>-nixos-*` aliases, so the cache never memoizes
/// a fallback result: this map only has to cover a single request during an
/// upstream blip.
pub const FALLBACK_CHANNELS: [(&str, &str); 5] = [
    ("unstable", "latest-50-nixos-unstable"),
    ("stable", "latest-50-nixos-26.05"),
    ("26.05", "latest-50-nixos-26.05"),
    ("25.11", "latest-48-nixos-25.11"),
    ("beta", "latest-50-nixos-26.05"),
];

/// The print view keeps the complete option catalogue in one document.
pub const HOME_MANAGER_URL: &str = "https://nix-community.github.io/home-manager/print.html";
pub const DARWIN_URL: &str = "https://nix-darwin.github.io/nix-darwin/manual/index.html";
/// Fallback only: the live `latest-<gen>-group-manual` alias is discovered at runtime.
pub const FLAKE_INDEX: &str = "latest-51-group-manual";

pub const FLAKEHUB_API: &str = "https://api.flakehub.com";
pub const GITHUB_API: &str = "https://api.github.com";

/// `chunks/N.json` holds ~300 options each; we walk chunks until a 404.
pub const NIXVIM_OPTIONS_CHUNKS_BASE: &str = "https://nix-community.github.io/nixvim/search/data/options/chunks";
pub const NVF_OPTIONS_URL: &str = "https://nvf.notashelf.dev/options.html";
pub const WIKI_API: &str = "https://wiki.nixos.org/w/api.php";
pub const NIXDEV_SEARCH_INDEX: &str = "https://nix.dev/searchindex.js";
pub const NIXDEV_BASE_URL: &str = "https://nix.dev";
pub const NOOGLE_API: &str = "https://noogle.dev/api/v1/data";
pub const NIXHUB_API: &str = "https://search.devbox.sh";
pub const CACHE_NIXOS_ORG: &str = "https://cache.nixos.org";

/// Largest file `store` and `flake-inputs` will read.
pub const MAX_FILE_SIZE: u64 = 1024 * 1024;
pub const DEFAULT_LINE_LIMIT: i64 = 500;
pub const MAX_LINE_LIMIT: i64 = 2000;

/// Upper bound on any upstream response body. The Noogle catalogue is the
/// largest legitimate payload; this exists so a misbehaving upstream cannot
/// exhaust memory.
pub const MAX_RESPONSE_BYTES: usize = 128 * 1024 * 1024;

/// Known sources, to distinguish them from flake paths.
pub const KNOWN_SOURCES: [&str; 11] =
    ["nixos", "home-manager", "darwin", "flakes", "flakehub", "nixvim", "nvf", "wiki", "nix-dev", "noogle", "nixhub"];
