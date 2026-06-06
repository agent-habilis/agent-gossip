import type { ExtensionAPI, ExtensionCommandContext } from "@earendil-works/pi-coding-agent";
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
import { isValidBody, requireAgentSwarm } from "./helpers";
import { state } from "./state";

export function registerCommands(pi: ExtensionAPI): void {
  pi.registerCommand("swarm-create", {
    description: "Create and join a new swarm for AI agent collaboration",
    handler: cmdCreate,
  });
  pi.registerCommand("swarm-join", {
    description: "Join an existing swarm by ID, domain, or git repo URL",
    handler: cmdJoin,
  });
  pi.registerCommand("swarm-msg", {
    description: "Send a message to the current swarm",
    handler: cmdMsg,
  });
  pi.registerCommand("swarm-leave", {
    description: "Leave the current swarm",
    handler: cmdLeave,
  });
  pi.registerCommand("swarm-monitor", {
    description: "Control swarm monitoring — toggle auto-reply or view the feed",
    handler: cmdMonitor,
  });
  pi.registerCommand("swarm-ping", {
    description: "Ping all peers in the swarm and measure round-trip time",
    handler: cmdPing,
  });
}

// Parse `/swarm-create [name] [flags]`. The first non-flag token is the
// optional swarm name; recognized flags mirror the `ah-s create` CLI.
function parseCreateArgs(args: string): { options: CreateOptions; error?: string } {
  const options: CreateOptions = {};
  const tokens = args.trim().split(/\s+/u).filter(Boolean);

  for (let index = 0; index < tokens.length; index += 1) {
    const token = tokens[index];
    const [flag, inlineValue] = token.includes("=")
      ? [token.slice(0, token.indexOf("=")), token.slice(token.indexOf("=") + 1)]
      : [token, undefined];

    switch (flag) {
      case "--public":
        options.network = "public";
        break;
      case "--mdns":
        options.mdns = true;
        break;
      case "--dht":
        options.dht = true;
        break;
      case "--relay":
        options.relay = inlineValue ?? "";
        break;
      case "--advertise":
        options.advertise = true;
        if (inlineValue) options.directory = inlineValue;
        break;
      case "--rate-limit": {
        let raw = inlineValue;
        if (raw === undefined) {
          index += 1;
          raw = tokens[index];
        }
        const parsed = Number(raw);
        if (!Number.isInteger(parsed) || parsed < 0) {
          return { options, error: `invalid --rate-limit value: ${raw ?? "(missing)"}` };
        }
        options.rateLimit = parsed;
        break;
      }
      default:
        if (flag.startsWith("--")) return { options, error: `unknown flag: ${flag}` };
        if (options.name !== undefined) return { options, error: `unexpected argument: ${token}` };
        options.name = token;
    }
  }

  return { options };
}

async function cmdCreate(args: string, ctx: ExtensionCommandContext): Promise<void> {
  state.ctx = ctx;
  if (!requireAgentSwarm(ctx)) return;

  const { options, error } = parseCreateArgs(args);
  if (error) {
    ctx.ui.notify(
      `🐝 ${error}\nusage: /swarm-create [name] [--public] [--mdns] [--dht] [--relay[=urls]] [--rate-limit N] [--advertise[=dir]]`,
      "error",
    );
    return;
  }
  const invalid = validateCreateOptions(options);
  if (invalid) {
    ctx.ui.notify(`🐝 ${invalid}`, "error");
    return;
  }

  ctx.ui.notify(options.name ? `🐝 creating #${options.name}...` : "🐝 creating swarm...", "info");
  const result = await createSwarm(options);
  ctx.ui.notify(`🐝 created #${result.name}\n/swarm-join ${result.swarm}`, "info");
}

async function cmdJoin(args: string, ctx: ExtensionCommandContext): Promise<void> {
  state.ctx = ctx;
  if (!requireAgentSwarm(ctx)) return;

  const target = args.trim();
  if (!target) {
    ctx.ui.notify("🐝 usage: /swarm-join {ahs... | domain | repo-url}", "error");
    return;
  }

  ctx.ui.notify(`🐝 joining ${target.slice(0, 40)}...`, "info");
  const result = await joinSwarm(target);
  ctx.ui.notify(`🐝 joined #${result.name} as <${result.nickname}>`, "info");
}

async function cmdMsg(args: string, ctx: ExtensionCommandContext): Promise<void> {
  state.ctx = ctx;
  if (!requireAgentSwarm(ctx)) return;

  const text = args.trim();
  if (!text) {
    ctx.ui.notify("🐝 usage: /swarm-msg {text}", "error");
    return;
  }

  if (!isValidBody(text)) {
    ctx.ui.notify(
      "🐝 message body must not contain control characters other than tab/newline",
      "error",
    );
    return;
  }

  const session = state.session;
  if (!session) {
    ctx.ui.notify("🐝 not in a swarm", "error");
    return;
  }

  try {
    sendSwarmMessage(text);
    ctx.ui.notify(`🐝 <${session.nickname}>: ${text}`, "info");
  } catch (error) {
    ctx.ui.notify(`🐝 send failed: ${error instanceof Error ? error.message : "unknown"}`, "error");
  }
}

async function cmdLeave(_args: string, ctx: ExtensionCommandContext): Promise<void> {
  const name = state.session?.name;
  leaveSwarm();
  ctx.ui.notify(name ? `🐝 left #${name}` : "🐝 left", "info");
}

async function cmdMonitor(args: string, ctx: ExtensionCommandContext): Promise<void> {
  state.ctx = ctx;
  const subcommand = args.trim();

  if (subcommand === "on") {
    state.autoReply = true;
    ctx.ui.notify("🐝 auto-reply on", "info");
  } else if (subcommand === "off") {
    state.autoReply = false;
    ctx.ui.notify("🐝 auto-reply off", "info");
  } else {
    const status = getSwarmStatus();
    const lines = [
      `swarm: ${status.swarm ?? "none"}`,
      `name: ${status.name ?? "none"}`,
      `nickname: <${status.nickname ?? "none"}>`,
      `auto-reply: ${status.autoReply ? "on" : "off"}`,
      "",
      "🐝 usage: /swarm-monitor {on|off}",
    ];
    ctx.ui.notify(`🐝\n${lines.join("\n")}`, "info");
  }
}

async function cmdPing(_args: string, ctx: ExtensionCommandContext): Promise<void> {
  state.ctx = ctx;
  if (!requireAgentSwarm(ctx)) return;

  if (!state.session?.swarm) {
    ctx.ui.notify("🐝 not in a swarm", "error");
    return;
  }

  ctx.ui.notify("🐝 pinging peers...", "info");

  try {
    const results = await pingPeers();
    if (results.length === 0) {
      ctx.ui.notify("🐝 no peers responded", "info");
      return;
    }

    const lines = ["Ping results", "| peer | RTT |", "|---|---|"];
    for (const result of results) {
      lines.push(`| ${result.author} | ${result.rtt}ms |`);
    }
    lines.push(`${results.length} online`);
    ctx.ui.notify(`🐝\n${lines.join("\n")}`, "info");
  } catch (error) {
    ctx.ui.notify(`🐝 ping failed: ${error instanceof Error ? error.message : "unknown"}`, "error");
  }
}
