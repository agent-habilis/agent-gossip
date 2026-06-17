import type { ExtensionAPI, ExtensionCommandContext } from "@earendil-works/pi-coding-agent";
import {
  type CreateOptions,
  createSwarm,
  discoverSwarms,
  getPeers,
  getSwarmStatus,
  joinSwarm,
  leaveSwarm,
  pingPeers,
  sendSwarmMessage,
  validateCreateOptions,
} from "./core";
import { injectAgent } from "./daemon";
import { formatRoster } from "./format";
import { formatSwarmMessage, isValidBody, requireAgentSwarm, runSwarmCommand } from "./helpers";
import { state } from "./state";
import type { DiscoveredSwarm, Peer } from "./types";

export function registerCommands(pi: ExtensionAPI): void {
  pi.registerCommand("swarm-create", {
    description: "Create and join a new swarm for AI agent collaboration",
    handler: cmdCreate,
  });
  pi.registerCommand("swarm-join", {
    description: "Join an existing swarm by ID, domain, or git repo URL",
    handler: cmdJoin,
  });
  pi.registerCommand("swarm-discover", {
    description: "Browse a directory for advertised swarms and join one",
    handler: cmdDiscover,
  });
  pi.registerCommand("swarm-msg", {
    description: "Send a message to the current swarm",
    handler: cmdMsg,
  });
  pi.registerCommand("swarm-reply", {
    description: "Send a message addressed to a specific peer (/swarm-reply {nick} {text})",
    handler: cmdReply,
  });
  pi.registerCommand("swarm-handover", {
    description: "Hand a task off to a peer (/swarm-handover {task})",
    handler: cmdHandover,
  });
  pi.registerCommand("swarm-task", {
    description: "Send a task to a peer and get the result back (/swarm-task {task})",
    handler: cmdTask,
  });
  pi.registerCommand("swarm-leave", {
    description: "Leave the current swarm",
    handler: cmdLeave,
  });
  pi.registerCommand("swarm-status", {
    description: "List swarm peers with connection type, model, and harness",
    handler: cmdStatus,
  });
  pi.registerCommand("swarm-monitor", {
    description: "Control swarm monitoring — toggle auto-reply or view the feed",
    handler: cmdMonitor,
  });
  pi.registerCommand("swarm-ping", {
    description: "Ping all peers in the swarm and measure round-trip time",
    handler: cmdPing,
  });
  pi.registerCommand("swarm-version", {
    description: "Show the swarm binary version and whether the installed extension is up to date",
    handler: cmdVersion,
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

  options.model = ctx.model?.name;
  ctx.ui.notify(options.name ? `🐝 creating #${options.name}...` : "🐝 creating swarm...", "info");
  const result = await createSwarm(options);
  ctx.ui.notify(`🐝 created #${result.name}\n/swarm-join ${result.swarm}`, "info");
  if (result.drift) ctx.ui.notify(`🐝 ${result.drift}`, "info");
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
  const result = await joinSwarm({ target, model: ctx.model?.name });
  ctx.ui.notify(`🐝 joined #${result.name} as <${result.nickname}>`, "info");
  if (result.drift) ctx.ui.notify(`🐝 ${result.drift}`, "info");
}

async function cmdDiscover(args: string, ctx: ExtensionCommandContext): Promise<void> {
  state.ctx = ctx;
  if (!requireAgentSwarm(ctx)) return;

  const directory = args.trim() || "global";
  ctx.ui.notify(`🐝 discovering #${directory}...`, "info");

  const swarms = await discoverSwarms({
    directory: directory === "global" ? undefined : directory,
  });
  if (swarms.length === 0) {
    ctx.ui.notify(`🐝 no swarms found in #${directory}`, "info");
    return;
  }

  // Option label carries name + peers + a short id so distinct swarms never
  // collide; map it back to the full `ahs…` id for the join.
  const byOption = new Map(
    swarms.map((swarm): [string, DiscoveredSwarm] => [
      `#${swarm.name} · ${swarm.peers} peers · ${swarm.swarm.slice(0, 14)}…`,
      swarm,
    ]),
  );
  const choice = await ctx.ui.select(`Swarms in #${directory}`, [...byOption.keys()]);
  const picked = choice ? byOption.get(choice) : undefined;
  if (!picked) return;

  ctx.ui.notify(`🐝 joining #${picked.name}...`, "info");
  const result = await joinSwarm({ target: picked.swarm, model: ctx.model?.name });
  ctx.ui.notify(`🐝 joined #${result.name} as <${result.nickname}>`, "info");
  if (result.drift) ctx.ui.notify(`🐝 ${result.drift}`, "info");
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
    sendSwarmMessage({ text });
    ctx.ui.notify(`🐝 <${session.nickname}>: ${text}`, "info");
  } catch (error) {
    ctx.ui.notify(`🐝 send failed: ${error instanceof Error ? error.message : "unknown"}`, "error");
  }
}

async function cmdReply(args: string, ctx: ExtensionCommandContext): Promise<void> {
  state.ctx = ctx;
  if (!requireAgentSwarm(ctx)) return;

  // First whitespace-delimited token is the target nickname (angle brackets
  // optional); the rest is the message body.
  const match = args.trim().match(/^(\S+)\s+([\s\S]+)$/u);
  if (!match) {
    ctx.ui.notify("🐝 usage: /swarm-reply {nick} {text}", "error");
    return;
  }
  const target = match[1].replace(/^<|>$/gu, "");
  const text = match[2];

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
    sendSwarmMessage({ text, reply: target });
    ctx.ui.notify(`🐝 <${session.nickname}> → <${target}>: ${text}`, "info");
  } catch (error) {
    ctx.ui.notify(
      `🐝 reply failed: ${error instanceof Error ? error.message : "unknown"}`,
      "error",
    );
  }
}

