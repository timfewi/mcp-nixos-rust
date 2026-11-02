# CLAUDE.md

This file provides guidance to Claude Code (claude.ai/code) when working with code in this repository.

## Project Overview

MCP-NixOS is a Model Context Protocol (MCP) server that provides accurate, real-time information about NixOS packages, configuration options, Home Manager, nix-darwin, Nixvim, NVF, and flakes. It prevents AI assistants from hallucinating about NixOS package names and configurations by querying official APIs and documentation.

## Project Structure & Module Organization

- `src/` - The MCP server (library plus `mcp-nixos` binary).
  - `src/main.rs` - Entry point; serves stdio until stdin closes.
  - `src/server.rs` - MCP surface: tool definitions, routing, protocol versions, cancellation.
  - `src/config.rs` - Configuration constants (API URLs, auth, limits).
  - `src/http.rs` - Bounded upstream HTTP: size cap, timeouts, TLS roots, `requests`-style status errors.
  - `src/caches.rs` - Process-local caches (channels, Home Manager, nix-darwin, Nixvim, NVF, Noogle, nix.dev).
  - `src/html.rs` - HTML text extraction and option catalogue parsing.
  - `src/text.rs` - Plain-text formatting helpers.
  - `src/nix_schema.json`, `src/nix_versions_schema.json`, `src/nix_tool.md`, `src/instructions.md` - The tool surface as data.
  - `src/sources/` - Data source implementations (one module per source):
    - `base.rs` - Channel helpers, Elasticsearch queries, HTML option search/info/stats/browse (Home Manager, nix-darwin).
    - `nixos.rs` - NixOS packages/options search, info, stats.
    - `flakes.rs` - Flake search from search.nixos.org.
    - `flakehub.rs` - FlakeHub API (Determinate Systems).
    - `wiki.rs` - NixOS Wiki search.
    - `nixdev.rs` - nix.dev documentation.
    - `nixvim.rs` - Nixvim options.
    - `nvf.rs` - NVF options and path normalization.
    - `noogle.rs` - Noogle function search.
    - `nixhub.rs` - NixHub API + binary cache status.
    - `flake_inputs.rs` - Local flake inputs via `nix flake archive`.
    - `store.rs` - Direct `/nix/store` ls/read.
- `tests/` - Offline integration tests. `common/mod.rs` is an HTTP fixture standing in for every upstream; `channels.rs` and `sources.rs` drive the tools, `protocol.rs` drives the real binary over stdio. Unit tests sit next to the code.
- `website/` - The VitePress docs/marketing site (`index.md`, `usage.md`, `about.md`, custom theme under `.vitepress/`); static assets live in `website/public/`. Output builds to `website/out/` and deploys to S3/CloudFront via `deploy-website.yml`.
- `.pi/` - Pi Coding Agent extension that wraps the MCP tools as native Pi tools (Pi does not speak MCP). `extensions/mcp-nixos.ts` spawns the server and makes one stateless `tools/call` per invocation; only relevant when running `pi` in this repo.
- `flake.nix` - Defines the Nix dev shell, package, overlay, and checks.
- `Cargo.toml` - Defines Rust packaging and dependencies.
- `target/` and `result/` are generated artifacts; do not edit by hand.

## Key Architecture

The project is an async Rust server on the official `rmcp` SDK, speaking MCP revisions **2026-07-28** and **2025-11-25** over **stdio only**. `server.rs` handles the protocol surface and routing, `sources/` contains per-source implementations, `http.rs` is the single path to upstreams, and `caches.rs` manages cached data.

Only **2 MCP tools** are exposed (consolidated from 17 in v1.0):
- `nix` - Unified query tool for search/info/stats/browse/channels/flake-inputs/cache/store across all sources. `options` is accepted as a silent alias for `browse` (legacy).
- `nix_versions` - Package version history from NixHub.io.

### Data Sources

- NixOS packages/options: Elasticsearch API at search.nixos.org
- Home Manager options: HTML parsing from official docs
- nix-darwin options: HTML parsing from official docs
- Nixvim options: NuschtOS search metadata
- NVF options: HTML parsing from the latest published unstable options documentation
- Package versions: NixHub.io API (search.devbox.sh)
- Package metadata: NixHub.io API for license, homepage, store paths
- Binary cache status: cache.nixos.org narinfo queries
- Flakes: search.nixos.org flake index
- Local flake inputs: Direct access to /nix/store via `nix flake archive`

All responses are formatted as plain text for optimal LLM consumption.

## Build, Test, and Development Commands

```bash
# Enter dev shell
nix develop

# Core commands (available via menu):
run           # Start the MCP server
run-tests     # cargo test (offline)
lint          # clippy and rustfmt check
format        # cargo fmt
ci-local      # lint plus tests, as CI runs them
build         # cargo build --release
build-nix     # nix build, including the test suite
```

## Testing Guidelines

