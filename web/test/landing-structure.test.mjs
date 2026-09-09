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
  assert.match(signature, /data-sync-signal="pulse-shadow"/);
  assert.match(signature, /data-sync-signal="pulse-ring"/);
  assert.match(signature, /h-px w-\[90px\]/);
  assert.match(signature, /border-dashed/);
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

test("mobile nav island replaces the details dropdown", async () => {
  const [header, island] = await Promise.all([
    read("src/components/SiteHeader.astro"),
    read("src/components/MobileNavIsland.astro"),
  ]);

  assert.match(header, /import MobileNavIsland from/);
  assert.match(header, /<MobileNavIsland \/>/);
  assert.match(header, /data-site-header/);
  assert.doesNotMatch(header, /<details/);
  assert.doesNotMatch(header, /data-mobile-nav/);

  assert.match(island, /from "motion"/);
  assert.match(island, /island-motion/);
  assert.match(island, /prefers-reduced-motion/);
  assert.match(island, /data-island-shell/);
  assert.match(island, /data-island-toggle/);
  assert.match(island, /data-island-menu/);
  assert.match(island, /min-\[801px\]:hidden/);
  assert.match(island, /#171a1f/);
  assert.match(island, /aria-expanded/);
  assert.match(island, /openSequence/);
  assert.match(island, /closeSequence/);
  assert.match(island, /data-island-open/);
  assert.match(island, /scrollHeight/);
  assert.match(island, /data-island-chip-label/);
  assert.match(island, /Escape/);
  assert.match(island, /data-island-link/);
  assert.match(island, /resize/);

  for (const href of ["#product", "#security", "#how", "#open", "#waitlist"]) {
    assert.ok(island.includes(`"${href}"`), `island links to ${href}`);
  }
});
