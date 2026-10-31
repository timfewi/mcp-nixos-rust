Use this server for any question about nixpkgs packages, NixOS / home-manager / nix-darwin / nixvim / NVF options, flakes, FlakeHub, channels, the binary cache, store paths, or the NixOS wiki and nix.dev docs. It queries live APIs (search.nixos.org, NixHub, FlakeHub, cache.nixos.org) and is faster and more current than `nix search`, scraping search.nixos.org by hand, or running `gh api` against NixOS/nixpkgs.

Trigger on any mention of a Nix package name, attribute path, NixOS / home-manager / darwin option, channel name (unstable, 25.05, ...), flake input, or `/nix/store/` path. Use even when you think you know the answer — your training data lags nixpkgs by months.

Two tools are exposed:
- `nix` — unified search/info/stats/browse/channels/flake-inputs/cache/store across NixOS, Home Manager, nix-darwin, Nixvim, NVF, flakes, FlakeHub, NixHub, the NixOS wiki, nix.dev, and Noogle. For package version *history* pair with `nix_versions`.
- `nix_versions` — commit-accurate history from NixHub (which nixpkgs commit shipped version X, what attribute path, which platforms).

Common intents → calls (copy the JSON shape exactly):
  "is package X in channel Y?"         → nix {"action":"info","query":"X","channel":"Y"}
  "which channels are available?"      → nix {"action":"channels"}
  "search NixOS options for X"         → nix {"action":"search","query":"X","type":"options"}
  "home-manager option for X"          → nix {"action":"search","source":"home-manager","query":"X"}
  "NVF option for X"                   → nix {"action":"search","source":"nvf","query":"X"}
  "does X have a binary cache?"        → nix {"action":"cache","query":"X"}
  "read /nix/store/<path>"             → nix {"action":"store","type":"read","query":"/nix/store/<path>"}
  "which commit shipped X version Y?"  → nix_versions {"package":"X","version":"Y"}
