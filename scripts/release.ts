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

type CommitInfo = { subject: string; body: string };

function buildChangelogDraft(commits: CommitInfo[]): string {
  const breaking: string[] = [];
  const bySection: Record<ChangelogSection, string[]> = {
    Added: [],
    Changed: [],
    Fixed: [],
  };

  for (const commit of commits) {
    const parsed = parseCommitSubject(commit.subject);
    if (!parsed) {
      continue;
    }
    const isBreaking = parsed.breaking || /^BREAKING CHANGE:/m.test(commit.body);
    if (isBreaking) {
      breaking.push(parsed.text);
      continue;
    }
    const section = classifyCommit(parsed);
    if (section) {
      bySection[section].push(parsed.text);
    }
  }

  const blocks: string[] = [];
  if (breaking.length > 0) {
    blocks.push(["### Breaking", "", ...breaking.map((t) => `- ${t}`), ""].join("\n"));
  }
  for (const section of ["Added", "Changed", "Fixed"] as const) {
    if (bySection[section].length > 0) {
      blocks.push(["", `### ${section}`, "", ...bySection[section].map((t) => `- ${t}`), ""].join("\n").replace(/^\n/, ""));
    }
  }
  return blocks.join("\n");
}

function bumpCargoToml(cargoToml: string, newVersion: string): string {
  return cargoToml.replace(/^version = "\d+\.\d+\.\d+"/m, `version = "${newVersion}"`);
}

function extractUnreleasedBody(changelog: string): string {
  const start = changelog.indexOf("## Unreleased");
  if (start === -1) {
    throw new Error("CHANGELOG.md has no `## Unreleased` section");
  }
  const afterHeading = changelog.indexOf("\n", start) + 1;
  const nextHeading = changelog.indexOf("\n## ", afterHeading);
  const end = nextHeading === -1 ? changelog.length : nextHeading + 1;
  return changelog.slice(afterHeading, end);
}

function isUnreleasedEmpty(body: string): boolean {
  return !/^- /m.test(body);
}

function renderChangelogRelease(
  changelog: string,
  newVersion: string,
  date: string,
  body: string,
): string {
  const start = changelog.indexOf("## Unreleased");
  const afterHeading = changelog.indexOf("\n", start) + 1;
  const nextHeading = changelog.indexOf("\n## ", afterHeading);
  const end = nextHeading === -1 ? changelog.length : nextHeading + 1;

  const freshUnreleased = [
    "## Unreleased",
    "",
    "### Added",
    "",
    "### Changed",
    "",
    "### Fixed",
    "",
    "",
  ].join("\n");
  const releasedSection = `## v${newVersion} - ${date}\n\n${body}`;

  return changelog.slice(0, start) + freshUnreleased + releasedSection + changelog.slice(end);
}

function parseRemoteUrl(url: string): { owner: string; repo: string } {
  const match = url.match(/github\.com[:/]([^/]+)\/(.+?)(?:\.git)?$/);
  if (!match) {
    throw new Error(`could not parse a GitHub owner/repo from remote URL: ${url}`);
  }
  return { owner: match[1], repo: match[2] };
}

class UserAbortedError extends Error {
  constructor() {
    super("aborted by user");
  }
}

