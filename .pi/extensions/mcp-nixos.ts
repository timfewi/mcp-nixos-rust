import { type Static, Type } from "@mariozechner/pi-ai";
import { defineTool, type ExtensionAPI } from "@mariozechner/pi-coding-agent";
import { spawn } from "node:child_process";
import { existsSync } from "node:fs";
import { dirname, resolve } from "node:path";
import { fileURLToPath } from "node:url";

const extensionDir = dirname(fileURLToPath(import.meta.url));
const projectRoot = resolve(extensionDir, "../..");

const PROTOCOL_VERSION = "2026-07-28";

class CancelledError extends Error {
	constructor() {
		super("Cancelled");
		this.name = "CancelledError";
	}
}

/**
 * One stateless MCP tools/call over stdio: start the server, send a
 * self-describing request, read its single response line, close stdin.
 */
async function callServer(
	command: string,
	commandArgs: string[],
	tool: string,
	args: unknown,
	signal?: AbortSignal,
): Promise<string> {
	if (signal?.aborted) throw new CancelledError();

	return new Promise((resolvePromise, reject) => {
		const child = spawn(command, commandArgs, { cwd: projectRoot, stdio: ["pipe", "pipe", "pipe"] });

		let stdout = "";
		let stderr = "";
		let settled = false;
		const settle = (finish: () => void) => {
			if (settled) return;
			settled = true;
			signal?.removeEventListener("abort", onAbort);
			finish();
		};
		const onAbort = () => {
			child.kill("SIGTERM");
			settle(() => reject(new CancelledError()));
		};
		if (signal) signal.addEventListener("abort", onAbort, { once: true });

		child.stdout.on("data", (chunk) => {
			stdout += String(chunk);
			const newline = stdout.indexOf("\n");
			if (newline === -1) return;
			child.stdin.end();
			try {
				const reply = JSON.parse(stdout.slice(0, newline));
				if (reply.error) throw new Error(reply.error.message);
				const text = reply.result.content.map((block: { text?: string }) => block.text ?? "").join("");
				settle(() => resolvePromise(text));
			} catch (error) {
				settle(() => reject(error));
			}
		});
		child.stderr.on("data", (chunk) => {
			stderr += String(chunk);
		});
		child.stdin.on("error", () => {
			// The close handler below reports a server that exited early.
		});
		child.on("error", (err) => settle(() => reject(err)));
		child.on("close", (code) => {
			settle(() => reject(new Error(`${command} exited with ${code}: ${stderr.trim()}`)));
		});

		const request = {
			jsonrpc: "2.0",
			id: 1,
			method: "tools/call",
			params: {
				name: tool,
				arguments: args,
				_meta: {
					"io.modelcontextprotocol/protocolVersion": PROTOCOL_VERSION,
					"io.modelcontextprotocol/clientInfo": { name: "pi-mcp-nixos", version: "1" },
					"io.modelcontextprotocol/clientCapabilities": {},
				},
			},
		};
		child.stdin.write(`${JSON.stringify(request)}\n`);
	});
}

async function runNixosTool(tool: "nix" | "nix_versions", args: unknown, signal?: AbortSignal): Promise<string> {
	const candidates: Array<{ command: string; args: string[] }> = [];
	for (const profile of ["release", "debug"]) {
		const binary = resolve(projectRoot, "target", profile, "mcp-nixos");
		if (existsSync(binary)) candidates.push({ command: binary, args: [] });
	}
	candidates.push(
		{ command: "mcp-nixos", args: [] },
		{ command: "nix", args: ["run", ".#mcp-nixos"] },
	);

	const errors: string[] = [];
	for (const candidate of candidates) {
		try {
			return (await callServer(candidate.command, candidate.args, tool, args, signal)).trimEnd();
		} catch (error) {
			// Cancellation stops retries — don't spawn more servers after abort.
			if (error instanceof CancelledError || signal?.aborted) {
				throw error instanceof CancelledError ? error : new CancelledError();
			}
			const message = error instanceof Error ? error.message : String(error);
			errors.push(`${candidate.command}: ${message}`);
		}
	}

	throw new Error(
		[
			"Failed to run mcp-nixos from pi.",
			"Tried target/{release,debug}/mcp-nixos, mcp-nixos on PATH, and nix run.",
			...errors,
		].join("\n"),
	);
}

