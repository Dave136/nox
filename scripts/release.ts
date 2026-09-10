#!/usr/bin/env bun

import assert from "node:assert";

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

  console.log("self-check OK");
}

if (import.meta.main) {
  if (process.argv.includes("--self-check")) {
    selfCheck();
  } else {
    console.log("release flow not implemented yet");
  }
}
