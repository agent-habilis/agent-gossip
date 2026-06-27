import { execSync } from "node:child_process";
import type { ExtensionContext } from "@earendil-works/pi-coding-agent";
import { notifyError } from "./ui";

function isControlChar(ch: string): boolean {
  const code = ch.codePointAt(0) ?? 0;
  // Unicode control: C0 (0x00-0x1F) plus DEL + C1 (0x7F-0x9F).
  return code <= 0x1f || (code >= 0x7f && code <= 0x9f);
}

// Mirrors the Rust MessageBody::new invariant: any UTF-8 is allowed; the
// only restriction is control characters other than tab / newline / CR.
export function isValidBody(text: string): boolean {
  for (const ch of text) {
    if (isControlChar(ch) && ch !== "\n" && ch !== "\t" && ch !== "\r") return false;
  }
  return true;
}

// Mirrors the Rust SwarmName invariant (src/protocol/swarm.rs::SwarmName::new):
// 1-32 Unicode scalar values, excluding control characters, whitespace, and
// any of / \ < > #. (The daemon additionally rejects bidi-control scalars; it
// is the authoritative backstop, so this client check stays simple.)
export function isValidSwarmName(name: string): boolean {
  const chars = [...name];
  if (chars.length < 1 || chars.length > 32) return false;
  return !chars.some((ch) => isControlChar(ch) || /\s/u.test(ch) || "/\\<>#".includes(ch));
}

export function agentSwarmAvailable(): boolean {
  try {
    execSync("which ahs", { stdio: "ignore" });
    return true;
  } catch {
    return false;
  }
}

export function requireAgentSwarm(_ctx: ExtensionContext): boolean {
  if (!agentSwarmAvailable()) {
    notifyError("ahs CLI not found on PATH");
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
