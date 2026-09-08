import assert from "node:assert/strict";
import { readFile } from "node:fs/promises";
import test from "node:test";

const root = new URL("../", import.meta.url);
const read = (path) => readFile(new URL(path, root), "utf8");

const sections = [
  "SiteHeader",
  "HeroSection",
  "ProofStrip",
  "ProductSection",
  "CapabilitiesSection",
  "HowItWorksSection",
  "LocalFirstSection",
  "OpenSourceSection",
  "WaitlistSection",
  "SiteFooter",
];

test("landing composes Tailwind Astro sections with Motion and global Geist", async () => {
  const [page, layout, signature, localFirst, css] = await Promise.all([
    read("src/pages/index.astro"),
    read("src/layouts/Layout.astro"),
    read("src/components/DeviceSyncSignature.astro"),
    read("src/components/LocalFirstSection.astro"),
    read("src/styles/global.css"),
  ]);

  for (const name of sections) {
    assert.match(page, new RegExp(`import ${name} from`));
    assert.match(page, new RegExp(`<${name} \\/?>`));
    await read(`src/components/${name}.astro`);
  }

  assert.match(css, /@import "tailwindcss"/);
  assert.match(css, /--font-sans:\s*"Geist"/);
  assert.match(css, /--font-mono:\s*"Geist Mono"/);
  assert.doesNotMatch(css, /\.(site-header|hero|section|network|waitlist|device-sync-signature)\b/);
  assert.match(layout, /from "motion"/);
  assert.match(layout, /prefers-reduced-motion/);
  assert.match(signature, /from "motion"/);
  assert.match(signature, /prefers-reduced-motion/);
  assert.match(signature, /w-\[660px\]/);
  assert.match(signature, /data-signature-device="desktop"/);
  assert.match(signature, /data-signature-device="phone"/);
  assert.doesNotMatch(signature, /data-sync-device=/);
  assert.doesNotMatch(signature, /rotate\s*:/);
  assert.match(signature, /data-sync-signal="line"/);
  assert.match(signature, /data-sync-signal="pulse"/);
  assert.match(signature, /data-sync-signal="encrypted"/);
  assert.match(signature, /data-sync-signal="pulse-ring"/);
  assert.match(signature, /h-\[150px\] w-px/);
  assert.match(signature, /ENCRYPTED/);
  assert.match(signature, /h-\[210px\] w-\[330px\]/);
  assert.match(signature, /h-\[260px\] w-\[148px\]/);
  assert.doesNotMatch(signature, /animate\('\[data-sync-orbit/);
  assert.match(signature, /data-sync-signal.*from "motion"/s);
  assert.match(localFirst, /data-sync-orbit="outer"/);
  assert.match(localFirst, /data-local-sync-device="tablet"/);
  assert.doesNotMatch(localFirst, /data-sync-device=/);
  assert.match(localFirst, /from "motion"/);
  assert.match(localFirst, /rotate\(360deg\)/);
  assert.match(localFirst, /@keyframes device-orbit/);
});
