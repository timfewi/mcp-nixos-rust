{
  description = "MCP-NixOS - Model Context Protocol server for NixOS, Home Manager, and nix-darwin";

  inputs = {
    nixpkgs.url = "https://channels.nixos.org/nixos-unstable/nixexprs.tar.zst";
    flake-parts.url = "github:hercules-ci/flake-parts";
    devshell = {
      url = "github:numtide/devshell";
      inputs.nixpkgs.follows = "nixpkgs";
    };
  };

  outputs =
    inputs@{
      self,
      nixpkgs,
      flake-parts,
      ...
    }:
    let
      mkMcpNixos =
        { pkgs }:
        let
          manifest = pkgs.lib.importTOML ./Cargo.toml;
        in
        pkgs.rustPlatform.buildRustPackage {
          pname = manifest.package.name;
          inherit (manifest.package) version;
          src = pkgs.lib.fileset.toSource {
            root = ./.;
            fileset = pkgs.lib.fileset.unions [
              ./Cargo.toml
              ./Cargo.lock
              ./README.md
              ./src
              ./tests
            ];
          };
          cargoLock.lockFile = ./Cargo.lock;

          # aws-lc-sys (the rustls crypto provider) builds its C sources with these.
          nativeBuildInputs = [
            pkgs.cmake
            pkgs.perl
          ];
          dontUseCmakeConfigure = true;

          meta = {
            inherit (manifest.package) description;
            homepage = "https://github.com/utensils/mcp-nixos";
            license = pkgs.lib.licenses.mit;
            mainProgram = "mcp-nixos";
          };
        };
    in
    flake-parts.lib.mkFlake { inherit inputs; } {
      imports = [
        inputs.devshell.flakeModule
      ];

      systems = [
        "x86_64-linux"
        "aarch64-linux"
        "aarch64-darwin"
      ];

      flake = {
        # Downstream consumers who apply `mcp-nixos.overlays.default` get
        # `pkgs.mcp-nixos`.
        overlays.default = final: _: {
          mcp-nixos = mkMcpNixos { pkgs = final; };
        };

        lib.mkMcpNixos = mkMcpNixos;
      };

      perSystem =
        { system, ... }:
        let
          pkgs = import nixpkgs { inherit system; };

          # Shared docs/website commands — available in both the default and
          # `web` devshells so you can pick the right weight class (full Rust
          # + docs vs docs-only).
          docsCommands = [
            {
              category = "docs";
              name = "docs-install";
              help = "install VitePress + theme deps (first-time setup)";
              command = "cd \"$PRJ_ROOT/website\" && npm install \"$@\"";
            }
            {
              category = "docs";
              name = "docs-dev";
              help = "VitePress dev server with hot reload (auto-increments port if 5173 is taken)";
              command = ''
                cd "$PRJ_ROOT/website"
                [ -d node_modules ] || npm install
                npm run dev -- "$@"
              '';
            }
            {
              category = "docs";
              name = "docs-build";
              help = "build the documentation site into website/out/";
              command = ''
                cd "$PRJ_ROOT/website"
                [ -d node_modules ] || npm install
                npm run build
              '';
            }
            {
              category = "docs";
              name = "docs-preview";
              help = "serve the built docs site (auto-increments port if 4173 is taken)";
              command = "cd \"$PRJ_ROOT/website\" && npm run preview -- \"$@\"";
            }
            {
              category = "docs";
              name = "docs-check";
              help = "type-check Vue components with vue-tsc";
              command = "cd \"$PRJ_ROOT/website\" && npm run check -- \"$@\"";
            }
            {
              category = "docs";
              name = "docs-clean";
              help = "remove VitePress build + cache artifacts";
              command = "rm -rf \"$PRJ_ROOT/website/.vitepress/cache\" \"$PRJ_ROOT/website/.vitepress/dist\" \"$PRJ_ROOT/website/out\"";
            }
          ];
        in
        {
          packages = rec {
            mcp-nixos = mkMcpNixos { inherit pkgs; };
            default = mcp-nixos;

            docker = pkgs.dockerTools.buildLayeredImage {
              name = "ghcr.io/utensils/mcp-nixos";
              tag = mcp-nixos.version;
              # Format: YYYYMMDDHHMMSS -> YYYY-MM-DDTHH:MM:SSZ
              created =
                let
                  d = self.lastModifiedDate;
                in
                "${builtins.substring 0 4 d}-${builtins.substring 4 2 d}-${builtins.substring 6 2 d}T${builtins.substring 8 2 d}:${builtins.substring 10 2 d}:${builtins.substring 12 2 d}Z";
              contents = [
                mcp-nixos
                pkgs.cacert
              ];
              config = {
                Entrypoint = [ (pkgs.lib.getExe mcp-nixos) ];
                Env = [
                  "SSL_CERT_FILE=${pkgs.cacert}/etc/ssl/certs/ca-bundle.crt"
                ];
              };
            };
          };

          apps = rec {
            mcp-nixos = {
              type = "app";
              program = pkgs.lib.getExe self.packages.${system}.mcp-nixos;
              meta.description = "MCP server for NixOS, Home Manager, and nix-darwin";
            };
            default = mcp-nixos;
          };

          formatter = pkgs.nixfmt-rfc-style;

          # Default dev shell — Rust backend + docs tooling in one place.
          # Enter with: `nix develop`
          devshells.default = {
            name = "mcp-nixos";

            motd = ''
              {202}mcp-nixos{reset} — Model Context Protocol server for NixOS ({bold}${system}{reset})
              $(type menu &>/dev/null && menu)
            '';

            packages = [
              pkgs.cargo
              pkgs.rustc
              pkgs.rustfmt
              pkgs.clippy
              pkgs.rust-analyzer
              pkgs.cmake
              pkgs.perl
              pkgs.nodejs_22
              pkgs.git
              pkgs.gh
              pkgs.jq
              pkgs.nixfmt-rfc-style
            ];

            commands = [
              # ── server / run ───────────────────────────────────────────────
              {
                category = "run";
                name = "run";
                help = "start the MCP server over STDIO";
                command = "cd \"$PRJ_ROOT\" && cargo run --release -- \"$@\"";
              }

              # ── checks / tests ─────────────────────────────────────────────
              {
                category = "check";
                name = "run-tests";
                help = "cargo test (offline; matches CI)";
                command = "cd \"$PRJ_ROOT\" && cargo test \"$@\"";
              }
              {
                category = "check";
                name = "lint";
                help = "clippy + rustfmt check (matches CI)";
                command = ''
                  cd "$PRJ_ROOT"
                  cargo clippy --all-targets -- -D warnings
                  cargo fmt --check
                '';
              }
              {
                category = "check";
                name = "format";
                help = "cargo fmt";
                command = "cd \"$PRJ_ROOT\" && cargo fmt";
              }
              {
                category = "check";
                name = "ci-local";
                help = "run the same sequence CI runs: lint, tests";
                command = ''
                  set -euo pipefail
                  cd "$PRJ_ROOT"
                  cargo clippy --all-targets -- -D warnings
                  cargo fmt --check
                  cargo test
                '';
              }

              # ── build / release ────────────────────────────────────────────
              {
                category = "build";
                name = "build";
                help = "cargo build --release";
                command = "cd \"$PRJ_ROOT\" && cargo build --release \"$@\"";
              }
              {
                category = "build";
                name = "build-nix";
                help = "nix build — full flake build (matches CI)";
                command = "cd \"$PRJ_ROOT\" && nix build \"$@\"";
              }
              {
                category = "build";
                name = "build-docker";
                help = "nix build .#docker — build the multi-arch Docker image";
                command = "cd \"$PRJ_ROOT\" && nix build .#docker \"$@\"";
              }
            ]
            ++ docsCommands;
          };

          # Lightweight docs-only dev shell — just Node + VitePress helpers.
          # Enter with: `nix develop .#web`
          devshells.web = {
            name = "mcp-nixos-website";

            motd = ''
              {202}mcp-nixos-website{reset} — VitePress docs ({bold}${system}{reset})
              $(type menu &>/dev/null && menu)
            '';

            packages = with pkgs; [
              nodejs_22
              git
              jq
            ];

            commands = docsCommands;
          };
        };
    };
}
