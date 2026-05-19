import { execSync } from "node:child_process";
import type { ExtensionContext } from "@earendil-works/pi-coding-agent";

export function isAscii(text: string): boolean {
  for (let i = 0; i < text.length; i++) {
    if (text.charCodeAt(i) > 0x7f) return false;
  }
  return true;
}

// Mirrors the Rust SwarmName invariant (src/protocol/ticket.rs::SwarmName::new).
const SWARM_NAME_RE = /^[a-zA-Z0-9_-]{1,12}$/;

export function isValidSwarmName(name: string): boolean {
  return SWARM_NAME_RE.test(name);
}

export function agentSwarmAvailable(): boolean {
  try {
    execSync("which ahs", { stdio: "ignore" });
    return true;
  } catch {
    return false;
  }
}

export function requireAgentSwarm(ctx: ExtensionContext): boolean {
  if (!agentSwarmAvailable()) {
    ctx.ui.notify("🐝 ahs CLI not found on PATH", "error");
    return false;
  }
  return true;
}

export function runSwarmCommand(args: string[]): string {
  return execSync(`ahs ${args.map((arg) => `"${arg}"`).join(" ")}`, {
    encoding: "utf-8",
    timeout: 15_000,
  }).trim();
}

export function formatSwarmMessage(text: string): string {
  const lines = text.split("\n");
  if (lines.length === 1) {
    return `🐝 ${lines[0]}`;
  }
  return `🐝\n${lines.join("\n")}`;
}
