# GitHub Copilot Instructions for MCP-NixOS

## Project Context

MCP-NixOS is a Model Context Protocol (MCP) server providing real-time information about NixOS packages, options, Home Manager, nix-darwin, Nixvim, NVF, flakes, and related documentation. It stops AI assistants from hallucinating package names by querying official APIs.

**Architecture:** Async Rust server on the official `rmcp` SDK, speaking MCP revision 2026-07-28 over stdio only, exposing 2 MCP tools: `nix` (unified query) and `nix_versions` (version history). See [AGENTS.md](../AGENTS.md) for the full guide.

**Key Modules:**
- `src/server.rs` - MCP surface, routing, cancellation
- `src/sources/` - Per-source modules
- `src/config.rs` - Configuration constants
- `src/http.rs` - Bounded upstream HTTP
- `src/caches.rs` - Process-local caches
- `src/html.rs`, `src/text.rs` - Parsing and formatting helpers

## Core Development Workflows

```bash
nix develop              # Enter dev shell
run                      # Start MCP server
run-tests                # cargo test (offline)
lint                     # clippy and rustfmt check
format                   # cargo fmt
build                    # cargo build --release
```

## Project-Specific Patterns

1. **Plain text output only.** Tool responses are formatted for LLM consumption; never return raw JSON or XML.
2. **Channel resolution.** Channels are discovered from `_cat/aliases`. `stable` maps to the newest release; fallback mappings are never memoized.
3. **Errors.** Use `text::error(msg, code)` or `text::err(msg)`. Errors are plain text, human-readable, and actionable.
4. **Output compatibility.** Lengths count characters, and case-insensitive ordering uses `text::casefold`.
5. **Tests are offline.** `tests/common/mod.rs` stands in for every upstream through `MCP_NIXOS_UPSTREAM`.

## CI/CD & Release Process

- **CI runs on all PRs:** flake check, Nix build, clippy, rustfmt, tests
- **Releases:** no registry publishing; consumers pin the flake from `github:timfewi/mcp-nixos-rust`
- **Commit convention:** `type: summary` format (feat:, fix:, docs:, refactor:, test:, chore:)

## Common Pitfalls

1. **Flake inputs security:** Always validate paths stay within `/nix/store/` (`sources::store::validate_store_path`)
2. **Line limits:** flake-inputs and store read allow 1-2000, all other actions 1-100
3. **Generated artifacts:** Never edit `target/` or `result/`
4. **Never bypass linting:** Do not add `#[allow]` for a lint you could fix; `unsafe` is forbidden in this crate
