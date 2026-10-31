//! Local flake inputs, via `nix flake archive` and direct store reads.

use std::collections::BTreeMap;
use std::path::Path;
use std::process::Stdio;
use std::time::Duration;

use serde_json::Value;
use tokio::process::Command;

use super::store::{Listing, ReadError, read_text, render_read, scan_directory, too_large, validate_store_path};
use crate::text::{error, format_size, str_of, strip, truthy};

const NIX_TIMEOUT: Duration = Duration::from_secs(60);

fn nix_available() -> bool {
    std::env::var_os("PATH").is_some_and(|paths| {
        std::env::split_paths(&paths).any(|dir| {
            use std::os::unix::fs::PermissionsExt;
            std::fs::metadata(dir.join("nix"))
                .is_ok_and(|meta| meta.is_file() && meta.permissions().mode() & 0o111 != 0)
        })
    })
}

/// Run a nix command with a timeout. The child is killed when the future is
/// dropped, which covers both the timeout and client cancellation.
async fn run_nix(args: &[&str], cwd: &str) -> Result<String, String> {
    let child = Command::new("nix")
        .args(["--extra-experimental-features", "nix-command flakes"])
        .args(args)
        .current_dir(cwd)
        .stdin(Stdio::null())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .kill_on_drop(true)
        .output();
    match tokio::time::timeout(NIX_TIMEOUT, child).await {
        Err(_) => Err("Command timed out".to_owned()),
        Ok(Err(err)) if err.kind() == std::io::ErrorKind::NotFound => Err("nix command not found".to_owned()),
        Ok(Err(err)) => Err(err.to_string()),
        Ok(Ok(output)) if output.status.success() => Ok(String::from_utf8_lossy(&output.stdout).into_owned()),
        Ok(Ok(output)) => Err(String::from_utf8_lossy(&output.stderr).into_owned()),
    }
}

/// Run `nix flake archive --json`.
async fn flake_data(flake_dir: &str) -> Result<Value, String> {
    if !Path::new(flake_dir).join("flake.nix").is_file() {
        return Err(format!("Not a flake directory: {flake_dir} (no flake.nix found)"));
    }
    let stdout = run_nix(&["flake", "archive", "--json"], flake_dir).await.map_err(|stderr| {
        if stderr.to_lowercase().contains("experimental feature") {
            "Flakes not enabled. Enable with: nix-command flakes experimental features".to_owned()
        } else if stderr.contains("does not provide attribute") {
            format!("Invalid flake: {}", strip(&stderr))
        } else {
            format!("Failed to get flake inputs: {}", strip(&stderr))
        }
    })?;
    serde_json::from_str(&stdout).map_err(|err| format!("Failed to parse flake archive output: {err}"))
}

/// Flatten nested inputs: `flake-parts.nixpkgs-lib` -> store path.
pub fn flatten_inputs(data: &Value, prefix: &str, result: &mut BTreeMap<String, String>) {
    let Some(inputs) = data.get("inputs").and_then(Value::as_object) else {
        return;
    };
    for (name, info) in inputs {
        let full_name = if prefix.is_empty() { name.clone() } else { format!("{prefix}.{name}") };
        let store_path = str_of(info, "path");
        if !store_path.is_empty() {
            result.insert(full_name.clone(), store_path.to_owned());
        }
        if info.get("inputs").is_some_and(truthy) {
            flatten_inputs(info, &full_name, result);
        }
    }
}

async fn load_inputs(flake_dir: &str) -> Result<BTreeMap<String, String>, String> {
    let data = flake_data(flake_dir).await.map_err(|message| error(message, "FLAKE_ERROR"))?;
    let mut inputs = BTreeMap::new();
    flatten_inputs(&data, "", &mut inputs);
    Ok(inputs)
}

fn missing_input(name: &str, inputs: &BTreeMap<String, String>) -> String {
    let available: Vec<&str> = inputs.keys().take(10).map(String::as_str).collect();
    let more = if inputs.len() > 10 { format!(" ... and {} more", inputs.len() - 10) } else { String::new() };
    error(format!("Input '{name}' not found. Available: {}{more}", available.join(", ")), "NOT_FOUND")
}

fn nix_missing() -> String {
    error("Nix is not installed or not in PATH", "NIX_NOT_FOUND")
}

/// List all flake inputs with their store paths.
pub async fn flake_inputs_list(flake_dir: &str) -> String {
    if !nix_available() {
        return nix_missing();
    }
    let data = match flake_data(flake_dir).await {
        Ok(data) => data,
        Err(message) => return error(message, "FLAKE_ERROR"),
    };
    let mut inputs = BTreeMap::new();
    flatten_inputs(&data, "", &mut inputs);
    if inputs.is_empty() {
        return "No inputs found for this flake.".to_owned();
    }
    let path = data.get("path").and_then(Value::as_str).unwrap_or(flake_dir);
    let mut lines =
        vec![format!("Flake inputs ({} found):", inputs.len()), format!("Flake path: {path}"), String::new()];
    for (name, store_path) in &inputs {
        lines.push(format!("* {name}"));
        lines.push(format!("  {store_path}"));
        lines.push(String::new());
    }
    strip(&lines.join("\n")).to_owned()
}

