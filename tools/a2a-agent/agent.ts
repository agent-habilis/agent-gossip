#!/usr/bin/env bun
// A minimal, dependency-free A2A (agent-to-agent) agent for bridge testing.
//
// Two roles, one file (run with Bun):
//
//   bun agent.ts server --port PORT [--name NAME]
//       Runs a vanilla A2A server: serves an Agent Card at
//       /.well-known/agent-card.json (with an *absolute* `url`) and answers the
//       `message/send` JSON-RPC method by echoing the text back.
//
//   bun agent.ts client --url BASE_URL [--text TEXT]
//       A vanilla A2A client: fetches the Agent Card from BASE_URL, reads the
//       endpoint `url` the card advertises, sends one `message/send`, and prints
//       the reply. Exits non-zero if the round-trip fails.
//
// This is deliberately raw HTTP/JSON-RPC (no SDK) so it exercises the protocol
// as a real A2A client/server would — including discovery via the card's `url`,
// which is exactly what `ahsw a2a` must rewrite to make the bridge transparent.

import { parseArgs } from "node:util";

const WELL_KNOWN = "/.well-known/agent-card.json";
const WELL_KNOWN_LEGACY = "/.well-known/agent.json";

interface TextPart {
  kind: "text";
  text: string;
}

interface A2aMessage {
  role: string;
  parts: TextPart[];
  messageId: string;
  kind: "message";
}

/** A spec-shaped A2A Agent Card. `url` is absolute — the whole point the bridge
 * has to rewrite so a remote client can reach it. */
function agentCard(baseUrl: string, name: string): Record<string, unknown> {
  return {
    protocolVersion: "0.3.0",
    name,
    description: `echo agent ${name}`,
    url: `${baseUrl}/`,
    version: "1.0.0",
    preferredTransport: "JSONRPC",
    capabilities: { streaming: false },
    defaultInputModes: ["text/plain"],
    defaultOutputModes: ["text/plain"],
    skills: [
      {
        id: "echo",
        name: "echo",
        description: "echoes the text you send",
        tags: ["test"],
      },
    ],
  };
}

/** Concatenate the text parts of an A2A message. */
function textOf(parts: TextPart[] | undefined): string {
  return (parts ?? [])
    .filter((part) => part?.kind === "text")
    .map((part) => part.text)
    .join("");
}

function runServer(port: number, name: string): void {
  const card = agentCard(`http://127.0.0.1:${port}`, name);
  Bun.serve({
    port,
    hostname: "127.0.0.1",
    async fetch(req): Promise<Response> {
      const { pathname } = new URL(req.url);
      if (
        req.method === "GET" &&
        (pathname === WELL_KNOWN || pathname === WELL_KNOWN_LEGACY)
      ) {
        // Response.json sends a fixed Content-Length body (not chunked) — the
        // bridge's card rewriter only rewrites length-framed JSON.
        return Response.json(card);
      }
      if (req.method === "POST") {
        let request: Record<string, unknown>;
        try {
          request = (await req.json()) as Record<string, unknown>;
        } catch {
          return Response.json({ error: "bad json" }, { status: 400 });
        }
        if (request.method !== "message/send") {
          return Response.json({
            jsonrpc: "2.0",
            id: request.id,
            error: { code: -32601, message: "method not found" },
          });
        }
        const params = request.params as { message?: { parts?: TextPart[] } };
        const text = textOf(params?.message?.parts);
        return Response.json({
          jsonrpc: "2.0",
          id: request.id,
          result: {
            role: "agent",
            parts: [{ kind: "text", text: `echo: ${text}` }],
            messageId: "reply-1",
            kind: "message",
          } satisfies A2aMessage,
        });
      }
      return Response.json({ error: "not found" }, { status: 404 });
    },
  });
  console.log(`a2a server '${name}' listening on http://127.0.0.1:${port}`);
}

async function runClient(baseUrl: string, text: string): Promise<number> {
  const base = baseUrl.replace(/\/+$/, "");
  const cardRes = await fetch(`${base}${WELL_KNOWN}`);
  const card = (await cardRes.json()) as { name?: string; url: string };
  const endpoint = card.url;
  console.log(`discovered agent '${card.name}' — endpoint ${endpoint}`);

  const res = await fetch(endpoint, {
    method: "POST",
    headers: { "Content-Type": "application/json" },
    body: JSON.stringify({
      jsonrpc: "2.0",
      id: 1,
      method: "message/send",
      params: {
        message: {
          role: "user",
          parts: [{ kind: "text", text }],
          messageId: "msg-1",
          kind: "message",
        },
      },
    }),
  });
  const response = (await res.json()) as {
    result?: { parts?: TextPart[] };
  };
  const reply = textOf(response.result?.parts);
  console.log(`reply: ${reply}`);

  const expected = `echo: ${text}`;
  if (reply !== expected) {
    console.error(`FAIL: expected ${JSON.stringify(expected)}, got ${JSON.stringify(reply)}`);
    return 1;
  }
  console.log("OK: vanilla A2A round-trip succeeded through the bridge");
  return 0;
}

async function main(): Promise<void> {
  const { values, positionals } = parseArgs({
    args: Bun.argv.slice(2),
    allowPositionals: true,
    options: {
      port: { type: "string" },
      name: { type: "string", default: "echo-agent" },
      url: { type: "string" },
      text: { type: "string", default: "hello over the swarm" },
    },
  });

  const role = positionals[0];
  if (role === "server") {
    if (!values.port) throw new Error("server needs --port");
    // Bun.serve keeps the process alive — do not exit.
    runServer(Number(values.port), values.name!);
    return;
  }
  if (role === "client") {
    if (!values.url) throw new Error("client needs --url");
    process.exit(await runClient(values.url, values.text!));
  }
  console.error("usage: agent.ts <server|client> [options]");
  process.exit(2);
}

main().catch((error) => {
  console.error(error instanceof Error ? error.message : error);
  process.exit(1);
});
