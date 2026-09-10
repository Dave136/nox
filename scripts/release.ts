#!/usr/bin/env bun

import assert from "node:assert";

type Version = { major: number; minor: number; patch: number };
type BumpKind = "major" | "minor" | "patch";
type ParsedCommit = { type: string; scope: string | null; breaking: boolean; text: string };
type ChangelogSection = "Added" | "Changed" | "Fixed";

function parseVersion(cargoToml: string): Version {
  const match = cargoToml.match(/^version = "(\d+)\.(\d+)\.(\d+)"/m);
  if (!match) {
    throw new Error("could not find a bare `version = \"X.Y.Z\"` line in Cargo.toml");
  }
  return {
    major: Number(match[1]),
    minor: Number(match[2]),
    patch: Number(match[3]),
  };
}

function bumpVersion(v: Version, kind: BumpKind): Version {
  switch (kind) {
    case "major":
      return { major: v.major + 1, minor: 0, patch: 0 };
    case "minor":
      return { major: v.major, minor: v.minor + 1, patch: 0 };
    case "patch":
      return { major: v.major, minor: v.minor, patch: v.patch + 1 };
  }
}

function formatVersion(v: Version): string {
  return `${v.major}.${v.minor}.${v.patch}`;
}

class CommandError extends Error {
  constructor(
    public cmd: string[],
    public code: number,
    public stderr: string,
  ) {
    super(`command failed (exit ${code}): ${cmd.join(" ")}\n${stderr}`);
  }
}

function run(
  cmd: string[],
  opts: { cwd?: string; allowFailure?: boolean } = {},
): { code: number; stdout: string; stderr: string } {
  const proc = Bun.spawnSync(cmd, {
    cwd: opts.cwd,
    stdout: "pipe",
    stderr: "pipe",
  });
  const stdout = proc.stdout.toString();
  const stderr = proc.stderr.toString();
  if (proc.exitCode !== 0 && !opts.allowFailure) {
    throw new CommandError(cmd, proc.exitCode, stderr);
  }
  return { code: proc.exitCode, stdout, stderr };
}

function parseCommitSubject(subject: string): ParsedCommit | null {
  const match = subject.match(/^(\w+)(\(([^)]+)\))?(!)?: (.+)$/);
  if (!match) {
    return null;
  }
  return {
    type: match[1],
    scope: match[3] ?? null,
    breaking: match[4] === "!",
    text: match[5],
  };
}

function classifyCommit(parsed: ParsedCommit): ChangelogSection | null {
  switch (parsed.type) {
    case "feat":
      return "Added";
    case "fix":
      return "Fixed";
    case "perf":
      return "Changed";
    default:
      return null;
  }
}

function selfCheck(): void {
  const ok = run(["true"]);
  assert.strictEqual(ok.code, 0, "run() should report exit code 0 for `true`");

  let threw = false;
  try {
    run(["false"]);
  } catch (err) {
    threw = err instanceof CommandError;
  }
  assert.ok(threw, "run() should throw CommandError on non-zero exit");

  const allowed = run(["false"], { allowFailure: true });
  assert.strictEqual(allowed.code, 1, "run() with allowFailure should return the exit code instead of throwing");

  const sampleToml = `[workspace]\nresolver = "2"\n\n[workspace.package]\nedition = "2024"\nversion = "1.2.3"\n`;
  const parsed = parseVersion(sampleToml);
  assert.deepStrictEqual(parsed, { major: 1, minor: 2, patch: 3 });

  assert.deepStrictEqual(bumpVersion(parsed, "patch"), { major: 1, minor: 2, patch: 4 });
  assert.deepStrictEqual(bumpVersion(parsed, "minor"), { major: 1, minor: 3, patch: 0 });
  assert.deepStrictEqual(bumpVersion(parsed, "major"), { major: 2, minor: 0, patch: 0 });

  assert.strictEqual(formatVersion(parsed), "1.2.3");

  assert.deepStrictEqual(parseCommitSubject("feat: add change password"), {
    type: "feat",
    scope: null,
    breaking: false,
    text: "add change password",
  });
  assert.deepStrictEqual(parseCommitSubject("fix(secure-notes): render blocks"), {
    type: "fix",
    scope: "secure-notes",
    breaking: false,
    text: "render blocks",
  });
  assert.deepStrictEqual(parseCommitSubject("feat!: drop legacy format"), {
    type: "feat",
    scope: null,
    breaking: true,
    text: "drop legacy format",
  });
  assert.strictEqual(parseCommitSubject("just a plain commit message"), null);

  assert.strictEqual(classifyCommit({ type: "feat", scope: null, breaking: false, text: "x" }), "Added");
  assert.strictEqual(classifyCommit({ type: "fix", scope: null, breaking: false, text: "x" }), "Fixed");
  assert.strictEqual(classifyCommit({ type: "perf", scope: null, breaking: false, text: "x" }), "Changed");
  assert.strictEqual(classifyCommit({ type: "chore", scope: null, breaking: false, text: "x" }), null);
  assert.strictEqual(classifyCommit({ type: "refactor", scope: null, breaking: false, text: "x" }), null);

  console.log("self-check OK");
}

if (import.meta.main) {
  if (process.argv.includes("--self-check")) {
    selfCheck();
  } else {
    console.log("release flow not implemented yet");
  }
}
