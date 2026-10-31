// Tests of scripts/script-language/script-language.ts. Each case plants files in a scratch git
// repository and runs the check there.

import { spawnSync } from "node:child_process";
import { copyFileSync, mkdirSync, mkdtempSync, rmSync, writeFileSync } from "node:fs";
import { tmpdir } from "node:os";
import path from "node:path";
import { fileURLToPath } from "node:url";
import { afterAll, expect, test } from "bun:test";

const here = path.dirname(fileURLToPath(import.meta.url));
const work = mkdtempSync(path.join(tmpdir(), "script-language-"));
afterAll(() => rmSync(work, { recursive: true, force: true }));

// Plant `files` (path -> content) and an allow list in a fresh repository, then run the check.
function check(files: Record<string, string>, allow = ""): { status: number; out: string } {
  const repo = mkdtempSync(path.join(work, "repo-"));
  mkdirSync(path.join(repo, "scripts", "script-language"), { recursive: true });
  copyFileSync(
    path.join(here, "script-language.ts"),
    path.join(repo, "scripts", "script-language", "script-language.ts"),
  );
  writeFileSync(path.join(repo, "scripts", "script-language", "script-language.allow"), `${allow}\n`);
  for (const [file, content] of Object.entries(files)) {
    mkdirSync(path.dirname(path.join(repo, file)), { recursive: true });
    writeFileSync(path.join(repo, file), content);
  }
  spawnSync("git", ["init", "-q"], { cwd: repo });
  const proc = spawnSync(
    process.execPath,
    [path.join(repo, "scripts", "script-language", "script-language.ts")],
    { cwd: repo, encoding: "utf8" },
  );
  return { status: proc.status ?? -1, out: `${proc.stdout ?? ""}${proc.stderr ?? ""}` };
}

const expectExit = (files: Record<string, string>, expected: number, allow = "") => {
  const run = check(files, allow);
  expect(run.status, run.out).toBe(expected);
};

// Each kind of script fails.
test("a .sh file fails", () => {
  expectExit({ "scripts/build.sh": "echo hi\n" }, 1);
});
test("a .bash file fails", () => {
  expectExit({ "scripts/build.bash": "echo hi\n" }, 1);
});
test("a .py file fails", () => {
  expectExit({ "scripts/tool.py": "print('hi')\n" }, 1);
});
test("a shebang file with no extension fails (/bin/sh)", () => {
  expectExit({ "bin/tool": "#!/bin/sh\necho hi\n" }, 1);
});
test("a shebang file with no extension fails (env python3)", () => {
  expectExit({ "bin/tool": "#!/usr/bin/env python3\nprint('hi')\n" }, 1);
});

// Other names for the same languages.
test("a .zsh file fails", () => {
  expectExit({ "scripts/build.zsh": "echo hi\n" }, 1);
});
test("a .js file fails", () => {
  expectExit({ "scripts/tool.js": "console.log('hi')\n" }, 1);
});
test("a shebang in /usr/local/bin fails", () => {
  expectExit({ "bin/tool": "#!/usr/local/bin/bash\necho hi\n" }, 1);
});
test("a shebang with env -S fails", () => {
  expectExit({ "bin/tool": "#!/usr/bin/env -S bash -e\necho hi\n" }, 1);
});
test("a shebang in a file with another extension fails", () => {
  expectExit({ "bin/tool.txt": "#!/bin/sh\necho hi\n" }, 1);
});
test("a bun shebang with env -S passes", () => {
  expectExit({ "bin/tool": "#!/usr/bin/env -S bun run\nconsole.log('hi')\n" }, 0);
});

// A TypeScript script, a script with another shebang, and a file that mentions a shell in its text pass.
test("a clean tree passes", () => {
  expectExit({ "scripts/build.ts": "console.log('hi')\n", "README.md": "run it with bun\n" }, 0);
});
test("a bun shebang file with no extension passes", () => {
  expectExit({ "bin/tool": "#!/usr/bin/env bun\nconsole.log('hi')\n" }, 0);
});

// The allow list.
test("an allowed file passes", () => {
  expectExit({ "scripts/old.py": "print('hi')\n" }, 0, "extension|scripts/old.py|kept for a bench");
});
test("a stale row fails", () => {
  expectExit({ "scripts/build.ts": "console.log('hi')\n" }, 1, "extension|scripts/gone.sh|no longer there");
});
test("two rows for one file fail", () => {
  expectExit(
    { "scripts/old.py": "print('hi')\n" },
    1,
    "extension|scripts/old.py|one\nextension|scripts/old.py|two",
  );
});
