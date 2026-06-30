export type Session = {
  swarm: string;
  name: string;
  nickname: string;
  pid?: number;
  // Set when the `ready` event reports the installed extension has fallen
  // behind the `ahsw` binary — surfaced once at swarm start.
  drift?: string;
};

export type SwarmEvent = {
  event: string;
  type?: string;
  subtype?: string;
  author?: string;
  body?: string;
  id?: string;
  reply?: string | null;
  self?: boolean;
  swarm?: string;
  nickname?: string;
  // On `exchange` / `exchange_progress` events.
  exchange_id?: string;
  kind?: ExchangeKind;
  phase?: string;
  to?: string;
  display?: string;
  // On a `state` event: the applied RFC 6902 op array (the delta) and the full
  // derived document AFTER the change — what you read to decide your reaction.
  patch?: Array<Record<string, unknown>>;
  document?: Record<string, unknown>;
};

export type ExchangeKind = "handover" | "task";

// One in-flight exchange this node is a party to, tracked so the receiver and
// initiator legs can be told apart and the agent can be driven through it.
export type ExchangeRecord = {
  exchangeId: string;
  kind: ExchangeKind;
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

export type DiscoveredSwarm = {
  swarm: string;
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
  // null until the peer's first heartbeat is timed.
  lastSeenSecsAgo: number | null;
  quiet: boolean;
};
