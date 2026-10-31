//! An in-process HTTP fixture standing in for every upstream API.
//!
//! The server redirects upstream traffic with `MCP_NIXOS_UPSTREAM`, so requests
//! arrive as `/<host>/<path>`. Tests answer them from a closure and can inspect
//! what was requested.

#![allow(dead_code, clippy::unwrap_used)]

use std::io::{BufRead, BufReader, Read, Write};
use std::net::TcpListener;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Mutex};
use std::time::Duration;

use mcp_nixos::App;
use mcp_nixos::http::Http;
use mcp_nixos::server::{NixParams, nix};
use serde_json::{Value, json};

pub struct Request {
    pub method: String,
    pub path: String,
    pub body: String,
}

pub struct Reply {
    pub status: u16,
    pub body: String,
    pub delay: Duration,
}

pub fn ok(body: impl Into<String>) -> Reply {
    Reply { status: 200, body: body.into(), delay: Duration::ZERO }
}

pub fn status(code: u16) -> Reply {
    Reply { status: code, body: String::new(), delay: Duration::ZERO }
}

pub fn json_reply(value: Value) -> Reply {
    ok(value.to_string())
}

pub struct Fixture {
    pub url: String,
    pub requests: Arc<Mutex<Vec<String>>>,
    stop: Arc<AtomicBool>,
}

impl Fixture {
    pub fn app(&self) -> App {
        App::new(Http::new(Some(self.url.clone())).unwrap())
    }

    /// How many requests hit a path containing `needle`.
    pub fn count(&self, needle: &str) -> usize {
        self.requests.lock().unwrap().iter().filter(|line| line.contains(needle)).count()
    }

    pub fn request_bodies(&self) -> String {
        self.requests.lock().unwrap().join("\n")
    }
}

impl Drop for Fixture {
    fn drop(&mut self) {
        self.stop.store(true, Ordering::SeqCst);
    }
}

fn read_request(stream: &std::net::TcpStream) -> Option<Request> {
    let mut reader = BufReader::new(stream);
    let mut line = String::new();
    reader.read_line(&mut line).ok()?;
    let mut parts = line.split_whitespace();
    let method = parts.next()?.to_owned();
    let path = parts.next()?.to_owned();
    let mut length = 0;
    loop {
        let mut header = String::new();
        reader.read_line(&mut header).ok()?;
        if header.trim().is_empty() {
            break;
        }
        if let Some((name, value)) = header.split_once(':')
            && name.eq_ignore_ascii_case("content-length")
        {
            length = value.trim().parse().unwrap_or(0);
        }
    }
    let mut body = vec![0; length];
    reader.read_exact(&mut body).ok()?;
    Some(Request { method, path, body: String::from_utf8_lossy(&body).into_owned() })
}

pub fn serve(handler: impl Fn(&Request) -> Reply + Send + Sync + 'static) -> Fixture {
    let listener = TcpListener::bind("127.0.0.1:0").unwrap();
    listener.set_nonblocking(true).unwrap();
    let url = format!("http://{}", listener.local_addr().unwrap());
    let requests = Arc::new(Mutex::new(Vec::new()));
    let stop = Arc::new(AtomicBool::new(false));
    let handler = Arc::new(handler);
    let (seen, stopping) = (requests.clone(), stop.clone());
    std::thread::spawn(move || {
        while !stopping.load(Ordering::SeqCst) {
            let Ok((mut stream, _)) = listener.accept() else {
                std::thread::sleep(Duration::from_millis(2));
                continue;
            };
            let (handler, seen) = (handler.clone(), seen.clone());
            std::thread::spawn(move || {
                stream.set_nonblocking(false).ok();
                let Some(request) = read_request(&stream) else { return };
                seen.lock().unwrap().push(format!("{} {} {}", request.method, request.path, request.body));
                let reply = handler(&request);
                std::thread::sleep(reply.delay);
                let head = format!(
                    "HTTP/1.1 {} X\r\ncontent-length: {}\r\nconnection: close\r\n\r\n",
                    reply.status,
                    reply.body.len()
                );
                stream.write_all(head.as_bytes()).ok();
                stream.write_all(reply.body.as_bytes()).ok();
            });
        }
    });
    Fixture { url, requests, stop }
}

pub async fn call(app: &App, args: Value) -> String {
    let params: NixParams = serde_json::from_value(args).unwrap();
    nix(app, params).await
}

/// Search backend with one unstable and two release channels.
pub fn search_backend(request: &Request) -> Option<Reply> {
    if request.path.contains("/_cat/aliases") {
        return Some(json_reply(json!([
            {"alias": "latest-48-nixos-unstable"},
            {"alias": "latest-50-nixos-unstable"},
            {"alias": "latest-50-nixos-26.05"},
            {"alias": "latest-48-nixos-25.11"},
            {"alias": "latest-51-group-manual"},
            {"alias": "unrelated"}
        ])));
    }
    request.path.ends_with("/_count").then(|| json_reply(json!({"count": 1234})))
}

pub fn es_hits(sources: Vec<Value>) -> Reply {
    let hits: Vec<Value> = sources.into_iter().map(|source| json!({"_source": source})).collect();
    json_reply(json!({"hits": {"total": {"value": hits.len()}, "hits": hits}}))
}
