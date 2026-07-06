import type { ExtensionAPI, ExtensionCommandContext } from "@earendil-works/pi-coding-agent";
import {
  type CreateOptions,
  applyStateMerge,
  createMesh,
  discoverMeshes,
  forumMesh,
  getPeers,
  getStateDocument,
  joinMesh,
  leaveMesh,
  pingPeers,
  sendMeshMessage,
  validateCreateOptions,
} from "../core";
import {
  formatOutbound,
  formatOutboundNotice,
  formatPeerIdent,
  formatPingReport,
  formatRoster,
} from "../format";
import { isValidBody, requireAgentMesh, runMeshCommand } from "../helpers";
import { state } from "../state";
import type { DiscoveredMesh, Peer } from "../types";
import { inject, notify, notifyBlock, notifyError } from "../ui";

export function registerCommands(pi: ExtensionAPI): void {
  pi.registerCommand("mesh-create", {
    description: "Create and join a new mesh for AI agent collaboration",
    handler: cmdCreate,
  });
  pi.registerCommand("mesh-join", {
    description: "Join an existing mesh by its 💬… ID",
    handler: cmdJoin,
  });
  pi.registerCommand("mesh-forum", {
    description: "Join a public mesh derived from a shared string (/mesh-forum {string})",
    handler: cmdForum,
  });
  pi.registerCommand("mesh-discover", {
    description: "Browse a directory for advertised meshes and join one",
    handler: cmdDiscover,
  });
  pi.registerCommand("mesh-msg", {
    description: "Send a message to the current mesh",
    handler: cmdMsg,
  });
  pi.registerCommand("mesh-notice", {
    description: "Send a notice — a message peers never auto-reply to (/mesh-notice {text})",
    handler: cmdNotice,
  });
  pi.registerCommand("mesh-reply", {
    description: "Send a message addressed to a specific peer (/mesh-reply {nick} {text})",
    handler: cmdReply,
  });
  pi.registerCommand("mesh-handover", {
    description: "Hand a task off to a peer (/mesh-handover {task})",
    handler: cmdHandover,
  });
  pi.registerCommand("mesh-task", {
    description: "Send a task to a peer and get the result back (/mesh-task {task})",
    handler: cmdTask,
  });
  pi.registerCommand("mesh-leave", {
    description: "Leave the current mesh",
    handler: cmdLeave,
  });
  pi.registerCommand("mesh-status", {
    description: "List mesh peers with connection type, model, and harness",
    handler: cmdStatus,
  });
  pi.registerCommand("mesh-state", {
    description: "Print the mesh's current shared-state document",
    handler: cmdState,
  });
  pi.registerCommand("mesh-state-merge", {
    description:
      "Apply an RFC 7386 JSON Merge Patch to shared state (/mesh-state-merge {merge-json})",
    handler: cmdStateMerge,
  });
  pi.registerCommand("mesh-ping", {
    description: "Ping all peers in the mesh and measure round-trip time",
    handler: cmdPing,
  });
  pi.registerCommand("mesh-version", {
    description: "Show the mesh binary version and whether the installed extension is up to date",
    handler: cmdVersion,
  });
}

// Parse `/mesh-create [name] [flags]`. The first non-flag token is the
// optional mesh name; recognized flags mirror the `agent-mesh create` CLI.
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
  if (!requireAgentMesh(ctx)) return;

  const { options, error } = parseCreateArgs(args);
  if (error) {
    notifyError(
      `${error}\nusage: /mesh-create [name] [--public] [--mdns] [--dht] [--relay[=urls]] [--advertise[=dir]]`,
    );
    return;
  }
  const invalid = validateCreateOptions(options);
  if (invalid) {
    notifyError(invalid);
    return;
  }

  options.model = ctx.model?.name;
  const result = await createMesh(options);
  // One notify so the confirmation renders as a single block — the bee prefix
  // is added once (in `send`), not per line, matching the Claude Code plugin.
  notify(
    [
      `created \`#${result.name}\` and joined as \`<${result.nickname}>\``,
      ...(options.advertise ? [`advertising on \`#${options.directory ?? "global"}\``] : []),
      `others can join with: \`/mesh-join ${result.mesh}\``,
    ].join("\n"),
  );
  if (result.drift) notify(result.drift);
}

// Join a mesh and print the standard confirmation — shared by /mesh-join and
// discover-initiated joins so the wording and drift handling stay in one place.
async function joinAndReport(target: string, ctx: ExtensionCommandContext): Promise<void> {
  const result = await joinMesh({ target, model: ctx.model?.name });
  notify(`joined \`#${result.name}\` as \`<${result.nickname}>\``);
  if (result.drift) notify(result.drift);
}

async function cmdJoin(args: string, ctx: ExtensionCommandContext): Promise<void> {
  state.ctx = ctx;
  if (!requireAgentMesh(ctx)) return;

  const target = args.trim();
  if (!target) {
    notifyError("usage: /mesh-join {💬...}");
    return;
  }

  await joinAndReport(target, ctx);
}

