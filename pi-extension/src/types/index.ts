export type Session = {
  mesh: string;
  name: string;
  nickname: string;
  pid?: number;
  // Set when the `ready` event reports the installed extension has fallen
  // behind the `agent-square` binary — surfaced once at mesh start.
  drift?: string;
};

export type MeshEvent = {
  event: string;
  type?: string;
  subtype?: string;
  author?: string;
  body?: string;
  id?: string;
  reply?: string | null;
  self?: boolean;
  mesh?: string;
  nickname?: string;
  // On `task` / `task_progress` events.
  task_id?: string;
  phase?: string;
  to?: string;
  display?: string;
  // On a `state`/`meta` event: the applied RFC 7386 merge document (the delta)
  // and the full derived document AFTER the change — what you read to decide your
  // reaction.
  merge?: Record<string, unknown>;
  document?: Record<string, unknown>;
};

// The delegation flavor. No longer on the wire (the binary's task primitive
// carries no discriminator); it travels in-band as a `[[handover]]`/`[[task]]`
// marker on the offer body and is tracked here so both legs drive the right flow.
export type DelegationMode = "handover" | "task";

// One in-flight task this node is a party to, tracked so the receiver and
// initiator legs can be told apart and the agent can be driven through it.
export type TaskRecord = {
  taskId: string;
  mode: DelegationMode;
  // The other party's nickname.
  peer: string;
  role: "initiator" | "receiver";
  // One-line summary of the offer, for prompts/notifications.
  task?: string;
};

export type PingResult = {
  author: string;
  rtt: number;
};

export type DiscoveredMesh = {
  mesh: string;
  name: string;
  peers: number;
  mode: "public" | "private";
};

export type Peer = {
  nickname: string;
  // "direct" => a live link (shown as "connected"); "gossip" => relayed.
  reach: "direct" | "gossip";
  // Self-reported by the peer; absent when it advertised none.
  model?: string;
  harness?: string;
  // The machine the peer runs on (its hostname); absent when not reported.
  host?: string;
  // Availability the peer advertises: "idle" (open, not working), "available"
  // (working but open), "busy" (not accepting work). Absent when not reported;
  // only "busy" means "don't send me work".
  status?: string;
  // null until the peer's first heartbeat is timed.
  lastSeenSecsAgo: number | null;
  quiet: boolean;
};
