#!/usr/bin/env node
// Mock MCP server for tests/mcp_gateway.rs.
//
// It deliberately misbehaves in two ways that the gateway must survive:
//
//   1. It writes a non-JSON banner line to STDOUT before the handshake.
//      stdout is the protocol channel, so a naive client would choke.
//      The gateway must skip non-JSON lines instead of failing.
//
//   2. It writes ordinary log lines to STDERR. Those must never be merged
//      into stdout by the gateway.
//
// Mode comes from argv[2]:
//   "normal"       - handshake + tools/list + tools/call
//   "crash"        - exit(1) right after the banner, before answering
//   "slow"         - never answer, so the client's timeout path runs
//
// Tools exposed: search, calc.

const mode = process.argv[2] || "normal";

// Pollute stdout on purpose (this is the whole point of the fixture).
process.stdout.write("[mock-mcp] starting up, version 0.0.1\n");
// And pollute stderr, which must stay out of the protocol channel.
process.stderr.write("[mock-mcp] this is a log line on stderr\n");

if (mode === "crash") {
  process.stderr.write("[mock-mcp] crashing on purpose\n");
  process.exit(1);
}

const TOOLS = [
  {
    name: "search",
    description: "Search the web",
    inputSchema: {
      type: "object",
      properties: { query: { type: "string" } },
      required: ["query"],
    },
  },
  {
    name: "calc",
    description: "Evaluate an arithmetic expression",
    inputSchema: {
      type: "object",
      properties: { expression: { type: "string" } },
      required: ["expression"],
    },
  },
];

function reply(id, result) {
  process.stdout.write(JSON.stringify({ jsonrpc: "2.0", id, result }) + "\n");
}

let buffer = "";
process.stdin.setEncoding("utf8");
process.stdin.on("data", (chunk) => {
  buffer += chunk;
  let index;
  while ((index = buffer.indexOf("\n")) >= 0) {
    const line = buffer.slice(0, index);
    buffer = buffer.slice(index + 1);
    handle(line);
  }
});

function handle(line) {
  const trimmed = line.trim();
  if (!trimmed) return;

  let msg;
  try {
    msg = JSON.parse(trimmed);
  } catch {
    // A malformed request is the client's bug, not ours: stay quiet.
    return;
  }

  // Notifications have no id and expect no response.
  if (msg.id === undefined || msg.id === null) return;

  if (mode === "slow") return; // never answer

  switch (msg.method) {
    case "initialize":
      reply(msg.id, {
        protocolVersion: "2024-11-05",
        capabilities: { tools: {} },
        serverInfo: { name: "mock-mcp", version: "0.0.1" },
      });
      break;
    case "tools/list":
      reply(msg.id, { tools: TOOLS });
      break;
    case "tools/call": {
      const name = msg.params && msg.params.name;
      const args = (msg.params && msg.params.arguments) || {};
      if (name === "search") {
        reply(msg.id, {
          content: [{ type: "text", text: `results for ${args.query}` }],
        });
      } else if (name === "calc") {
        reply(msg.id, { content: [{ type: "text", text: "42" }] });
      } else {
        process.stdout.write(
          JSON.stringify({
            jsonrpc: "2.0",
            id: msg.id,
            error: { code: -32601, message: `unknown tool ${name}` },
          }) + "\n",
        );
      }
      break;
    }
    default:
      process.stdout.write(
        JSON.stringify({
          jsonrpc: "2.0",
          id: msg.id,
          error: { code: -32601, message: `unknown method ${msg.method}` },
        }) + "\n",
      );
  }
}

process.stdin.on("end", () => process.exit(0));
