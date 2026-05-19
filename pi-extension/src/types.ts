export type Session = {
  swarm: string;
  name: string;
  nickname: string;
  pid?: number;
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
};

export type PingResult = {
  author: string;
  rtt: number;
};

export type NotifyType = "info" | "warning" | "error";