- `cargo test` is fully offline: upstreams are replaced with `tests/common/mod.rs` through `MCP_NIXOS_UPSTREAM`.
- Add a regression test next to the code for pure logic, in `tests/sources.rs` or `tests/channels.rs` for source behavior, and in `tests/protocol.rs` for anything a client can observe on the wire.
- Tests ensure plain text output (no XML/JSON leakage).

```bash
cargo test --test sources                 # One test file
cargo test noogle                         # Tests matching a pattern
```

## Coding Style & Naming Conventions

- Rust 2024; max line width 120 (`rustfmt.toml`).
- `unsafe` is forbidden in this crate. `unwrap` is denied in library code; static patterns use `expect` with a local `allow`.
- Keep MCP responses plain text (no raw JSON) to match server behavior.
- Tool output is a compatibility surface: lengths count characters, not bytes, and case-insensitive ordering uses full case folding (`text::casefold`).
- **Never bypass linting**: Do not use `#[allow]` to silence a lint you could fix. If a lint is genuinely wrong for the project, change `[lints]` in `Cargo.toml`.

## Installation & Configuration

### As a Nix Package

```nix
# In your flake.nix
{
  inputs.mcp-nixos.url = "github:utensils/mcp-nixos";

  outputs = { nixpkgs, mcp-nixos, ... }: {
    # Use the overlay to add pkgs.mcp-nixos
    nixpkgs.overlays = [ mcp-nixos.overlays.default ];

    # Then use in your config:
    # environment.systemPackages = [ pkgs.mcp-nixos ];  # NixOS
    # home.packages = [ pkgs.mcp-nixos ];               # Home Manager
  };
}
```

### MCP Client Configuration (Claude Desktop, etc.)

```json
{
  "mcpServers": {
    "nixos": {
      "type": "stdio",
      "command": "nix",
      "args": ["run", "github:utensils/mcp-nixos"]
    }
  }
}
```

## Important Implementation Notes

1. **Channel Resolution**: The server dynamically discovers available NixOS channels. "stable" always maps to the current stable release. Fallback mappings are never memoized.
2. **Error Handling**: All tools return helpful plain text error messages. API failures gracefully degrade.
3. **Process-local Caching**: There is no persistent cache or database. Catalogue-style sources such as Home Manager, nix-darwin, Nixvim, NVF, Noogle, and nix.dev are cached in memory for the lifetime of the server process; other queries hit live APIs. These hold upstream data, never client state.
4. **Protocol Versions**: Native 2026-07-28 requests carry their own metadata. The SDK also negotiates 2025-11-25 sessions for existing Codex, Claude, OMP and MemoryCreep clients. Both use the same handlers; do not add a separate tool loop or translation process. Remove the older revision only after those named consumers migrate and their protocol checks pass.
5. **Plain Text Output**: All responses are formatted as human-readable plain text. Never return raw JSON or XML to users.
6. **Bounded Work**: Response bodies are capped in `http.rs`. Tool calls run in their own task, so `notifications/cancelled` aborts in-flight HTTP and `nix` children. HTML parsing and file I/O run off the async workers.
7. **Environment Variables**: `MCP_NIXOS_UPSTREAM` redirects all upstream traffic to `<url>/<host>/<path>`, for offline testing.
8. **TLS Roots**: System CAs are preferred; a bundled Mozilla set is the fallback when the host has none.
9. **Flake Inputs**: The `flake-inputs` action requires nix to be installed locally. It uses `nix flake archive --json` to discover inputs and their store paths, with security validation to ensure paths stay within `/nix/store/`.
10. **Binary Cache Status**: The `cache` action queries cache.nixos.org to check if packages have pre-built binaries. It uses NixHub to resolve package versions to store paths, then checks narinfo availability.
11. **NixHub Source**: The `nixhub` source provides rich package metadata including license, homepage, programs, and store paths via the search.devbox.sh API.
12. **NVF Source**: The `nvf` source tracks NVF's latest published unstable options. Its canonical names are `vim.*`; callers may also use `programs.nvf.vim.*` or `programs.nvf.settings.vim.*`, which normalize to the canonical path before search, info, or browse operations.

## Commit, PR, & Release Guidelines

- Commit messages follow `type: summary` (e.g., `feat:`, `fix:`, `docs:`, `refactor:`, `test:`, `chore:`).
- **CI**: Runs on all PRs - flake check, Nix build, clippy, rustfmt, tests.
- **Publish**: Release Please creates tags and GitHub Releases; the authenticated release event then publishes multi-arch Docker images to GHCR and Docker Hub, and the tagged flake to FlakeHub.
- **Release Process**: Use the `/release` command to review and merge the generated `release: vX.Y.Z` PR and verify every public registry. Do not bump versions or create tags manually.
- Release PRs update `Cargo.toml`, `Cargo.lock`, `.release-please-manifest.json`, and `RELEASE_NOTES.md`; their squash merge message is `release: vX.Y.Z`.