async function select(title: string, options: string[]): Promise<number> {
  const readline = await import("node:readline");
  return new Promise((resolve, reject) => {
    let index = 0;

    function render() {
      console.log(title);
      for (const [i, option] of options.entries()) {
        console.log(`${i === index ? ">" : " "} ${option}`);
      }
    }

    function clear() {
      // Move cursor up (options.length + 1 lines for the title) and clear each.
      for (let i = 0; i < options.length + 1; i++) {
        process.stdout.write("\x1b[1A\x1b[2K");
      }
    }

    readline.emitKeypressEvents(process.stdin);
    const wasRaw = process.stdin.isRaw ?? false;
    process.stdin.setRawMode(true);
    process.stdin.resume();

    function cleanup() {
      process.stdin.setRawMode(wasRaw);
      process.stdin.pause();
      process.stdin.removeListener("keypress", onKeypress);
    }

    function onKeypress(_str: string, key: { name: string; ctrl: boolean }) {
      if (key.ctrl && key.name === "c") {
        clear();
        cleanup();
        reject(new UserAbortedError());
        return;
      }
      if (key.name === "escape") {
        clear();
        cleanup();
        reject(new UserAbortedError());
        return;
      }
      if (key.name === "up") {
        clear();
        index = (index - 1 + options.length) % options.length;
        render();
        return;
      }
      if (key.name === "down") {
        clear();
        index = (index + 1) % options.length;
        render();
        return;
      }
      if (key.name === "return") {
        clear();
        cleanup();
        resolve(index);
      }
    }

    process.stdin.on("keypress", onKeypress);
    render();
  });
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

  const draft = buildChangelogDraft([
    { subject: "feat: add change password", body: "feat: add change password" },
    { subject: "fix: render blocks", body: "fix: render blocks" },
    { subject: "chore: bump ci", body: "chore: bump ci" },
    {
      subject: "feat!: drop legacy vault format",
      body: "feat!: drop legacy vault format\n\nBREAKING CHANGE: old vaults must be re-created",
    },
    { subject: "not a conventional commit", body: "not a conventional commit" },
  ]);
  assert.strictEqual(
    draft,
    [
      "### Breaking",
      "",
      "- drop legacy vault format",
      "",
      "### Added",
      "",
      "- add change password",
      "",
      "### Fixed",
      "",
      "- render blocks",
      "",
    ].join("\n"),
  );

  assert.strictEqual(buildChangelogDraft([{ subject: "chore: x", body: "chore: x" }]), "");

  const bumpedToml = bumpCargoToml(sampleToml, "1.2.4");
  assert.ok(bumpedToml.includes('version = "1.2.4"'));
  assert.ok(!bumpedToml.includes('version = "1.2.3"'));

  const sampleChangelog = [
    "# Changelog",
    "",
    "## Unreleased",
    "",
    "### Added",
    "",
    "### Changed",
    "",
    "### Fixed",
    "",
    "## v0.1.0 - 2026-01-01",
    "",
    "### Added",
    "",
    "- first release",
    "",
  ].join("\n");

  const emptyBody = extractUnreleasedBody(sampleChangelog);
  assert.ok(emptyBody.includes("### Added"));
  assert.ok(!emptyBody.includes("v0.1.0"));
  assert.strictEqual(isUnreleasedEmpty(emptyBody), true);

  const filledBody = "### Added\n\n- new thing\n\n### Changed\n\n### Fixed\n\n";
  assert.strictEqual(isUnreleasedEmpty(filledBody), false);

  const released = renderChangelogRelease(sampleChangelog, "1.2.4", "2026-09-10", filledBody);
  assert.ok(released.includes("## v1.2.4 - 2026-09-10"));
  assert.ok(released.includes("- new thing"));
  assert.ok(released.includes("## Unreleased"));
  assert.ok(released.indexOf("## Unreleased") < released.indexOf("## v1.2.4"));
  assert.ok(released.includes("## v0.1.0 - 2026-01-01"));

  assert.deepStrictEqual(parseRemoteUrl("git@github.com:Dave136/nox.git"), {
    owner: "Dave136",
    repo: "nox",
  });
  assert.deepStrictEqual(parseRemoteUrl("https://github.com/Dave136/nox.git"), {
    owner: "Dave136",
    repo: "nox",
  });
  assert.deepStrictEqual(parseRemoteUrl("https://github.com/Dave136/nox"), {
    owner: "Dave136",
    repo: "nox",
  });

  console.log("self-check OK");
}

if (import.meta.main) {
  if (process.argv.includes("--self-check")) {
    selfCheck();
  } else {
    console.log("release flow not implemented yet");
  }
}
