//! MCP-NixOS: Model Context Protocol tools for NixOS, Home Manager, and nix-darwin.
//!
//! Provides search and query capabilities for NixOS packages and options via
//! Elasticsearch, Home Manager, nix-darwin and NVF options via their published
//! documentation, plus Nixvim, flakes, FlakeHub, NixHub, Noogle, the NixOS wiki
//! and nix.dev. All responses are human-readable plain text.

pub mod caches;
pub mod config;
pub mod html;
pub mod http;
pub mod server;
pub mod sources;
pub mod text;

use caches::Caches;
use http::Http;

/// Process-wide state shared by every tool call.
pub struct App {
    pub http: Http,
    pub caches: Caches,
}

impl App {
    pub fn new(http: Http) -> Self {
        Self { http, caches: Caches::default() }
    }
}
