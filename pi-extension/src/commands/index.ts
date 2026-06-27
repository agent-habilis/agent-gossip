import type { ExtensionAPI, ExtensionCommandContext } from "@earendil-works/pi-coding-agent";
import {
  type CreateOptions,
  createSwarm,
  discoverSwarms,
  getPeers,
  joinSwarm,
  leaveSwarm,
  pingPeers,
  sendSwarmMessage,
  validateCreateOptions,
} from "../core";
import { formatOutbound, formatRoster } from "../format";
import { isValidBody, requireAgentSwarm, runSwarmCommand } from "../helpers";
import { state } from "../state";
import type { DiscoveredSwarm, Peer } from "../types";
import { inject, notify, notifyBlock, notifyError } from "../ui";

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
// optional swarm name; recognized flags mirror the `ahs create` CLI.
function parseCreateArgs(args: string): {
  options: CreateOptions;
  error?: string;
} {
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
          return {
            options,
            error: `invalid --rate-limit value: ${raw ?? "(missing)"}`,
          };
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
    notifyError(
      `${error}\nusage: /swarm-create [name] [--public] [--mdns] [--dht] [--relay[=urls]] [--rate-limit N] [--advertise[=dir]]`,
    );
    return;
  }
  const invalid = validateCreateOptions(options);
  if (invalid) {
    notifyError(invalid);
    return;
  }

  options.model = ctx.model?.name;
  notify(options.name ? `creating \`#${options.name}\`...` : "creating swarm...");
  const result = await createSwarm(options);
  notify(`created \`#${result.name}\``);
  notify(`\`/swarm-join ${result.swarm}\``);
  if (result.drift) notify(result.drift);
}

async function cmdJoin(args: string, ctx: ExtensionCommandContext): Promise<void> {
  state.ctx = ctx;
  if (!requireAgentSwarm(ctx)) return;

  const target = args.trim();
  if (!target) {
    notifyError("usage: /swarm-join {ahs... | domain | repo-url}");
    return;
  }

  notify(`joining swarm ${target} ...`);
  const result = await joinSwarm({ target, model: ctx.model?.name });
  notify(`joined \`#${result.name}\` as \`<${result.nickname}>\``);
  if (result.drift) notify(result.drift);
}