// Read the roster and let the user pick an active peer. Returns the chosen
// peer, or null when the roster is empty/failed or the picker was cancelled.
async function selectWorker(ctx: ExtensionCommandContext, title: string): Promise<Peer | null> {
  let participants: Peer[];
  try {
    participants = getPeers().participants;
  } catch (error) {
    ctx.ui.notify(`🐝 ${error instanceof Error ? error.message : "roster failed"}`, "error");
    return null;
  }
  const eligible = participants
    .filter((peer) => !peer.quiet)
    .sort(
      (left, right) =>
        (left.lastSeenSecsAgo ?? Number.POSITIVE_INFINITY) -
        (right.lastSeenSecsAgo ?? Number.POSITIVE_INFINITY),
    );
  if (eligible.length === 0) {
    ctx.ui.notify("🐝 no peers available", "info");
    return null;
  }
  // Label carries the model/harness so the choice is informed; map back to the
  // peer for the nickname.
  const byLabel = new Map(
    eligible.map((peer): [string, Peer] => {
      const meta = [peer.model, peer.harness].filter(Boolean).join(" / ");
      return [`<${peer.nickname}>${meta ? ` (${meta})` : ""}`, peer];
    }),
  );
  const choice = await ctx.ui.select(title, [...byLabel.keys()]);
  return (choice ? byLabel.get(choice) : undefined) ?? null;
}

async function cmdHandover(args: string, ctx: ExtensionCommandContext): Promise<void> {
  state.ctx = ctx;
  if (!requireAgentSwarm(ctx)) return;
  if (!state.session) {
    ctx.ui.notify("🐝 not in a swarm", "error");
    return;
  }
  const task = args.trim();
  if (!task) {
    ctx.ui.notify("🐝 usage: /swarm-handover {task}", "error");
    return;
  }
  const worker = await selectWorker(ctx, `Hand "${task.slice(0, 60)}" to which peer?`);
  if (!worker) return;

  // The agent composes the brief and sends it via swarm_handover (mirrors the
  // Claude Code plan-as-brief flow, agent-driven).
  injectAgent(
    `🐝 Compose a concise handover brief (what to do, the goal, current state, constraints) for this task and send it to <${worker.nickname}> with the swarm_handover tool (to "${worker.nickname}"):\n\n${task}`,
  );
}

async function cmdTask(args: string, ctx: ExtensionCommandContext): Promise<void> {
  state.ctx = ctx;
  if (!requireAgentSwarm(ctx)) return;
  if (!state.session) {
    ctx.ui.notify("🐝 not in a swarm", "error");
    return;
  }
  const task = args.trim();
  if (!task) {
    ctx.ui.notify("🐝 usage: /swarm-task {task}", "error");
    return;
  }
  const worker = await selectWorker(ctx, `Send "${task.slice(0, 60)}" to which peer?`);
  if (!worker) return;

  // The agent composes the task brief and sends it via swarm_task; the worker
  // runs it and returns a result the agent then confirms or revises.
  injectAgent(
    `🐝 Compose a task brief with an explicit completion criterion for this task and send it to <${worker.nickname}> with the swarm_task tool (to "${worker.nickname}"):\n\n${task}`,
  );
}

async function cmdLeave(_args: string, ctx: ExtensionCommandContext): Promise<void> {
  const name = state.session?.name;
  leaveSwarm();
  ctx.ui.notify(name ? `🐝 left #${name}` : "🐝 left", "info");
}

async function cmdStatus(_args: string, ctx: ExtensionCommandContext): Promise<void> {
  state.ctx = ctx;
  if (!requireAgentSwarm(ctx)) return;
  const session = state.session;
  if (!session) {
    ctx.ui.notify("🐝 not in a swarm", "error");
    return;
  }
  try {
    const { count, participants } = getPeers();
    ctx.ui.notify(formatRoster({ name: session.name, count, participants }), "info");
  } catch (error) {
    ctx.ui.notify(
      `🐝 status failed: ${error instanceof Error ? error.message : "unknown"}`,
      "error",
    );
  }
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

// `ah-s status` reports the binary version and whether each installed
// integration still matches the binary — the on-demand drift check, the
// counterpart to the startup warning folded into the `ready` event.
async function cmdVersion(_args: string, ctx: ExtensionCommandContext): Promise<void> {
  state.ctx = ctx;
  if (!requireAgentSwarm(ctx)) return;

  try {
    ctx.ui.notify(formatSwarmMessage(runSwarmCommand(["status"])), "info");
  } catch (error) {
    ctx.ui.notify(
      `🐝 version check failed: ${error instanceof Error ? error.message : "unknown"}`,
      "error",
    );
  }
}
