import type { ExtensionAPI, ExtensionContext } from "@earendil-works/pi-coding-agent";
import { Type } from "typebox";
import {
  type CreateOptions,
  createSwarm,
  getSwarmStatus,
  joinSwarm,
  leaveSwarm,
  pingPeers,
  sendSwarmMessage,
  validateCreateOptions,
} from "./core";
import { requireAgentSwarm } from "./helpers";
import { state } from "./state";

function toolError(text: string) {
  return {
    content: [{ type: "text" as const, text }],
    details: null,
    isError: true,
  };
}

export function registerTools(pi: ExtensionAPI): void {
  pi.registerTool({
    name: "swarm_create",
    label: "Swarm Create",
    description: "Create and join a new agent swarm",
    promptSnippet: "Create a new swarm for AI agent collaboration",
    promptGuidelines: [
      "Use swarm_create when the user asks to start a new swarm or collaborate with other agents",
      "Do not reformat or add extra prose after the tool result. The tool output is already the complete response.",
    ],
    parameters: Type.Object({
      name: Type.Optional(
        Type.String({
          description:
            "Human-readable swarm name. 1-32 UTF-8 chars (any script/emoji), excluding control characters, whitespace, and any of / \\ < > #. Bound cryptographically into the swarm identity. Omit for a random word-word name.",
        }),
      ),
      network: Type.Optional(
        Type.String({
          description:
            'Network mode: "public" for the all-on lookup preset (mDNS + DHT + default relay), "private" for localhost only (default: private). Naming mdns/dht/relay overrides the preset.',
        }),
      ),
      mdns: Type.Optional(Type.Boolean({ description: "Enable the LAN mDNS lookup." })),
      dht: Type.Optional(Type.Boolean({ description: "Enable the mainline-DHT lookup." })),
      relay: Type.Optional(
        Type.String({
          description:
            'Relay lookup: omit for off, "" for the default n0 prod ladder, or a comma-separated a,b,c of relay URLs.',
        }),
      ),
      rate_limit: Type.Optional(
        Type.Number({
          description:
            "Per-author messages-per-minute cap baked into the swarm id (every joiner inherits it). 0 disables. Default 60.",
        }),
      ),
      advertise: Type.Optional(
        Type.Boolean({
          description:
            'List this swarm in a directory so others find it with discover. Requires network "public"; makes the swarm open.',
        }),
      ),
      directory: Type.Optional(
        Type.String({ description: "Directory to advertise into. Omit for the global directory." }),
      ),
    }),
    async execute(_toolCallId, params, _signal, _onUpdate, ctx: ExtensionContext) {
      if (!requireAgentSwarm(ctx)) {
        return toolError("ahs CLI not found on PATH");
      }
      const options: CreateOptions = {
        name: params.name,
        network: params.network === "public" ? "public" : "private",
        mdns: params.mdns,
        dht: params.dht,
        relay: params.relay,
        rateLimit: params.rate_limit,
        advertise: params.advertise,
        directory: params.directory,
      };
      const invalid = validateCreateOptions(options);
      if (invalid) {
        return toolError(invalid);
      }
      const result = await createSwarm(options);
      return {
        content: [{ type: "text", text: "ok" }],
        details: { swarm: result.swarm, name: result.name, nickname: result.nickname },
      };
    },
  });

  pi.registerTool({
    name: "swarm_join",
    label: "Swarm Join",
    description: "Join an existing agent swarm",
    promptSnippet: "Join an existing agent swarm by ID, domain, or git repo URL",
    promptGuidelines: [
      "Use swarm_join when the user provides a swarm ID, domain, or git repo URL to join",
      "Use swarm_join when the user says they want to join an existing swarm",
      "Do not reformat or add extra prose after the tool result. The tool output is already the complete response.",
    ],
    parameters: Type.Object({
      target: Type.String({
        description: "Swarm identifier (ahs...), domain (example.com), or git repo URL",
      }),
      nickname: Type.Optional(
        Type.String({ description: "Optional nickname override (auto-generated if omitted)" }),
      ),
    }),
    async execute(_toolCallId, params, _signal, _onUpdate, ctx: ExtensionContext) {
      if (!requireAgentSwarm(ctx)) {
        return toolError("ahs CLI not found on PATH");
      }
      const result = await joinSwarm(params.target, params.nickname);
      return {
        content: [{ type: "text", text: "ok" }],
        details: { swarm: result.swarm, name: result.name, nickname: result.nickname },
      };
    },
  });

  pi.registerTool({
    name: "swarm_send",
    label: "Swarm Send",
    description: "Send a message to the agent swarm",
    promptSnippet: "Broadcast a message to the agent swarm",
    promptGuidelines: [
      "Use swarm_send when the user asks to send a message to other agents in the swarm",
      "Use swarm_send when the agent needs to ask the swarm for help and auto-reply is off",
      "Do not call swarm_status before sending. Use your memory of whether you joined or created a swarm.",
      "If not currently in a swarm, inform the user instead of calling swarm_status first.",
    ],
    parameters: Type.Object({
      text: Type.String({ description: "Message text to send to the swarm (UTF-8)" }),
      reply: Type.Optional(
        Type.String({ description: "Target peer's nickname to address this message to" }),
      ),
    }),
    async execute(_toolCallId, params, _signal, _onUpdate, ctx: ExtensionContext) {
      if (!requireAgentSwarm(ctx)) {
        return toolError("ahs CLI not found on PATH");
      }
      if (!state.session?.swarm) {
        return toolError("Not in a swarm. Use swarm_create or swarm_join first.");
      }
      try {
        sendSwarmMessage(params.text, params.reply);
        return { content: [{ type: "text", text: "ok" }], details: null };
      } catch (error) {
        return toolError(`Send failed: ${error instanceof Error ? error.message : "unknown"}`);
      }
    },
  });

  pi.registerTool({
    name: "swarm_status",
    label: "Swarm Status",
    description: "Get current swarm connection status and recent activity",
    promptSnippet: "Check swarm connection status, nickname, and recent activity",
    promptGuidelines: [
      "Use swarm_status when the user asks about swarm status or peers",
      "Do not use swarm_status to check connectivity before other swarm operations. Rely on memory instead.",
    ],
    parameters: Type.Object({}),
    async execute() {
      const status = getSwarmStatus();
      const lines = [
        `swarm: ${status.swarm ?? "none"}`,
        `name: ${status.name ?? "none"}`,
        `nickname: <${status.nickname ?? "none"}>`,
        `auto-reply: ${status.autoReply ? "on" : "off"}`,
      ];
      return { content: [{ type: "text", text: lines.join("\n") }], details: status };
    },
  });

  pi.registerTool({
    name: "swarm_leave",
    label: "Swarm Leave",
    description: "Leave the current agent swarm",
    promptSnippet: "Leave the current agent swarm",
    promptGuidelines: [
      "Use swarm_leave when the user asks to leave the swarm or stop collaborating",
      "Use swarm_leave when done with swarm operations to clean up",
    ],
    parameters: Type.Object({}),
    async execute() {
      leaveSwarm();
      return { content: [{ type: "text", text: "ok" }], details: null };
    },
  });

  pi.registerTool({
    name: "swarm_ping",
    label: "Swarm Ping",
    description: "Ping all peers in the swarm and measure round-trip time",
    promptSnippet: "Ping all peers in the swarm and measure latency",
    promptGuidelines: ["Use swarm_ping when the user asks to check peer health or connectivity"],
    parameters: Type.Object({}),
    async execute(_toolCallId, _params, _signal, _onUpdate, ctx: ExtensionContext) {
      if (!requireAgentSwarm(ctx)) {
        return toolError("ahs CLI not found on PATH");
      }
      if (!state.session?.swarm) {
        return toolError("Not in a swarm");
      }
      try {
        const results = await pingPeers();
        if (results.length === 0) {
          return { content: [{ type: "text", text: "No peers responded" }], details: null };
        }
        const lines = ["| peer | RTT |", "|---|---|"];
        for (const result of results) {
          lines.push(`| ${result.author} | ${result.rtt}ms |`);
        }
        lines.push(`${results.length} peer(s) online`);
        return { content: [{ type: "text", text: lines.join("\n") }], details: { peers: results } };
      } catch (error) {
        return toolError(`Ping failed: ${error instanceof Error ? error.message : "unknown"}`);
      }
    },
  });

  pi.registerTool({
    name: "swarm_monitor",
    label: "Swarm Monitor",
    description: "Toggle auto-reply or view swarm activity feed",
    promptSnippet: "Toggle swarm auto-reply or view recent activity feed",
    promptGuidelines: [
      "Use swarm_monitor to toggle auto-reply when the user wants to enable or disable automatic responses to swarm questions",
      "Use swarm_monitor to view recent swarm activity",
    ],
    parameters: Type.Object({
      action: Type.String({
        description:
          'Action: "on" to enable auto-reply, "off" to disable, "status" to view current state and feed',
      }),
    }),
    async execute(_toolCallId, params) {
      const action = params.action;
      if (action === "on") {
        state.autoReply = true;
        return { content: [{ type: "text", text: "Auto-reply enabled" }], details: null };
      }
      if (action === "off") {
        state.autoReply = false;
        return { content: [{ type: "text", text: "Auto-reply disabled" }], details: null };
      }
      const status = getSwarmStatus();
      const lines = [
        `swarm: ${status.swarm ?? "none"}`,
        `name: ${status.name ?? "none"}`,
        `nickname: [${status.nickname ?? "none"}]`,
        `auto-reply: ${status.autoReply ? "on" : "off"}`,
      ];
      return { content: [{ type: "text", text: lines.join("\n") }], details: status };
    },
  });
}
