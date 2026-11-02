# MCP-NixOS - Because Your AI Shouldn't Hallucinate Package Names

[![Rust](https://img.shields.io/badge/rust-2024-blue.svg)](https://www.rust-lang.org/)
[![Built with Claude](https://img.shields.io/badge/Built%20with-Claude-D97757?logo=claude&logoColor=white)](https://claude.ai)

## Quick Start

**🚨 No Nix/NixOS Required!** One binary, no language runtime. You're just querying APIs.

MCP-NixOS speaks MCP revision **2026-07-28** over **stdio**, with self-contained
requests and no `initialize` handshake. Clients that still use the session-based
**2025-11-25** revision negotiate it through the SDK's standard handshake. Both
protocols use the same Rust tool implementation; no Python runtime or bridge is required.

### Option 1: Nix

[![Install MCP Server](https://cursor.com/deeplink/mcp-install-dark.svg)](https://cursor.com/install-mcp?name=nixos&config=eyJjb21tYW5kIjoibml4IHJ1biBnaXRodWI6dGltZmV3aS9tY3Atbml4b3MtcnVzdCAtLSJ9)

```json
{
  "mcpServers": {
    "nixos": {
      "command": "nix",
      "args": ["run", "github:timfewi/mcp-nixos-rust", "--"]
    }
  }
}
```

Your AI now has access to real NixOS data instead of making things up. You're welcome.

### Option 2: Cargo

```bash
cargo install --git https://github.com/timfewi/mcp-nixos-rust
```

```json
{
  "mcpServers": {
    "nixos": {
      "command": "mcp-nixos"
    }
  }
}
```

### Option 3: Pi Coding Agent

[Pi](https://www.npmjs.com/package/@mariozechner/pi-coding-agent) does not speak MCP natively. Two supported paths:

**A. pi-mcp-adapter (recommended — speaks MCP, single source of truth):**

```bash
pi install npm:pi-mcp-adapter
```

Then add to `~/.pi/agent/mcp.json`:

```json
{
  "mcpServers": {
    "nixos": {
      "command": "mcp-nixos",
      "lifecycle": "lazy"
    }
  }
}
```

**B. Project-local extension (clone + run):** this repo ships `.pi/extensions/mcp-nixos.ts`, auto-loaded when you run `pi` in the cloned repo. It talks to `target/release/mcp-nixos`, `mcp-nixos` on `PATH`, or `nix run`. Optional: `cd .pi && npm install` for editor type resolution. Pi runs it either way.

## What Is This?

An MCP server providing accurate, real-time information about:

- **NixOS packages** - 130K+ packages that actually exist
- **NixOS options** - 23K+ ways to configure your system
- **Home Manager** - 5K+ options for dotfile enthusiasts
- **nix-darwin** - 1K+ macOS settings Apple doesn't document
- **Nixvim** - 5K+ options for Neovim configuration via [NuschtOS search](https://github.com/NuschtOS/search)
- **NVF** - 2.4K+ Neovim options from its [published unstable documentation](https://nvf.notashelf.dev/options.html)
- **FlakeHub** - 600+ flakes from [FlakeHub.com](https://flakehub.com) registry
- **Noogle** - 2K+ Nix functions with type signatures via [noogle.dev](https://noogle.dev)
- **NixOS Wiki** - Community documentation and guides from [wiki.nixos.org](https://wiki.nixos.org)
- **nix.dev** - Official Nix tutorials and guides from [nix.dev](https://nix.dev)
- **Package versions** - Historical versions with commit hashes via [NixHub.io](https://www.nixhub.io)
- **Binary cache status** - Check if packages are cached on cache.nixos.org with download sizes
- **Local flake inputs** - Explore your pinned flake dependencies directly from the Nix store (requires Nix)

## Why Rust?

Version 3.1 replaces the Python/FastMCP server with a native Rust server on the
official `rmcp` SDK. The tool surface (`nix`, `nix_versions`) and its answers
stay the same; what changes is everything around them:

- **Startup is no longer the bottleneck.** Most MCP clients spawn one server per
  session. The Python server spent almost a second importing its runtime before
  it could answer `initialize`; the Rust binary answers in a few milliseconds,
  so the tool is ready before the model's first turn.
- **One binary instead of a Python environment.** No interpreter, virtualenv or
  pinned FastMCP/MCP SDK pair to keep compatible. The FastMCP 3 → 4 and MCP SDK
  1 → 2 transitions changed the wire protocol underneath the server; in Rust both
  protocol revisions are served by one SDK and one set of handlers.
- **Lower memory.** A coding session keeps the server alive for its whole
  lifetime; after the handshake it now needs about a tenth of the former memory.
- **Fewer failure modes at launch.** Problems such as the former import-time
  `.env` parsing crash (2.4.2) cannot occur: nothing is read from the working
  directory at startup.
- **Bounded, offline-testable I/O.** All upstream traffic goes through one HTTP
  path with size caps and timeouts, and the full test suite runs offline against
  local fixtures.

### Benchmark

Spawn the server, send `initialize` (2025-11-25), `notifications/initialized` and
`tools/list`, then close stdin. 20 sequential runs per build after one warm-up,
x86_64-linux, 8 CPUs, no network access needed. Medians:

| Build | `initialize` ready | `tools/list` | RSS after handshake | Runtime closure |
| ----- | ------------------ | ------------ | ------------------- | --------------- |
| 3.0.2 Python / FastMCP 3 | 920 ms (863–955) | 2.66 ms | 92 MiB | 328 MiB, 107 store paths |
| 3.1.0 Rust / rmcp | **2.5 ms** (2.2–3.2) | **0.24 ms** | **8.6 MiB** | **60 MiB**, 7 store paths |
| Improvement | ~370× faster | ~11× faster | ~11× less | ~5.5× smaller |

The Rust closure is dominated by glibc and the GCC runtime library shared with
the rest of the system; the binary itself is 14 MiB. Query latency for real
lookups is bounded by the upstream APIs (search.nixos.org, NixHub, FlakeHub …)
and is not part of this measurement.

## The Tools

Just two. We consolidated 17 tools into 2 because your AI's context window isn't infinite.

**~1,030 tokens total.** That's it. While other MCP servers are hogging your context like it's Black Friday, we're sipping minimalist tea in the corner. Your AI gets NixOS superpowers without the bloat.

### `nix` - Unified Query Tool

One tool to rule them all:

```text
nix(action, query, source, type, channel, limit, version, system)
```

`version` and `system` are only used by `action="cache"`.

| Action | What it does |
|--------|-------------|
| `search` | Search packages, options, programs, or flakes |
| `info` | Get detailed info about a package or option |
| `stats` | Get counts and categories |
| `browse` | Browse Home Manager, Darwin, Nixvim, NVF, or Noogle by prefix (legacy alias: `options`) |
| `channels` | List available NixOS channels |
| `flake-inputs` | Explore local flake inputs from Nix store |
| `cache` | Check binary cache status for packages |
| `store` | Read files or list directories at an explicit `/nix/store` path (requires Nix) |

| Source | What it queries |
|--------|----------------|
| `nixos` | Packages, options, programs |
| `home-manager` | Home Manager options |
| `darwin` | nix-darwin options |
| `flakes` | Community flakes (search.nixos.org) |
| `flakehub` | FlakeHub registry (flakehub.com) |
| `nixvim` | Nixvim Neovim configuration options |
| `nvf` | NVF Neovim configuration options (latest unstable docs) |
| `noogle` | Nix function signatures and docs (noogle.dev) |
| `wiki` | NixOS Wiki articles (wiki.nixos.org) |
| `nix-dev` | Official Nix documentation (nix.dev) |
| `nixhub` | Package metadata and store paths (nixhub.io) |

NVF results use canonical `vim.*` option paths. Queries may also use the shorthand
`programs.nvf.vim.*` or the NixOS/Home Manager module path
`programs.nvf.settings.vim.*`; both are normalized automatically.

**Examples:**

```text
# Search NixOS packages
nix(action="search", query="firefox", source="nixos", type="packages")

# Get package info
nix(action="info", query="firefox", source="nixos", type="package")

# Search Home Manager options
nix(action="search", query="git", source="home-manager")

# Browse darwin options
nix(action="browse", source="darwin", query="system.defaults")

# Search Nixvim options
nix(action="search", query="telescope", source="nixvim")

# Get Nixvim option info
nix(action="info", query="plugins.telescope.enable", source="nixvim")

# Search NVF options
nix(action="search", query="vim.languages.nix", source="nvf")

# Get NVF option info using a module wrapper path
nix(action="info", query="programs.nvf.settings.vim.languages.nix.enable", source="nvf")

# Browse NVF options below a prefix
nix(action="browse", query="programs.nvf.vim.languages.nix", source="nvf")

# Search FlakeHub
nix(action="search", query="nixpkgs", source="flakehub")

# Get FlakeHub flake info
nix(action="info", query="NixOS/nixpkgs", source="flakehub")

# Search Noogle for Nix functions
nix(action="search", query="mapAttrs", source="noogle")

# Get Noogle function info
nix(action="info", query="lib.attrsets.mapAttrs", source="noogle")

# Browse Noogle function categories
nix(action="browse", source="noogle", query="lib.strings")

# Search NixOS Wiki
nix(action="search", query="nvidia", source="wiki")

# Get Wiki page info
nix(action="info", query="Flakes", source="wiki")

# Search nix.dev documentation
nix(action="search", query="packaging tutorial", source="nix-dev")

# Search NixHub for package metadata
nix(action="search", query="nodejs", source="nixhub")

# Get detailed package info from NixHub (license, homepage, store paths)
nix(action="info", query="python", source="nixhub")

# Check binary cache status
nix(action="cache", query="hello")

# Check cache for specific version
nix(action="cache", query="python", version="3.12.0")

# Check cache for specific system
nix(action="cache", query="firefox", system="x86_64-linux")

# Get stats
nix(action="stats", source="nixos", channel="stable")

# List local flake inputs (requires Nix)
nix(action="flake-inputs", type="list")

# Browse files in a flake input
nix(action="flake-inputs", type="ls", query="nixpkgs:pkgs/by-name")

# Read a file from a flake input
nix(action="flake-inputs", type="read", query="nixpkgs:flake.nix")

# List or read a store path directly (requires Nix)
nix(action="store", type="ls", query="/nix/store/<hash>-<name>")
nix(action="store", type="read", query="/nix/store/<hash>-<name>/flake.nix")
```

### `nix_versions` - Package Version History

Find historical versions with nixpkgs commit hashes. Output includes:
- Package metadata (license, homepage, programs) when available
- Platform availability per version (Linux/macOS)
- Nixpkgs commit hash for reproducible builds
- Attribute path for Nix expressions

```text
nix_versions(package, version, limit)
```

**Examples:**

```text
# List recent versions with metadata
nix_versions(package="python", limit=5)

# Find specific version
nix_versions(package="nodejs", version="20.0.0")
```

## Installation

**You DON'T need Nix installed.** The server is a single binary:

```bash
cargo install --git https://github.com/timfewi/mcp-nixos-rust
```

For Nix users:

```bash
nix run github:timfewi/mcp-nixos-rust
nix profile install github:timfewi/mcp-nixos-rust
```

### Declarative Installation (NixOS / Home Manager / nix-darwin)

The `mcp-nixos` package in [nixpkgs](https://search.nixos.org/packages?channel=unstable&show=mcp-nixos&query=mcp-nixos)
is the upstream Python server, not this Rust rewrite. Use it only if you want the
Python version:

```nix
# NixOS (configuration.nix)
environment.systemPackages = [ pkgs.mcp-nixos ];

# Home Manager (home.nix)
home.packages = [ pkgs.mcp-nixos ];

# nix-darwin (darwin-configuration.nix)
environment.systemPackages = [ pkgs.mcp-nixos ];
```

Or use the flake directly with the provided overlay:

```nix
# flake.nix
{
  inputs = {
    nixpkgs.url = "github:NixOS/nixpkgs/nixos-unstable";
    mcp-nixos.url = "github:timfewi/mcp-nixos-rust";
  };

  outputs = { self, nixpkgs, mcp-nixos, ... }: {
    # Example: NixOS configuration
    nixosConfigurations.myhost = nixpkgs.lib.nixosSystem {
      system = "x86_64-linux";
      modules = [{
        nixpkgs.overlays = [ mcp-nixos.overlays.default ];
        environment.systemPackages = [ pkgs.mcp-nixos ];
      }];
    };

    # Example: Home Manager standalone
    homeConfigurations.myuser = home-manager.lib.homeManagerConfiguration {
      pkgs = import nixpkgs {
        system = "x86_64-linux";
        overlays = [ mcp-nixos.overlays.default ];
      };
      modules = [{
        home.packages = [ pkgs.mcp-nixos ];
      }];
    };
  };
}
```

`overlays.default` only adds `pkgs.mcp-nixos`.

## Development

```bash
nix develop          # Enter dev shell
nix build            # Build package
cargo test           # Run tests (offline)
cargo clippy         # Lint
cargo fmt            # Format
```

## Acknowledgments

- **[utensils/mcp-nixos](https://github.com/utensils/mcp-nixos)** - The original Python server this Rust rewrite is forked from
- **[NixHub.io](https://www.nixhub.io)** - Package version history
- **[search.nixos.org](https://search.nixos.org)** - Official NixOS search
- **[FlakeHub](https://flakehub.com)** - Flake registry by Determinate Systems
- **[Jetify](https://www.jetify.com)** - Creators of Devbox and NixHub
- **[Noogle](https://noogle.dev)** - Nix function search engine
- **[NuschtOS](https://github.com/NuschtOS/search)** - Static option search infrastructure powering Nixvim support
- **[Nixvim](https://github.com/nix-community/nixvim)** - Neovim configuration framework for Nix
- **[NVF](https://github.com/NotAShelf/nvf)** - Neovim configuration framework and published option catalogue

## License

MIT - Because sharing is caring.

---

*Created by James Brink. Maintained by mass̶o̶c̶h̶i̶s̶t̶s̶ enthusiasts who enjoy Nix.*
