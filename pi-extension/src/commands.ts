import type { ExtensionAPI, ExtensionCommandContext } from "@earendil-works/pi-coding-agent";
import {
  createSwarm,
  getSwarmStatus,
  joinSwarm,
  leaveSwarm,
  pingPeers,
  sendSwarmMessage,
} from "./core";
import { isAscii, isValidSwarmName, requireAgentSwarm } from "./helpers";
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
  pi.registerCommand("swarm-whoami", {
    description: "Show your current swarm nickname",
    handler: cmdWhoami,
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

async function cmdCreate(args: string, ctx: ExtensionCommandContext): Promise<void> {
  state.ctx = ctx;
  if (!requireAgentSwarm(ctx)) return;

  const name = args.trim();
  if (!name) {
    ctx.ui.notify("🐝 usage: /swarm-create {name} (1-12 chars, a-z A-Z 0-9 _ -)", "error");
    return;
  }
  if (!isValidSwarmName(name)) {
    ctx.ui.notify("🐝 invalid name — must be 1-12 chars from [a-zA-Z0-9_-]", "error");
    return;
  }

  ctx.ui.notify(`🐝 creating #${name}...`, "info");
  const result = await createSwarm(name);
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

  if (!isAscii(text)) {
    ctx.ui.notify("🐝 message body must be ASCII only", "error");
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

async function cmdWhoami(_args: string, ctx: ExtensionCommandContext): Promise<void> {
  if (!state.session?.nickname) {
    ctx.ui.notify("🐝 not in a swarm", "error");
    return;
  }
  ctx.ui.notify(`🐝 #${state.session.name} as <${state.session.nickname}>`, "info");
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
