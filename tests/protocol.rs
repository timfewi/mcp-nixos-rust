//! Black-box tests of native 2026-07-28 and configured-client 2025-11-25 stdio.

#![allow(clippy::unwrap_used)]

mod common;

use std::io::{BufRead, BufReader, Write};
use std::process::{Child, ChildStdin, ChildStdout, Command, Stdio};
use std::time::{Duration, Instant};

use common::*;
use serde_json::{Value, json};

fn meta() -> Value {
    json!({
        "io.modelcontextprotocol/protocolVersion": "2026-07-28",
        "io.modelcontextprotocol/clientInfo": {"name": "test", "version": "0"},
        "io.modelcontextprotocol/clientCapabilities": {}
    })
}

struct Server {
    child: Child,
    stdin: Option<ChildStdin>,
    stdout: BufReader<ChildStdout>,
}

impl Server {
    fn spawn(upstream: &str) -> Self {
        let mut child = Command::new(env!("CARGO_BIN_EXE_mcp-nixos"))
            .env("MCP_NIXOS_UPSTREAM", upstream)
            .stdin(Stdio::piped())
            .stdout(Stdio::piped())
            .stderr(Stdio::null())
            .spawn()
            .unwrap();
        let stdin = child.stdin.take();
        let stdout = BufReader::new(child.stdout.take().unwrap());
        Self { child, stdin, stdout }
    }

    fn send(&mut self, message: Value) {
        let stdin = self.stdin.as_mut().unwrap();
        writeln!(stdin, "{message}").unwrap();
        stdin.flush().unwrap();
    }

    fn receive(&mut self) -> Value {
        let mut line = String::new();
        self.stdout.read_line(&mut line).unwrap();
        serde_json::from_str(&line).unwrap_or_else(|err| panic!("stdout must be JSON ({err}): {line:?}"))
    }

    fn request(&mut self, id: u64, method: &str, mut params: Value) -> Value {
        params["_meta"] = meta();
        self.send(json!({"jsonrpc": "2.0", "id": id, "method": method, "params": params}));
        self.receive()
    }

    fn call(&mut self, id: u64, arguments: Value) -> Value {
        self.request(id, "tools/call", json!({"name": "nix", "arguments": arguments}))
    }
}

impl Drop for Server {
    fn drop(&mut self) {
        self.child.kill().ok();
        self.child.wait().ok();
    }
}

fn offline() -> Fixture {
    serve(|_| status(500))
}

#[test]
fn discover_needs_no_handshake_and_identifies_the_server() {
    let fixture = offline();
    let mut server = Server::spawn(&fixture.url);
    let result = server.request(1, "server/discover", json!({}))["result"].clone();
    assert_eq!(result["resultType"], "complete");
    assert_eq!(result["supportedVersions"], json!(["2026-07-28", "2025-11-25"]));
    assert_eq!(result["capabilities"], json!({"tools": {}}));
    assert_eq!(result["cacheScope"], "public");
    assert!(result["ttlMs"].as_u64().unwrap() > 0);
    assert_eq!(result["_meta"]["io.modelcontextprotocol/serverInfo"]["name"], "mcp-nixos");
    assert_eq!(result["_meta"]["io.modelcontextprotocol/serverInfo"]["version"], env!("CARGO_PKG_VERSION"));
    assert!(result["instructions"].as_str().unwrap().contains("Common intents"));
}

#[test]
fn tool_list_is_deterministic_and_cacheable() {
    let fixture = offline();
    let mut server = Server::spawn(&fixture.url);
    let first = server.request(1, "tools/list", json!({}))["result"].clone();
    let second = server.request(2, "tools/list", json!({}))["result"].clone();
    assert_eq!(first, second);
    assert!(first["ttlMs"].as_u64().unwrap() > 0);
    assert_eq!(first["cacheScope"], "public");
    let tools = first["tools"].as_array().unwrap();
    let names: Vec<&str> = tools.iter().map(|tool| tool["name"].as_str().unwrap()).collect();
    assert_eq!(names, ["nix", "nix_versions"]);
    assert!(tools[0]["description"].as_str().unwrap().contains("INTENTS"));
    assert_eq!(tools[0]["inputSchema"]["additionalProperties"], false);
    assert_eq!(tools[0]["inputSchema"]["required"], json!(["action"]));
}

#[test]
fn tool_calls_are_self_contained() {
    let fixture = offline();
    let mut server = Server::spawn(&fixture.url);
    let reply = server.call(1, json!({"action": "stats", "source": "wiki"}));
    assert_eq!(reply["result"]["resultType"], "complete");
    assert_eq!(reply["result"]["isError"], false);
    assert_eq!(reply["result"]["content"][0]["text"], "Error (ERROR): Stats not available for source=wiki.");
}

#[test]
fn unsupported_versions_are_rejected_with_the_supported_list() {
    let fixture = offline();
    let mut server = Server::spawn(&fixture.url);
    let mut stale = meta();
    stale["io.modelcontextprotocol/protocolVersion"] = json!("1900-01-01");
    server.send(json!({"jsonrpc": "2.0", "id": 1, "method": "tools/list", "params": {"_meta": stale}}));
    let reply = server.receive();
    assert_eq!(reply["error"]["code"], -32022);
    assert_eq!(reply["error"]["data"]["supported"], json!(["2026-07-28", "2025-11-25"]));
}

