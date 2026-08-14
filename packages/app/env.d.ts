declare module '*.css'

/**
 * WebMCP — the W3C Web Machine Learning CG draft that lets a page publish tools
 * to an agent. No browser ships it on by default, so `Document.modelContext` is
 * declared optional even though the IDL has it required: every caller has to
 * feature-detect, and the type should make that impossible to forget.
 *
 * Written by hand rather than pulled from a package. The surface is small, it
 * is the only thing standing between us and a dependency, and the spec is still
 * moving — pinning our own copy means a spec change shows up as a type error we
 * chose to look at.
 *
 * Verified against Chrome 151 with `--enable-features=WebMCP`.
 */
interface ToolAnnotations {
  /** The tool does not change state. A hint to the agent, not enforcement. */
  readOnlyHint?: boolean
  /** The tool's output carries content this origin does not vouch for. */
  untrustedContentHint?: boolean
}

interface ModelContextTool {
  /** 1–128 chars of ASCII alphanumeric, `_`, `-` or `.`. Unique per document. */
  name: string
  title?: string
  description: string
  inputSchema?: object
  annotations?: ToolAnnotations
  /**
   * The browser does **not** validate the input against `inputSchema` — a call
   * omitting a `required` field arrives here with that field `undefined`. Hence
   * the untyped bag: every tool validates its own arguments.
   */
  execute(input: Record<string, unknown>): Promise<unknown>
}

/** A tool as a *caller* sees it. Note `inputSchema` arrives serialized. */
interface RegisteredTool {
  name: string
  title?: string
  description: string
  /** JSON Schema as a string, per the IDL's `DOMString`. */
  inputSchema?: string
  origin: string
  window: Window
  annotations?: ToolAnnotations
}

interface ModelContextRegisterToolOptions {
  /** Aborting unregisters the tool. This is the only way to remove one. */
  signal?: AbortSignal
  exposedTo?: string[]
}

interface ModelContext extends EventTarget {
  registerTool(tool: ModelContextTool, options?: ModelContextRegisterToolOptions): Promise<void>
  getTools(options?: { fromOrigins?: string[] }): Promise<RegisteredTool[]>
  /**
   * Takes the `RegisteredTool` from [`getTools`] — *not* a tool name — and a
   * JSON **string** of arguments. Passing an object throws "Failed to parse
   * input arguments". The resolved value is always serialized to a string.
   */
  executeTool(tool: RegisteredTool, input?: string): Promise<unknown>
  ontoolchange: ((this: ModelContext, event: Event) => unknown) | null
}

interface Document {
  readonly modelContext?: ModelContext
}

/**
 * The crate version, substituted at build time from
 * `crates/agent-gossip/Cargo.toml` so the browser's `gossip_version` cannot
 * drift from the binary's. Declared `var` because `Bun.build`'s `define` is a
 * textual substitution, so it never exists as a real binding.
 */
declare var __APP_VERSION__: string

/** Content-addressed URL of the gossip client's `.wasm`, set at build time. */
declare var __WASM_PATH__: string
