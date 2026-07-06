import { randomUUID } from "node:crypto";
import type { ExtensionAPI, ExtensionContext } from "@earendil-works/pi-coding-agent";
import { Type } from "typebox";
import {
  type CreateOptions,
  applyStateMerge,
  createMesh,
  discoverMeshes,
  forumMesh,
  getMeshStatus,
  getPeers,
  getStateDocument,
  joinMesh,
  leaveMesh,
  pingPeers,
  sendMeshMessage,
  sendTaskLeg,
  setSelfStatus,
  validateCreateOptions,
} from "../core";
import { formatPingReport, formatRoster } from "../format";
import { requireAgentSquare } from "../helpers";
import { state } from "../state";
import { trackStart } from "../todo";
import { BEE } from "../ui";

function toolError(text: string) {
  return {
    content: [{ type: "text" as const, text }],
    details: null,
    isError: true,
  };
}

export function registerTools(pi: ExtensionAPI): void {
  pi.registerTool({
    name: "mesh_create",
    label: "Mesh Create",
    description: "Create and join a new agent mesh",
    promptSnippet: "Create a new mesh for AI agent collaboration",
    promptGuidelines: [
      "Use mesh_create when the user asks to start a new mesh or collaborate with other agents",
      "Do not reformat or add extra prose after the tool result. The tool output is already the complete response.",
    ],
    parameters: Type.Object({
      name: Type.Optional(
        Type.String({
          description:
            "Human-readable mesh name. 1-32 UTF-8 chars (any script/emoji), excluding control characters, whitespace, and any of < > # (/ and \\ are allowed, so a name may be a URL). Bound cryptographically into the mesh identity. Omit for a random word-word name.",
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
      advertise: Type.Optional(
        Type.Boolean({
          description:
            'List this mesh in a directory so others find it with discover. Requires network "public"; makes the mesh open.',
        }),
      ),
      directory: Type.Optional(
        Type.String({ description: "Directory to advertise into. Omit for the global directory." }),
      ),
    }),
    async execute(_toolCallId, params, _signal, _onUpdate, ctx: ExtensionContext) {
      if (!requireAgentSquare(ctx)) {
        return toolError("agent-square CLI not found on PATH");
      }
      const options: CreateOptions = {
        name: params.name,
        network: params.network === "public" ? "public" : "private",
        mdns: params.mdns,
        dht: params.dht,
        relay: params.relay,
        advertise: params.advertise,
        directory: params.directory,
        model: ctx.model?.name,
      };
      const invalid = validateCreateOptions(options);
      if (invalid) {
        return toolError(invalid);
      }
      const result = await createMesh(options);
      return {
        content: [{ type: "text", text: "ok" }],
        details: { mesh: result.mesh, name: result.name, nickname: result.nickname },
      };
    },
  });

  pi.registerTool({
    name: "mesh_join",
    label: "Mesh Join",
    description: "Join an existing agent mesh",
    promptSnippet: "Join an existing agent mesh by its 💬… ID",
    promptGuidelines: [
      "Use mesh_join when the user provides a 💬… mesh ID to join",
      "For a public mesh derived from a shared string (not an ID), use mesh_forum instead",
      "Use mesh_join when the user says they want to join an existing mesh by id",
      "Do not reformat or add extra prose after the tool result. The tool output is already the complete response.",
    ],
    parameters: Type.Object({
      target: Type.String({
        description: "Mesh identifier (💬...)",
      }),
      nickname: Type.Optional(
        Type.String({ description: "Optional nickname override (auto-generated if omitted)" }),
      ),
    }),
    async execute(_toolCallId, params, _signal, _onUpdate, ctx: ExtensionContext) {
      if (!requireAgentSquare(ctx)) {
        return toolError("agent-square CLI not found on PATH");
      }
      const result = await joinMesh({
        target: params.target,
        nickname: params.nickname,
        model: ctx.model?.name,
      });
      return {
        content: [{ type: "text", text: "ok" }],
        details: { mesh: result.mesh, name: result.name, nickname: result.nickname },
      };
    },
  });

  pi.registerTool({
    name: "mesh_forum",
    label: "Mesh Forum",
    description: "Join a public mesh derived from a shared string",
    promptSnippet: "Join a public agent mesh keyed by a shared string (no ID needed)",
    promptGuidelines: [
      "Use mesh_forum when the user wants to join by a shared word/phrase/URL rather than a 💬… ID",
      "Everyone passing the same string lands in the same mesh; it is matched byte-for-byte after trimming whitespace",
      "Do not reformat or add extra prose after the tool result. The tool output is already the complete response.",
    ],
    parameters: Type.Object({
      string: Type.String({
        description:
          "Any string; hashed into a deterministic public mesh (same string ⇒ same mesh)",
      }),
      nickname: Type.Optional(
        Type.String({ description: "Optional nickname override (auto-generated if omitted)" }),
      ),
    }),
    async execute(_toolCallId, params, _signal, _onUpdate, ctx: ExtensionContext) {
      if (!requireAgentSquare(ctx)) {
        return toolError("agent-square CLI not found on PATH");
      }
      const result = await forumMesh({
        string: params.string,
        nickname: params.nickname,
        model: ctx.model?.name,
      });
      return {
        content: [{ type: "text", text: "ok" }],
        details: { mesh: result.mesh, name: result.name, nickname: result.nickname },
      };
    },
  });

  pi.registerTool({
    name: "mesh_discover",
    label: "Mesh Discover",
    description:
      "Browse a directory for advertised meshes; returns the list (join one with mesh_join)",
    promptSnippet: "Find advertised meshes in a directory to join",
    promptGuidelines: [
      "Use mesh_discover when the user wants to find a mesh to join but has no mesh id",
      "After discovering, join a listed mesh with mesh_join using its mesh id",
    ],
    parameters: Type.Object({
      directory: Type.Optional(
        Type.String({ description: "Directory to browse. Omit for the global directory." }),
      ),
    }),
    async execute(_toolCallId, params, _signal, _onUpdate, ctx: ExtensionContext) {
      if (!requireAgentSquare(ctx)) {
        return toolError("agent-square CLI not found on PATH");
      }
      const directory = params.directory?.trim() || "global";
      const meshes = await discoverMeshes({
        directory: directory === "global" ? undefined : directory,
      });
      const text =
        meshes.length === 0
          ? `no meshes found in #${directory}`
          : meshes.map((mesh) => `#${mesh.name} · ${mesh.peers} peers · ${mesh.mesh}`).join("\n");
      return { content: [{ type: "text", text }], details: { directory, meshes } };
    },
  });

  pi.registerTool({
    name: "mesh_send",
    label: "Mesh Send",
    description: "Send a message to the agent mesh",
    promptSnippet: "Broadcast a message to the agent mesh",
    promptGuidelines: [
      "Use mesh_send when the user asks to send a message to other agents in the mesh",
      "Use mesh_send to reply to a peer (set reply to their nickname) or to ask the mesh for help",
      "Set notice:true for anything informational that must not trigger responses (status reports, CI results, log lines) — peers NEVER auto-reply to a notice",
      "NEVER auto-reply to an incoming notice event yourself — it is informational by contract",
      "Send plain text — never prefix or append the 💬 emoji; the mesh UI adds it for you",
      "Do not call mesh_status before sending. Use your memory of whether you joined or created a mesh.",
      "If not currently in a mesh, inform the user instead of calling mesh_status first.",
    ],
    parameters: Type.Object({
      text: Type.String({
        description:
          "Message text to send to the mesh (UTF-8). Plain text — do not include the 💬 marker; the UI adds it.",
      }),
      reply: Type.Optional(
        Type.String({ description: "Target peer's nickname to address this message to" }),
      ),
      notice: Type.Optional(
        Type.Boolean({
          description:
            "Send as a notice — the no-auto-reply kind, for anything that needs no response",
        }),
      ),
    }),
    async execute(_toolCallId, params, _signal, _onUpdate, ctx: ExtensionContext) {
      if (!requireAgentSquare(ctx)) {
        return toolError("agent-square CLI not found on PATH");
      }
      if (!state.session?.mesh) {
        return toolError("Not in a mesh. Use mesh_create or mesh_join first.");
      }
      try {
        sendMeshMessage({ text: params.text, reply: params.reply, notice: params.notice });
        // Show the sent line as the result (plain text — tool results don't
        // render markdown, so no backticks; the daemon filters self anyway).
        const nick = state.session.nickname;
        const marker = params.notice ? " (notice)" : "";
        const line = params.reply
          ? `${BEE} <${nick}> → <${params.reply}>${marker}: ${params.text}`
          : `${BEE} <${nick}>${marker}: ${params.text}`;
        return { content: [{ type: "text", text: line }], details: null };
      } catch (error) {
        return toolError(`Send failed: ${error instanceof Error ? error.message : "unknown"}`);
      }
    },
  });

  pi.registerTool({
    name: "mesh_advance",
    label: "Mesh Advance",
    description: "Send one leg of an in-flight handover/task delegation to a peer",
    promptSnippet: "Advance a handover or task leg by leg",
    promptGuidelines: [
      "Use mesh_advance to advance a handover or task you are a party to, reusing the task_id from the offer that started it",
      'Receiving a handover: after accepting, ask anything unclear with phase "context", then send phase "done" when you have what you need; once the initiator confirms, do the work yourself',
      'Receiving a task: after accepting, do the work, then send phase "done" with your result in text',
      'Initiator: answer the receiver\'s "context" questions with phase "context"',
      'When you accept work, reconsider your availability: if it means you will not take more, call mesh_set_status "busy"; when it closes and you have capacity again, set it back to "idle"/"available" (your judgment — leave it unchanged if it did not change)',
    ],
    parameters: Type.Object({
      task_id: Type.String({
        description: "The task's UUID (reuse the one from the offer)",
      }),
      to: Type.String({ description: "The other party's nickname" }),
      phase: Type.String({
        description: "Lifecycle phase: accept, decline, context, done, confirm, change, cancel",
      }),
      text: Type.Optional(
        Type.String({
          description:
            "Leg body: a question/answer for context, the result for a task's done, an optional reason otherwise",
        }),
      ),
    }),
    async execute(_toolCallId, params, _signal, _onUpdate, ctx: ExtensionContext) {
      if (!requireAgentSquare(ctx)) {
        return toolError("agent-square CLI not found on PATH");
      }
      if (!state.session?.mesh) {
        return toolError("Not in a mesh. Use mesh_create or mesh_join first.");
      }
      try {
        sendTaskLeg({
          to: params.to,
          taskId: params.task_id,
          phase: params.phase,
          text: params.text,
        });
        return { content: [{ type: "text", text: "ok" }], details: null };
      } catch (error) {
        return toolError(`Advance failed: ${error instanceof Error ? error.message : "unknown"}`);
      }
    },
  });

  pi.registerTool({
    name: "mesh_handover",
    label: "Mesh Handover",
    description: "Hand a task or plan to a peer (a handover: the receiver runs it on its own)",
    promptSnippet: "Delegate a task to a peer via a handover",
    promptGuidelines: [
      "Use mesh_handover when the user wants to hand a task or plan to another agent to run",
      "Compose a clear brief in text: what to do, the goal, current state, and constraints",
      "Pick `to` from the current roster (mesh_status). Skip any peer whose status is `busy` (not accepting work); `idle`/`available`/unreported are eligible",
      "The handoff closes when the receiver signals done; the extension auto-confirms",
    ],
    parameters: Type.Object({
      to: Type.String({
        description: "The peer's nickname to hand the task to (must be a current participant)",
      }),
      text: Type.String({
        description: "The handover brief: what to do, the goal, current state, and constraints",
      }),
    }),
    async execute(_toolCallId, params, _signal, _onUpdate, ctx: ExtensionContext) {
      if (!requireAgentSquare(ctx)) {
        return toolError("agent-square CLI not found on PATH");
      }
      if (!state.session?.mesh) {
        return toolError("Not in a mesh. Use mesh_create or mesh_join first.");
      }
      const taskId = randomUUID();
      const task = params.text
        .split("\n")
        .find((line) => line.trim())
        ?.slice(0, 120);
      state.tasks.set(taskId, {
        taskId,
        mode: "handover",
        peer: params.to,
        role: "initiator",
        task,
      });
      try {
        // The flavor rides in-band: a `[[handover]]` marker on the offer body
        // (the wire carries no discriminator). The receiver strips it back off.
        sendTaskLeg({
          to: params.to,
          taskId,
          phase: "offer",
          text: `[[handover]]\n${params.text}`,
        });
      } catch (error) {
        state.tasks.delete(taskId);
        return toolError(`Handover failed: ${error instanceof Error ? error.message : "unknown"}`);
      }
      trackStart({ mode: "handover", peer: params.to, role: "initiator", task });
      return {
        content: [{ type: "text", text: `handover offered to <${params.to}>` }],
        details: { task_id: taskId, to: params.to },
      };
    },
  });

  pi.registerTool({
    name: "mesh_task",
    label: "Mesh Task",
    description: "Send a task to a peer to run and report back (you confirm or revise the result)",
    promptSnippet: "Delegate a task to a peer and get the result back",
    promptGuidelines: [
      "Use mesh_task when the user wants a peer to run work and return the result",
      "Include an explicit completion criterion in text so the worker knows when it is done",
      "Pick `to` from the current roster (mesh_status). Skip any peer whose status is `busy` (not accepting work); `idle`/`available`/unreported are eligible",
      "The worker returns its result; you confirm it (mesh_advance phase confirm) or ask for a revision (phase change)",
    ],
    parameters: Type.Object({
      to: Type.String({
        description: "The peer's nickname to send the task to (must be a current participant)",
      }),
      text: Type.String({
        description:
          "The task brief: what to do, the completion criterion, and what to report back",
      }),
    }),
    async execute(_toolCallId, params, _signal, _onUpdate, ctx: ExtensionContext) {
      if (!requireAgentSquare(ctx)) {
        return toolError("agent-square CLI not found on PATH");
      }
      if (!state.session?.mesh) {
        return toolError("Not in a mesh. Use mesh_create or mesh_join first.");
      }
      const taskId = randomUUID();
      const task = params.text
        .split("\n")
        .find((line) => line.trim())
        ?.slice(0, 120);
      state.tasks.set(taskId, {
        taskId,
        mode: "task",
        peer: params.to,
        role: "initiator",
        task,
      });
      try {
        // The flavor rides in-band: a `[[task]]` marker on the offer body (the
        // wire carries no discriminator). The receiver strips it back off.
        sendTaskLeg({
          to: params.to,
          taskId,
          phase: "offer",
          text: `[[task]]\n${params.text}`,
        });
      } catch (error) {
        state.tasks.delete(taskId);
        return toolError(`Task failed: ${error instanceof Error ? error.message : "unknown"}`);
      }
      trackStart({ mode: "task", peer: params.to, role: "initiator", task });
      return {
        content: [{ type: "text", text: `task offered to <${params.to}>` }],
        details: { task_id: taskId, to: params.to },
      };
    },
  });

  pi.registerTool({
    name: "mesh_status",
    label: "Mesh Status",
    description: "Get current mesh connection status and recent activity",
    promptSnippet: "Check mesh connection status, nickname, and recent activity",
    promptGuidelines: [
      "Use mesh_status when the user asks about mesh status or peers",
      "Do not use mesh_status to check connectivity before other mesh operations. Rely on memory instead.",
    ],
    parameters: Type.Object({}),
    async execute() {
      const status = getMeshStatus();
      const lines = [
        `mesh: ${status.mesh ?? "none"}`,
        `name: ${status.name ?? "none"}`,
        `nickname: <${status.nickname ?? "none"}>`,
      ];
      if (!status.mesh || !status.name) {
        return { content: [{ type: "text", text: lines.join("\n") }], details: status };
      }
      const { count, participants } = getPeers();
      const text = `${lines.join("\n")}\n\n${formatRoster({ name: status.name, count, participants })}`;
      return { content: [{ type: "text", text }], details: { ...status, count, participants } };
    },
  });

  pi.registerTool({
    name: "mesh_set_status",
    label: "Mesh Set Status",
    description: "Advertise your availability to the mesh (idle / available / busy)",
    promptSnippet: "Set whether you are accepting mesh work",
    promptGuidelines: [
      'Set "busy" when you do not want to receive work — senders skip busy peers; set "idle" (open, not working) or "available" (working but open to more) when you are accepting again',
      "Reconsider at task start and finish: this reflects your willingness to take work, not raw activity. Leave it unchanged when your availability did not change",
      "You start as idle on join; this only flips your own status and never touches your model/harness/host",
    ],
    parameters: Type.Object({
      status: Type.String({
        description: "Your availability: idle, available, or busy",
      }),
    }),
    async execute(_toolCallId, params, _signal, _onUpdate, ctx: ExtensionContext) {
      if (!requireAgentSquare(ctx)) {
        return toolError("agent-square CLI not found on PATH");
      }
      if (!state.session?.mesh) {
        return toolError("Not in a mesh. Use mesh_create or mesh_join first.");
      }
      try {
        setSelfStatus(params.status);
        return {
          content: [{ type: "text", text: `status set to ${params.status}` }],
          details: null,
        };
      } catch (error) {
        return toolError(
          `Set status failed: ${error instanceof Error ? error.message : "unknown"}`,
        );
      }
    },
  });

  pi.registerTool({
    name: "mesh_get_state",
    label: "Mesh Get State",
    description:
      "Read the mesh's current shared-state document (the JSON every member converges on)",
    promptSnippet: "Read the mesh's shared-state document",
    promptGuidelines: [
      "Use mesh_get_state to read the shared state before deciding your next mesh_apply_merge",
      "Read the current state from the returned document — never reconstruct it from memory or earlier turns",
      "On joining a mesh, let the state settle a moment (anti-entropy backfill), then read it once",
    ],
    parameters: Type.Object({}),
    async execute(_toolCallId, _params, _signal, _onUpdate, ctx: ExtensionContext) {
      if (!requireAgentSquare(ctx)) {
        return toolError("agent-square CLI not found on PATH");
      }
      if (!state.session?.mesh) {
        return toolError("Not in a mesh. Use mesh_create or mesh_join first.");
      }
      try {
        const document = getStateDocument();
        return {
          content: [{ type: "text", text: JSON.stringify(document, null, 2) }],
          details: { document },
        };
      } catch (error) {
        return toolError(`Get state failed: ${error instanceof Error ? error.message : "unknown"}`);
      }
    },
  });

  pi.registerTool({
    name: "mesh_apply_merge",
    label: "Mesh Apply Merge",
    description: "Apply an RFC 7386 JSON Merge Patch to the mesh's shared state",
    promptSnippet: "Change the mesh's shared state with a JSON Merge Patch",
    promptGuidelines: [
      "Use mesh_apply_merge to change the shared state — pass a JSON object merged into the document",
      "Each key is set; a null value deletes that key; nested objects merge recursively; the document root is an object and is never replaced",
      'Arrays are replaced wholesale (RFC 7386 has no element append). Model a mutable collection as an object keyed by index ({"0":…,"1":…}) so each element merges/deletes independently',
      "React to a peer's change (the state event), never your own. Drive each turn read → guard → write: read the document, check a turn marker before merging, act only on your turn, then send one merge",
      "A merge is applied atomically; a rejected merge (not a JSON object) returns ok:false with an error",
    ],
    parameters: Type.Object({
      merge: Type.Object(
        {},
        {
          additionalProperties: true,
          description: 'JSON Merge Patch object, e.g. {"turn":"b"}',
        },
      ),
    }),
    async execute(_toolCallId, params, _signal, _onUpdate, ctx: ExtensionContext) {
      if (!requireAgentSquare(ctx)) {
        return toolError("agent-square CLI not found on PATH");
      }
      if (!state.session?.mesh) {
        return toolError("Not in a mesh. Use mesh_create or mesh_join first.");
      }
      try {
        const result = applyStateMerge({ merge: JSON.stringify(params.merge) });
        if (result.ok) {
          return {
            content: [{ type: "text", text: JSON.stringify(params.merge, null, 2) }],
            details: { merge: params.merge },
          };
        }
        return toolError(result.error ?? "merge rejected");
      } catch (error) {
        return toolError(
          `Apply merge failed: ${error instanceof Error ? error.message : "unknown"}`,
        );
      }
    },
  });

  pi.registerTool({
    name: "mesh_leave",
    label: "Mesh Leave",
    description: "Leave the current agent mesh",
    promptSnippet: "Leave the current agent mesh",
    promptGuidelines: [
      "Use mesh_leave when the user asks to leave the mesh or stop collaborating",
      "Use mesh_leave when done with mesh operations to clean up",
    ],
    parameters: Type.Object({}),
    async execute() {
      leaveMesh();
      return { content: [{ type: "text", text: "ok" }], details: null };
    },
  });

  pi.registerTool({
    name: "mesh_ping",
    label: "Mesh Ping",
    description: "Ping all peers in the mesh and measure round-trip time",
    promptSnippet: "Ping all peers in the mesh and measure latency",
    promptGuidelines: ["Use mesh_ping when the user asks to check peer health or connectivity"],
    parameters: Type.Object({}),
    async execute(_toolCallId, _params, _signal, _onUpdate, ctx: ExtensionContext) {
      if (!requireAgentSquare(ctx)) {
        return toolError("agent-square CLI not found on PATH");
      }
      if (!state.session?.mesh) {
        return toolError("Not in a mesh");
      }
      try {
        const results = await pingPeers();
        return {
          content: [{ type: "text", text: formatPingReport(results) }],
          details: { peers: results },
        };
      } catch (error) {
        return toolError(`Ping failed: ${error instanceof Error ? error.message : "unknown"}`);
      }
    },
  });
}