const nixToolParams = Type.Object({
	action: Type.String({
		description:
			"One of: search, info, stats, browse, channels, flake-inputs, cache, store. " +
			"search = keyword lookup; info = details for a specific name; " +
			"browse = walk an option hierarchy by prefix (home-manager/darwin/nixvim/nvf/noogle only); " +
			"store = read files or list directories at an explicit /nix/store/ path.",
	}),
	query: Type.Optional(
		Type.String({
			description:
				"Search term for 'search', exact name for 'info', prefix path for 'browse'. " +
				"For flake-inputs: input_name or input:path. For store: absolute /nix/store/ path. " +
				"Omit for 'stats'/'channels'.",
		}),
	),
	source: Type.Optional(
		Type.String({
			description:
				"Data source for search/info/stats/browse/cache. One of: nixos (default), " +
				"home-manager, darwin, flakes, flakehub, nixvim, nvf, wiki, nix-dev, noogle, nixhub. " +
				"For action=flake-inputs, this may instead be a path to a flake directory; " +
				"omit/default to use the current project. Ignored by action=store.",
		}),
	),
	type: Type.Optional(
		Type.String({
			description:
				"Sub-type of query. For source=nixos with action=search, one of: " +
				"packages, options, programs, flakes. For source=nixos with action=info, one of: " +
				"package, option. For flake-inputs, one of: list, ls, read. For store, one of: " +
				"ls, read. Ignored by most other sources.",
		}),
	),
	channel: Type.Optional(
		Type.String({ description: "NixOS channel: unstable (default), stable, or a release like 25.05." }),
	),
	limit: Type.Optional(
		Type.Integer({ description: "Max results. 1-100 (or 1-2000 for flake-inputs/store read). Default 20." }),
	),
	version: Type.Optional(
		Type.String({ description: "Only used by action=cache. Package version (default: latest)." }),
	),
	system: Type.Optional(
		Type.String({
			description: "Only used by action=cache. System arch e.g. x86_64-linux. Empty for all.",
		}),
	),
});

type NixToolParams = Static<typeof nixToolParams>;

const nixToolDescription = [
	"Query live NixOS data (packages, options, flakes, wiki, nix.dev, Home Manager, nix-darwin, Nixvim, NVF, Noogle, NixHub, binary cache).",
	"",
	"Examples (copy the JSON shape exactly):",
	'  Search NixOS packages:    {"action": "search", "query": "firefox"}',
	'  Search NixOS options:     {"action": "search", "query": "nginx", "type": "options"}',
	'  Get a package:            {"action": "info", "query": "firefox"}',
	'  Get an option:            {"action": "info", "query": "services.nginx.enable", "type": "option"}',
	'  Search Home Manager:      {"action": "search", "query": "git", "source": "home-manager"}',
	'  Browse HM option tree:    {"action": "browse", "query": "programs", "source": "home-manager"}',
	'  Search NVF options:       {"action": "search", "query": "languages.nix", "source": "nvf"}',
	'  Inspect an NVF option:    {"action": "info", "query": "programs.nvf.vim.languages.nix.enable", "source": "nvf"}',
	'  Browse NVF options:       {"action": "browse", "query": "vim.languages.nix", "source": "nvf"}',
	'  Search the wiki:          {"action": "search", "query": "zfs", "source": "wiki"}',
	'  Search nix.dev docs:      {"action": "search", "query": "flakes", "source": "nix-dev"}',
	'  Read a nix.dev page:      {"action": "info", "query": "tutorials/nix-language", "source": "nix-dev"}',
	'  List channels:            {"action": "channels"}',
	'  Check binary cache:       {"action": "cache", "query": "firefox"}',
	'  List a store directory:   {"action": "store", "type": "ls", "query": "/nix/store/abc...-foo"}',
	'  Read a store file:        {"action": "store", "type": "read", "query": "/nix/store/abc...-foo/bin/foo"}',
	"",
	"Notes:",
	"  - To search NixOS options, use action=search with type=options. Do NOT use action=browse for source=nixos.",
	"  - action=browse walks a pre-indexed option tree and only supports home-manager, darwin, nixvim, nvf, or noogle.",
	"  - NVF's canonical paths are vim.*; programs.nvf.vim.* is shorthand and programs.nvf.settings.vim.* is the module-path alias.",
	"  - For source=nix-dev, action=info returns the page markdown. Query accepts a bare docname ('tutorials/nix-language'), the URL printed by nix-dev search ('https://nix.dev/tutorials/nix-language'), or a rendered '.html' URL.",
	"  - Omit optional parameters; don't pass empty strings.",
	"  - For package version history use the separate nix_versions tool.",
].join("\n");