async function cmdForum(args: string, ctx: ExtensionCommandContext): Promise<void> {
  state.ctx = ctx;
  if (!requireAgentMesh(ctx)) return;

  const string = args.trim();
  if (!string) {
    notifyError("usage: /mesh-forum {string}");
    return;
  }

  const result = await forumMesh({ string, model: ctx.model?.name });
  notify(`joined forum \`#${result.name}\` as \`<${result.nickname}>\``);
  if (result.drift) notify(result.drift);
}

async function cmdDiscover(args: string, ctx: ExtensionCommandContext): Promise<void> {
  state.ctx = ctx;
  if (!requireAgentMesh(ctx)) return;

  const directory = args.trim() || "global";
  notify(`discovering \`#${directory}\` directory`);
  notify("waiting for meshes…");

  // Sentinel option that re-polls the directory — mirrors the Claude Code
  // discover skill's refreshable picker.
  const KEEP_LOOKING = "🔄 keep looking";

  for (let first = true; ; first = false) {
    const meshes = await discoverMeshes({
      directory: directory === "global" ? undefined : directory,
      // A re-poll on an idle directory shouldn't block the full discovery
      // window again — only the first sweep waits the default.
      ...(first ? {} : { maxMs: 3000 }),
    });

    if (meshes.length === 0) {
      notify(`no meshes in \`#${directory}\` yet`);
      const again = await ctx.ui.select(`Discover #${directory}`, [KEEP_LOOKING]);
      if (again === KEEP_LOOKING) continue;
      return;
    }

    // Option label carries name + peers + a short id so distinct meshes never
    // collide; map it back to the full `💬…` id for the join.
    const byOption = new Map(
      meshes.map((mesh): [string, DiscoveredMesh] => [
        `#${mesh.name} · ${mesh.peers} peers · ${mesh.mesh.slice(0, 14)}…`,
        mesh,
      ]),
    );
    const choice = await ctx.ui.select(`Meshes in #${directory}`, [
      ...byOption.keys(),
      KEEP_LOOKING,
    ]);
    if (!choice) return;
    if (choice === KEEP_LOOKING) continue;
    const picked = byOption.get(choice);
    if (!picked) return;

    await joinAndReport(picked.mesh, ctx);
    return;
  }
}

async function cmdMsg(args: string, ctx: ExtensionCommandContext): Promise<void> {
  state.ctx = ctx;
  if (!requireAgentMesh(ctx)) return;

  const text = args.trim();
  if (!text) {
    notifyError("usage: /mesh-msg {text}");
    return;
  }

  if (!isValidBody(text)) {
    notifyError("message body must not contain control characters other than tab/newline");
    return;
  }

  const session = state.session;
  if (!session) {
    notifyError("not in a mesh");
    return;
  }

  try {
    sendMeshMessage({ text });
    notify(formatOutbound(session.nickname, text));
  } catch (error) {
    notifyError(`send failed: ${error instanceof Error ? error.message : "unknown"}`);
  }
}

async function cmdNotice(args: string, ctx: ExtensionCommandContext): Promise<void> {
  state.ctx = ctx;
  if (!requireAgentMesh(ctx)) return;

  const text = args.trim();
  if (!text) {
    notifyError("usage: /mesh-notice {text}");
    return;
  }

  if (!isValidBody(text)) {
    notifyError("message body must not contain control characters other than tab/newline");
    return;
  }

  const session = state.session;
  if (!session) {
    notifyError("not in a mesh");
    return;
  }

  try {
    sendMeshMessage({ text, notice: true });
    notify(formatOutboundNotice(session.nickname, text));
  } catch (error) {
    notifyError(`notice failed: ${error instanceof Error ? error.message : "unknown"}`);
  }
}

