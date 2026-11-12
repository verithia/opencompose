#!/usr/bin/env node
// Minimal MCP stdio <-> OpenCompose HTTP bridge.
//
// The DAW writes {"port", "token"} to ~/.opencompose/agent.json on startup.
// This process speaks newline-delimited JSON-RPC on stdin/stdout (MCP stdio)
// and forwards each message to the DAW's /mcp endpoint.
//
// Usage:
//   node crates/oc-gui/frontend/scripts/opencompose-mcp.mjs
//
// Environment overrides:
//   OPENCOMPOSE_AGENT_FILE  path to agent.json
//   OPENCOMPOSE_AGENT_URL   base URL, e.g. http://127.0.0.1:38642
//   OPENCOMPOSE_AGENT_TOKEN bearer token

import { readFile } from "node:fs/promises";
import { homedir } from "node:os";
import { join } from "node:path";
import { createInterface } from "node:readline";

const endpointFile =
  process.env.OPENCOMPOSE_AGENT_FILE ||
  join(homedir(), ".opencompose", "agent.json");

let endpoint;
try {
  endpoint = JSON.parse(await readFile(endpointFile, "utf8"));
} catch (error) {
  console.error(
    `[opencompose-mcp] cannot read ${endpointFile}: ${error.message}. Start OpenCompose first.`,
  );
  process.exit(1);
}

const base =
  process.env.OPENCOMPOSE_AGENT_URL || `http://127.0.0.1:${endpoint.port}`;
const token = process.env.OPENCOMPOSE_AGENT_TOKEN || endpoint.token;

async function post(message) {
  const response = await fetch(`${base}/mcp`, {
    method: "POST",
    headers: {
      "content-type": "application/json",
      authorization: `Bearer ${token}`,
    },
    body: JSON.stringify(message),
  });
  if (response.status === 202) return null;
  const text = await response.text();
  if (!text) return null;
  return JSON.parse(text);
}

const lines = createInterface({ input: process.stdin, crlfDelay: Infinity });
for await (const line of lines) {
  const trimmed = line.trim();
  if (!trimmed) continue;
  let message;
  try {
    message = JSON.parse(trimmed);
  } catch (error) {
    console.error(`[opencompose-mcp] invalid JSON: ${error.message}`);
    continue;
  }
  try {
    const response = await post(message);
    if (response) {
      process.stdout.write(`${JSON.stringify(response)}\n`);
    }
  } catch (error) {
    console.error(`[opencompose-mcp] request failed: ${error.message}`);
    if (message.id !== undefined) {
      process.stdout.write(
        `${JSON.stringify({
          jsonrpc: "2.0",
          id: message.id,
          error: { code: -32000, message: error.message },
        })}\n`,
      );
    }
  }
}