#[test]
fn legacy_handshake_serves_configured_clients() {
    let fixture = offline();
    let mut server = Server::spawn(&fixture.url);
    server.send(json!({"jsonrpc": "2.0", "id": 1, "method": "initialize", "params": {
        "protocolVersion": "2025-11-25", "capabilities": {}, "clientInfo": {"name": "old", "version": "0"}
    }}));
    let reply = server.receive();
    assert_eq!(reply["result"]["protocolVersion"], "2025-11-25", "{reply}");
    assert_eq!(reply["result"]["serverInfo"]["name"], "mcp-nixos");
    assert_eq!(reply["result"]["capabilities"], json!({"tools": {}}));
    server.send(json!({"jsonrpc": "2.0", "method": "notifications/initialized"}));

    // Session clients send neither the 2026 protocol metadata nor discovery.
    server.send(json!({"jsonrpc": "2.0", "id": "catalogue", "method": "tools/list", "params": {}}));
    let reply = server.receive();
    assert_eq!(reply["id"], "catalogue");
    let tools = reply["result"]["tools"].as_array().unwrap();
    assert_eq!(tools.iter().map(|tool| tool["name"].as_str().unwrap()).collect::<Vec<_>>(), ["nix", "nix_versions"]);
    server.send(json!({"jsonrpc": "2.0", "id": 3, "method": "tools/call", "params": {
        "name": "nix", "arguments": {"action": "stats", "source": "wiki"}
    }}));
    let reply = server.receive();
    assert_eq!(reply["id"], 3);
    assert_eq!(reply["result"]["content"][0]["text"], "Error (ERROR): Stats not available for source=wiki.");
    assert_eq!(fixture.count("wiki"), 0, "local validation must not make an upstream request");
}

#[test]
fn bad_arguments_are_tool_text_and_unknown_tools_are_protocol_errors() {
    let fixture = offline();
    let mut server = Server::spawn(&fixture.url);
    let reply = server.call(1, json!({"action": "search", "bogus": true}));
    let text = reply["result"]["content"][0]["text"].as_str().unwrap();
    assert!(text.starts_with("Error (ERROR): Invalid arguments for tool 'nix'") && text.contains("bogus"), "{text}");
    let reply = server.request(2, "tools/call", json!({"name": "nope", "arguments": {}}));
    assert_eq!(reply["error"]["code"], -32602);
}

#[test]
fn requests_run_concurrently_on_one_process() {
    let slow = serve(|request| {
        if request.path.contains("wiki") {
            Reply { status: 200, body: json!({"query": {"search": []}}).to_string(), delay: Duration::from_secs(2) }
        } else {
            status(500)
        }
    });
    let mut server = Server::spawn(&slow.url);
    let call = |id, args: Value| {
        json!({"jsonrpc": "2.0", "id": id, "method": "tools/call",
               "params": {"name": "nix", "arguments": args, "_meta": meta()}})
    };
    server.send(call(1, json!({"action": "search", "source": "wiki", "query": "slow"})));
    server.send(call(2, json!({"action": "stats", "source": "wiki"})));
    assert_eq!(server.receive()["id"], 2, "a fast call must not queue behind a slow one");
    assert_eq!(server.receive()["id"], 1);
}

#[test]
fn cancellation_stops_work_and_sends_no_further_messages() {
    let slow = serve(|_| Reply { status: 200, body: "{}".to_owned(), delay: Duration::from_secs(30) });
    let mut server = Server::spawn(&slow.url);
    server.send(json!({"jsonrpc": "2.0", "id": 7, "method": "tools/call", "params": {
        "name": "nix", "arguments": {"action": "search", "source": "wiki", "query": "slow"}, "_meta": meta()}}));
    while slow.count("wiki") == 0 {
        std::thread::sleep(Duration::from_millis(5));
    }
    server.send(json!({"jsonrpc": "2.0", "method": "notifications/cancelled",
        "params": {"requestId": 7, "_meta": meta()}}));
    let started = Instant::now();
    let reply = server.request(8, "tools/list", json!({}));
    assert_eq!(reply["id"], 8, "the cancelled request must stay silent: {reply}");
    assert!(started.elapsed() < Duration::from_secs(5));
}

#[test]
fn closing_stdin_exits_promptly_and_cleanly() {
    let fixture = offline();
    let mut server = Server::spawn(&fixture.url);
    server.request(1, "server/discover", json!({}));
    drop(server.stdin.take());
    let deadline = Instant::now() + Duration::from_secs(5);
    loop {
        if let Some(status) = server.child.try_wait().unwrap() {
            assert!(status.success(), "{status}");
            break;
        }
        assert!(Instant::now() < deadline, "server must exit on stdin EOF");
        std::thread::sleep(Duration::from_millis(10));
    }
}

#[test]
fn store_reads_go_through_the_tool_surface() {
    let fixture = offline();
    let mut server = Server::spawn(&fixture.url);
    let reply = server.call(1, json!({"action": "store", "type": "ls", "query": "/etc"}));
    let text = reply["result"]["content"][0]["text"].as_str().unwrap();
    assert_eq!(text, "Error (INVALID_PATH): Invalid store path: must stay within /nix/store/, got '/etc'");
}