async function cmdReply(args: string, ctx: ExtensionCommandContext): Promise<void> {
  state.ctx = ctx;
  if (!requireAgentMesh(ctx)) return;

  // First whitespace-delimited token is the target nickname (angle brackets
  // optional); the rest is the message body.
  const match = args.trim().match(/^(\S+)\s+([\s\S]+)$/u);
  if (!match) {
    notifyError("usage: /mesh-reply {nick} {text}");
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
    notifyError("not in a mesh");
    return;
  }

  try {
    sendMeshMessage({ text, reply: target });
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
  // Label carries the model/harness/host so the choice is informed; map back to
  // the peer for the nickname.
  const byLabel = new Map(
    eligible.map((peer): [string, Peer] => {
      const meta = formatPeerIdent(peer);
      return [`<${peer.nickname}>${meta ? ` (${meta})` : ""}`, peer];
    }),
  );
  const choice = await ctx.ui.select(title, [...byLabel.keys()]);
  return (choice ? byLabel.get(choice) : undefined) ?? null;
}

async function cmdHandover(args: string, ctx: ExtensionCommandContext): Promise<void> {
  state.ctx = ctx;
  if (!requireAgentMesh(ctx)) return;
  if (!state.session) {
    notifyError("not in a mesh");
    return;
  }
  const task = args.trim();
  if (!task) {
    notifyError("usage: /mesh-handover {task}");
    return;
  }
  const worker = await selectWorker(ctx, `Hand "${task.slice(0, 60)}" to which peer?`);
  if (!worker) return;

  // The agent composes the brief and sends it via mesh_handover (mirrors the
  // Claude Code plan-as-brief flow, agent-driven).
  inject(
    `Compose a concise handover brief (what to do, the goal, current state, constraints) for this task and send it to \`<${worker.nickname}>\` with the mesh_handover tool (to "${worker.nickname}"):\n\n${task}`,
  );
}

async function cmdTask(args: string, ctx: ExtensionCommandContext): Promise<void> {
  state.ctx = ctx;
  if (!requireAgentMesh(ctx)) return;
  if (!state.session) {
    notifyError("not in a mesh");
    return;
  }
  const task = args.trim();
  if (!task) {
    notifyError("usage: /mesh-task {task}");
    return;
  }
  const worker = await selectWorker(ctx, `Send "${task.slice(0, 60)}" to which peer?`);
  if (!worker) return;

  // The agent composes the task brief and sends it via mesh_task; the worker
  // runs it and returns a result the agent then confirms or revises.
  inject(
    `Compose a task brief with an explicit completion criterion for this task and send it to \`<${worker.nickname}>\` with the mesh_task tool (to "${worker.nickname}"):\n\n${task}`,
  );
}

async function cmdLeave(_args: string, ctx: ExtensionCommandContext): Promise<void> {
  const name = state.session?.name;
  leaveMesh();
  notify(name ? `left \`#${name}\`` : "left");
}

async function cmdStatus(_args: string, ctx: ExtensionCommandContext): Promise<void> {
  state.ctx = ctx;
  if (!requireAgentMesh(ctx)) return;
  const session = state.session;
  if (!session) {
    notifyError("not in a mesh");
    return;
  }
  try {
    const { count, participants } = getPeers();
    notifyBlock(formatRoster({ name: session.name, count, participants }));
  } catch (error) {
    notifyError(`status failed: ${error instanceof Error ? error.message : "unknown"}`);
  }
}

async function cmdState(_args: string, ctx: ExtensionCommandContext): Promise<void> {
  state.ctx = ctx;
  if (!requireAgentMesh(ctx)) return;
  if (!state.session) {
    notifyError("not in a mesh");
    return;
  }
  try {
    // Plain block (not markdown) so the JSON isn't reflowed — same as the roster.
    notifyBlock(JSON.stringify(getStateDocument(), null, 2));
  } catch (error) {
    notifyError(`state failed: ${error instanceof Error ? error.message : "unknown"}`);
  }
}

async function cmdStateMerge(args: string, ctx: ExtensionCommandContext): Promise<void> {
  state.ctx = ctx;
  if (!requireAgentMesh(ctx)) return;
  if (!state.session) {
    notifyError("not in a mesh");
    return;
  }
  const merge = args.trim();
  if (!merge) {
    notifyError('usage: /mesh-state-merge {merge-json}  e.g. {"turn":"b"}');
    return;
  }
  try {
    // The incoming self `state` event isn't displayed, so confirm here at send
    // time (mirrors how /mesh-msg confirms an outbound message).
    const result = applyStateMerge({ merge });
    if (result.ok) {
      notify("you changed shared state");
    } else {
      notifyError(result.error ?? "merge rejected");
    }
  } catch (error) {
    notifyError(`state merge failed: ${error instanceof Error ? error.message : "unknown"}`);
  }
}

async function cmdPing(_args: string, ctx: ExtensionCommandContext): Promise<void> {
  state.ctx = ctx;
  if (!requireAgentMesh(ctx)) return;

  if (!state.session?.mesh) {
    notifyError("not in a mesh");
    return;
  }

  notify("pinging peers…");

  try {
    notify(formatPingReport(await pingPeers()));
  } catch (error) {
    notifyError(`ping failed: ${error instanceof Error ? error.message : "unknown"}`);
  }
}

// `agent-mesh status` reports the binary version and whether each installed
// integration still matches the binary — the on-demand drift check, the
// counterpart to the startup warning folded into the `ready` event.
async function cmdVersion(_args: string, ctx: ExtensionCommandContext): Promise<void> {
  state.ctx = ctx;
  if (!requireAgentMesh(ctx)) return;

  try {
    notifyBlock(runMeshCommand(["status"]));
  } catch (error) {
    notifyError(`version check failed: ${error instanceof Error ? error.message : "unknown"}`);
  }
}