const nixTool = defineTool({
	name: "nix",
	label: "NixOS",
	description: nixToolDescription,
	promptSnippet:
		"Prefer the nix tool over web search for NixOS packages, options, flakes, wiki, nix.dev, Home Manager, nix-darwin, Nixvim, NVF, Noogle, flake inputs, and binary cache status.",
	promptGuidelines: [
		"Prefer the nix tool over web search for NixOS-related package, option, flake, wiki, nix.dev, and cache questions.",
		'To search NixOS options: {"action": "search", "query": "<keyword>", "type": "options"}.',
		'To inspect a NixOS option: {"action": "info", "query": "<option.path>", "type": "option"}.',
		"action=browse is for walking option prefixes in home-manager, darwin, nixvim, nvf, or noogle only — never with source=nixos.",
	],
	parameters: nixToolParams,
	async execute(_toolCallId: string, params: NixToolParams, signal: AbortSignal | undefined) {
		// Legacy alias: older model sessions may still emit action=options.
		// The server also normalizes this, but translating here keeps details.args honest.
		const action = params.action === "options" ? "browse" : params.action;

		// Only forward keys the caller actually set. Passing empty-string defaults would
		// (a) echo noisy args back in details.args, contradicting the "omit optional
		// parameters" guidance in the tool description, and (b) train small models to
		// copy those empties into future calls. The server already supplies its own
		// sensible defaults when a key is omitted.
		const normalized: Record<string, string | number> = { action };
		if (params.query !== undefined) normalized.query = params.query;
		if (params.source !== undefined) normalized.source = params.source;
		if (params.type !== undefined) normalized.type = params.type;
		if (params.channel !== undefined) normalized.channel = params.channel;
		if (params.limit !== undefined) normalized.limit = params.limit;
		if (params.version !== undefined) normalized.version = params.version;
		if (params.system !== undefined) normalized.system = params.system;

		const text = await runNixosTool("nix", normalized, signal);
		return {
			content: [{ type: "text", text }],
			details: { tool: "nix", args: normalized },
		};
	},
});

const nixVersionsParams = Type.Object({
	package: Type.String({ description: "Package name, e.g. git or firefox." }),
	version: Type.Optional(Type.String({ description: "Optional exact version to find." })),
	limit: Type.Optional(Type.Integer({ description: "How many recent versions to return (1-50, default 10)." })),
});

type NixVersionsParams = Static<typeof nixVersionsParams>;

const nixVersionsTool = defineTool({
	name: "nix_versions",
	label: "NixOS Versions",
	description: [
		"Get a Nix package's version history from NixHub.io (commit hashes, license, homepage, programs).",
		"",
		"Examples:",
		'  Recent versions:      {"package": "git"}',
		'  Find exact version:   {"package": "git", "version": "2.42.0"}',
	].join("\n"),
	promptSnippet: "Look up historical versions for a Nix package, including commit hashes and metadata.",
	promptGuidelines: [
		"Use nix_versions when the user wants version history or wants to pin a specific historical package version.",
	],
	parameters: nixVersionsParams,
	async execute(_toolCallId: string, params: NixVersionsParams, signal: AbortSignal | undefined) {
		const normalized: Record<string, string | number> = { package: params.package };
		if (params.version !== undefined) normalized.version = params.version;
		if (params.limit !== undefined) normalized.limit = params.limit;

		const text = await runNixosTool("nix_versions", normalized, signal);
		return {
			content: [{ type: "text", text }],
			details: { tool: "nix_versions", args: normalized },
		};
	},
});

export default function mcpNixosExtension(pi: ExtensionAPI) {
	pi.registerTool(nixTool);
	pi.registerTool(nixVersionsTool);
}
