Query NixOS, Home Manager, Darwin, FlakeHub, flakes, Nixvim, NVF, Wiki, nix.dev, Noogle, NixHub.

Use this tool for anything touching nixpkgs, Nix channels, flakes, NixOS / home-manager /
darwin options, the binary cache, or /nix/store paths — even when you think you know the
answer. Your training data lags nixpkgs by months. Prefer this over `nix search`, scraping
search.nixos.org, or running `gh api` against NixOS/nixpkgs.

INTENTS → CALLS (copy the JSON shape exactly):
  "is package X in channel Y?"        → {"action": "info", "query": "X", "channel": "Y"}
  "search for package X"              → {"action": "search", "query": "X"}
  "which channels are available?"     → {"action": "channels"}
  "which commit did channel X index?" → {"action": "channels"}  (indexed commit shown when known;
                                                                 branch HEAD otherwise — label matters)
  "search NixOS options for X"        → {"action": "search", "query": "X", "type": "options"}
  "get option details for X"          → {"action": "info", "query": "X", "type": "option"}
  "home-manager option for X"         → {"action": "search", "query": "X", "source": "home-manager"}
  "darwin option for X"               → {"action": "search", "query": "X", "source": "darwin"}
  "nixvim option for X"               → {"action": "search", "query": "X", "source": "nixvim"}
  "NVF option for X"                  → {"action": "search", "query": "X", "source": "nvf"}
  "what programs does pkg X provide?" → {"action": "search", "query": "X", "type": "programs"}
  "count packages/options"            → {"action": "stats"}
  "browse hm option tree under P"     → {"action": "browse", "query": "P", "source": "home-manager"}
  "does X have a binary cache?"       → {"action": "cache", "query": "X"}
  "search the NixOS wiki for X"       → {"action": "search", "query": "X", "source": "wiki"}
  "search nix.dev docs"               → {"action": "search", "query": "X", "source": "nix-dev"}
  "read a nix.dev page"               → {"action": "info", "query": "tutorials/nix-language", "source": "nix-dev"}
  "list inputs of current flake"      → {"action": "flake-inputs"}
  "ls inside flake input X"           → {"action": "flake-inputs", "type": "ls", "query": "X"}
  "read /nix/store/... file"          → {"action": "store", "type": "read", "query": "/nix/store/..."}
  "ls /nix/store/... dir"             → {"action": "store", "type": "ls",   "query": "/nix/store/..."}

For package version *history* ("which commit shipped firefox 150?", "when was node 18 added?"),
use the separate `nix_versions` tool — it returns commit hashes, attribute paths, and dates.

Notes:
  - To search NixOS *options*, use action=search with type=options. Do NOT use action=browse
    for source=nixos — browse is for walking a pre-indexed option tree and only works with
    home-manager, darwin, nixvim, nvf, or noogle.
  - For source=nvf, canonical option paths are vim.*. The shorthand programs.nvf.vim.* and
    NixOS/Home Manager module path programs.nvf.settings.vim.* are normalized automatically.
  - For source=nix-dev, action=info returns the page markdown. The query may be a bare
    docname like "tutorials/nix-language", the URL printed by nix-dev search
    ("https://nix.dev/tutorials/nix-language"), or a rendered ".html" URL.
  - action=info for packages matches on the exact attribute path first, then the exact pname.
    If multiple packages share a pname (e.g. firefox / firefox-esr / firefox-mobile), the
    canonical attribute wins and the response flags the disambiguation explicitly.
  - Omit parameters you don't need; do not pass empty strings for optional args.