async function cmdDiscover(args: string, ctx: ExtensionCommandContext): Promise<void> {
  state.ctx = ctx;
  if (!requireAgentSwarm(ctx)) return;

  const directory = args.trim() || "global";
  notify(`discovering \`#${directory}\`...`);

  const swarms = await discoverSwarms({
    directory: directory === "global" ? undefined : directory,
  });
  if (swarms.length === 0) {
    notify(`no swarms found in \`#${directory}\``);
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

  notify(`joining \`#${picked.name}\`...`);
  const result = await joinSwarm({
    target: picked.swarm,
    model: ctx.model?.name,
  });
  notify(`joined \`#${result.name}\` as \`<${result.nickname}>\``);
  if (result.drift) notify(result.drift);
}

async function cmdMsg(args: string, ctx: ExtensionCommandContext): Promise<void> {
  state.ctx = ctx;
  if (!requireAgentSwarm(ctx)) return;

  const text = args.trim();
  if (!text) {
    notifyError("usage: /swarm-msg {text}");
    return;
  }

  if (!isValidBody(text)) {
    notifyError("message body must not contain control characters other than tab/newline");
    return;
  }

  const session = state.session;
  if (!session) {
    notifyError("not in a swarm");
    return;
  }

  try {
    sendSwarmMessage({ text });
    notify(formatOutbound(session.nickname, text));
  } catch (error) {
    notifyError(`send failed: ${error instanceof Error ? error.message : "unknown"}`);
  }
}

async function cmdReply(args: string, ctx: ExtensionCommandContext): Promise<void> {
  state.ctx = ctx;
  if (!requireAgentSwarm(ctx)) return;

  // First whitespace-delimited token is the target nickname (angle brackets
  // optional); the rest is the message body.
  const match = args.trim().match(/^(\S+)\s+([\s\S]+)$/u);
  if (!match) {
    notifyError("usage: /swarm-reply {nick} {text}");
    return;
  }
  const target = match[1].replace(/^<|>$/gu, "");
  const text = match[2];

  if (!isValidBody(text)) {
    notifyError("message body must not contain control characters other than tab/newline");
    return;
  }

  const session = state.session;
  if (!session) {
    notifyError("not in a swarm");
    return;
  }

  try {
    sendSwarmMessage({ text, reply: target });
    notify(formatOutbound(session.nickname, text, target));
  } catch (error) {
    notifyError(`reply failed: ${error instanceof Error ? error.message : "unknown"}`);
  }
}

// Read the roster and let the user pick an active peer. Returns the chosen
// peer, or null when the roster is empty/failed or the picker was cancelled.
async function selectWorker(ctx: ExtensionCommandContext, title: string): Promise<Peer | null> {
  let participants: Peer[];
  try {
    participants = getPeers().participants;
  } catch (error) {
    notifyError(`${error instanceof Error ? error.message : "roster failed"}`);
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
    notify("no peers available");
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
    notifyError("not in a swarm");
    return;
  }
  const task = args.trim();
  if (!task) {
    notifyError("usage: /swarm-handover {task}");
    return;
  }
  const worker = await selectWorker(ctx, `Hand "${task.slice(0, 60)}" to which peer?`);
  if (!worker) return;

  // The agent composes the brief and sends it via swarm_handover (mirrors the
  // Claude Code plan-as-brief flow, agent-driven).
  inject(
    `Compose a concise handover brief (what to do, the goal, current state, constraints) for this task and send it to \`<${worker.nickname}>\` with the swarm_handover tool (to "${worker.nickname}"):\n\n${task}`,
  );
}

async function cmdTask(args: string, ctx: ExtensionCommandContext): Promise<void> {
  state.ctx = ctx;
  if (!requireAgentSwarm(ctx)) return;
  if (!state.session) {
    notifyError("not in a swarm");
    return;
  }
  const task = args.trim();
  if (!task) {
    notifyError("usage: /swarm-task {task}");
    return;
  }
  const worker = await selectWorker(ctx, `Send "${task.slice(0, 60)}" to which peer?`);
  if (!worker) return;

  // The agent composes the task brief and sends it via swarm_task; the worker
  // runs it and returns a result the agent then confirms or revises.
  inject(
    `Compose a task brief with an explicit completion criterion for this task and send it to \`<${worker.nickname}>\` with the swarm_task tool (to "${worker.nickname}"):\n\n${task}`,
  );
}

async function cmdLeave(_args: string, ctx: ExtensionCommandContext): Promise<void> {
  const name = state.session?.name;
  leaveSwarm();
  notify(name ? `left \`#${name}\`` : "left");
}

async function cmdStatus(_args: string, ctx: ExtensionCommandContext): Promise<void> {
  state.ctx = ctx;
  if (!requireAgentSwarm(ctx)) return;
  const session = state.session;
  if (!session) {
    notifyError("not in a swarm");
    return;
  }
  try {
    const { count, participants } = getPeers();
    notifyBlock(formatRoster({ name: session.name, count, participants }));
  } catch (error) {
    notifyError(`status failed: ${error instanceof Error ? error.message : "unknown"}`);
  }
}

async function cmdPing(_args: string, ctx: ExtensionCommandContext): Promise<void> {
  state.ctx = ctx;
  if (!requireAgentSwarm(ctx)) return;

  if (!state.session?.swarm) {
    notifyError("not in a swarm");
    return;
  }

  notify("pinging peers...");

  try {
    const results = await pingPeers();
    if (results.length === 0) {
      notify("no peers responded");
      return;
    }

    const rows = results.map((result) => `| \`<${result.author}>\` | ${result.rtt}ms |`);
    notify(
      [
        "ping results",
        "",
        "| peer | RTT |",
        "| --- | --- |",
        ...rows,
        "",
        `${results.length} online`,
      ].join("\n"),
    );
  } catch (error) {
    notifyError(`ping failed: ${error instanceof Error ? error.message : "unknown"}`);
  }
}

// `ahs status` reports the binary version and whether each installed
// integration still matches the binary — the on-demand drift check, the
// counterpart to the startup warning folded into the `ready` event.
async function cmdVersion(_args: string, ctx: ExtensionCommandContext): Promise<void> {
  state.ctx = ctx;
  if (!requireAgentSwarm(ctx)) return;

  try {
    notifyBlock(runSwarmCommand(["status"]));
  } catch (error) {
    notifyError(`version check failed: ${error instanceof Error ? error.message : "unknown"}`);
  }
}