fn ls_target(store_path: &str, input_name: &str, subpath: &str) -> String {
    let target = Path::new(store_path).join(subpath);
    let target_str = target.to_string_lossy();
    if !validate_store_path(&target_str) {
        return error("Invalid path: must stay within /nix/store/", "SECURITY_ERROR");
    }
    let shown = if subpath.is_empty() { "/" } else { subpath };
    if !target.exists() {
        return error(format!("Path not found: {shown} in {input_name}"), "NOT_FOUND");
    }
    if !target.is_dir() {
        return error(format!("Not a directory: {shown} in {input_name}"), "NOT_DIRECTORY");
    }
    let listing = match scan_directory(&target) {
        Ok(listing) => listing,
        Err(err) if err.kind() == std::io::ErrorKind::PermissionDenied => {
            return error(format!("Permission denied: {shown}"), "PERMISSION_ERROR");
        }
        Err(err) => return error(format!("Cannot list directory: {err}"), "OS_ERROR"),
    };
    if listing.is_empty() {
        return format!("Directory '{shown}' in {input_name} is empty.");
    }
    let display_path = if subpath.is_empty() { input_name.to_owned() } else { format!("{input_name}:{subpath}") };
    let mut lines = vec![
        format!("Contents of {display_path} ({} dirs, {} files):", listing.dirs.len(), listing.files.len()),
        String::new(),
    ];
    lines.extend(listing.dirs.iter().map(|name| format!("  {name}/")));
    lines.extend(listing.files.iter().map(|(name, size)| Listing::file_line(name, *size)));
    lines.join("\n")
}

/// List directory contents within a flake input (`input` or `input:subpath`).
pub async fn flake_inputs_ls(flake_dir: &str, query: &str) -> String {
    if !nix_available() {
        return nix_missing();
    }
    let (input_name, subpath) = query.split_once(':').unwrap_or((query, ""));
    let subpath = subpath.trim_start_matches('/');
    let inputs = match load_inputs(flake_dir).await {
        Ok(inputs) => inputs,
        Err(message) => return message,
    };
    let Some(store_path) = inputs.get(input_name) else {
        return missing_input(input_name, &inputs);
    };
    ls_target(store_path, input_name, subpath)
}

fn read_target(store_path: &str, input_name: &str, file_path: &str, limit: usize) -> String {
    let target = Path::new(store_path).join(file_path);
    if !validate_store_path(&target.to_string_lossy()) {
        return error("Invalid path: must stay within /nix/store/", "SECURITY_ERROR");
    }
    if !target.exists() {
        return error(format!("File not found: {file_path} in {input_name}"), "NOT_FOUND");
    }
    if target.is_dir() {
        return error(format!("'{file_path}' is a directory. Use type='ls' to list contents."), "IS_DIRECTORY");
    }
    match read_text(&target) {
        Ok((size, text)) => render_read(&format!("{input_name}:{file_path}"), size, &text, limit),
        Err(ReadError::TooLarge(size)) => too_large(size),
        Err(ReadError::Binary(size)) => {
            error(format!("Binary file detected: {file_path} ({})", format_size(size)), "BINARY_FILE")
        }
        Err(ReadError::Access(err)) => error(format!("Cannot access file: {err}"), "OS_ERROR"),
        Err(ReadError::Read(err)) if err.kind() == std::io::ErrorKind::PermissionDenied => {
            error(format!("Permission denied: {file_path}"), "PERMISSION_ERROR")
        }
        Err(ReadError::Read(err)) => error(format!("Cannot read file: {err}"), "OS_ERROR"),
    }
}

/// Read a file from a flake input (`input:path/to/file`).
pub async fn flake_inputs_read(flake_dir: &str, query: &str, limit: usize) -> String {
    if !nix_available() {
        return nix_missing();
    }
    let Some((input_name, file_path)) = query.split_once(':') else {
        return error("Read requires 'input:path' format (e.g., 'nixpkgs:flake.nix')", "INVALID_FORMAT");
    };
    let file_path = file_path.trim_start_matches('/');
    if file_path.is_empty() {
        return error("File path required (e.g., 'nixpkgs:flake.nix')", "INVALID_FORMAT");
    }
    let inputs = match load_inputs(flake_dir).await {
        Ok(inputs) => inputs,
        Err(message) => return message,
    };
    let Some(store_path) = inputs.get(input_name) else {
        return missing_input(input_name, &inputs);
    };
    read_target(store_path, input_name, file_path, limit)
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    #[test]
    fn nested_inputs_flatten_with_dotted_names() {
        let data = json!({"inputs": {
            "nixpkgs": {"path": "/nix/store/a"},
            "parts": {"path": "/nix/store/b", "inputs": {"lib": {"path": "/nix/store/c", "inputs": {}}}},
            "empty": {"path": ""}
        }});
        let mut inputs = BTreeMap::new();
        flatten_inputs(&data, "", &mut inputs);
        assert_eq!(inputs.len(), 3);
        assert_eq!(inputs["parts.lib"], "/nix/store/c");
    }

    #[test]
    fn missing_input_lists_ten_then_counts() {
        let inputs: BTreeMap<String, String> = (0..12).map(|i| (format!("i{i:02}"), String::new())).collect();
        assert!(missing_input("x", &inputs).ends_with("i09 ... and 2 more"));
    }

    #[test]
    fn traversal_out_of_an_input_is_refused() {
        let message = ls_target("/nix/store/abc-x", "x", "../../../etc");
        assert_eq!(message, "Error (SECURITY_ERROR): Invalid path: must stay within /nix/store/");
    }
}
