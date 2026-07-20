import { readFileSync } from "node:fs";
import { resolve } from "node:path";
import { describe, expect, it } from "vitest";

const demoCss = readFileSync(resolve(process.cwd(), "src/demo.css"), "utf8");
const stylesCss = readFileSync(resolve(process.cwd(), "src/styles.css"), "utf8");
const reactCss = readFileSync(resolve(process.cwd(), "src/react/react-overrides.css"), "utf8");

function blockFor(css: string, selector: string): string {
  const start = css.indexOf(`${selector} {`);
  if (start === -1) throw new Error(`Missing CSS block: ${selector}`);
  const end = css.indexOf("}", start);
  return css.slice(start, end + 1);
}

function hexProperty(block: string, property: string): string {
  const match = block.match(new RegExp(`${property}:\\s*(#[0-9a-fA-F]{6})`));
  if (!match) throw new Error(`Missing hex property: ${property}`);
  return match[1];
}

function pxProperty(block: string, property: string): number {
  const match = block.match(new RegExp(`${property}:\\s*(\\d+)px`));
  if (!match) throw new Error(`Missing pixel property: ${property}`);
  return Number.parseInt(match[1], 10);
}

function matchedHex(css: string, pattern: RegExp, label: string): string {
  const match = css.match(pattern);
  if (!match) throw new Error(`Missing effective color: ${label}`);
  return match[1];
}

function matchedPx(css: string, pattern: RegExp, label: string): number {
  const match = css.match(pattern);
  if (!match) throw new Error(`Missing effective font size: ${label}`);
  return Number.parseInt(match[1], 10);
}

function channel(value: number): number {
  const normalized = value / 255;
  return normalized <= 0.04045
    ? normalized / 12.92
    : ((normalized + 0.055) / 1.055) ** 2.4;
}

function luminance(hex: string): number {
  const channels = [
    Number.parseInt(hex.slice(1, 3), 16),
    Number.parseInt(hex.slice(3, 5), 16),
    Number.parseInt(hex.slice(5, 7), 16),
  ].map(channel);
  return channels[0] * 0.2126 + channels[1] * 0.7152 + channels[2] * 0.0722;
}

function contrast(foreground: string, background: string): number {
  const lighter = Math.max(luminance(foreground), luminance(background));
  const darker = Math.min(luminance(foreground), luminance(background));
  return (lighter + 0.05) / (darker + 0.05);
}

describe("dark research inspector contrast", () => {
  it("keeps content, compact metadata, headings, and links readable", () => {
    const darkTheme = blockFor(demoCss, "[data-theme=\"dark\"]");
    const background = hexProperty(darkTheme, "--workspace");
    const darkInspectorStart = reactCss.indexOf("[data-theme=\"dark\"] .research-inspector {");
    const foregrounds = darkInspectorStart >= 0
      ? Object.fromEntries([
          "--inspector-content",
          "--inspector-muted",
          "--inspector-accent",
          "--inspector-link",
        ].map((property) => [property, hexProperty(
          reactCss.slice(darkInspectorStart, reactCss.indexOf("}", darkInspectorStart) + 1),
          property,
        )]))
      : {
          "--inspector-content": matchedHex(stylesCss, /\.inspector-section p,[\s\S]*?\{\s*color:\s*(#[0-9a-fA-F]{6})/, "content"),
          "--inspector-muted": matchedHex(stylesCss, /\.inspector-section small,[\s\S]*?\{[\s\S]*?color:\s*(#[0-9a-fA-F]{6})/, "muted"),
          "--inspector-accent": matchedHex(stylesCss, /\.inspector-section h3\s*\{[\s\S]*?color:\s*(#[0-9a-fA-F]{6})/, "accent"),
          "--inspector-link": matchedHex(stylesCss, /\.inspector-source-list a,[\s\S]*?\{\s*color:\s*(#[0-9a-fA-F]{6})/, "link"),
        };

    for (const [property, foreground] of Object.entries(foregrounds)) {
      expect(contrast(foreground, background), property).toBeGreaterThanOrEqual(4.5);
    }
  });

  it("keeps inspector body copy, metadata, and labels above compact readability floors", () => {
    const typographyStart = reactCss.indexOf("\n.research-inspector {");
    const typography = typographyStart >= 0
      ? Object.fromEntries([
          "--inspector-body-size",
          "--inspector-meta-size",
          "--inspector-label-size",
        ].map((property) => [property, pxProperty(
          reactCss.slice(typographyStart, reactCss.indexOf("}", typographyStart) + 1),
          property,
        )]))
      : {
          "--inspector-body-size": matchedPx(stylesCss, /\.inspector-section p,[\s\S]*?\{[\s\S]*?font-size:\s*(\d+)px/, "body"),
          "--inspector-meta-size": matchedPx(stylesCss, /\.inspector-section small,[\s\S]*?\{[\s\S]*?font-size:\s*(\d+)px/, "metadata"),
          "--inspector-label-size": matchedPx(stylesCss, /\.inspector-section h3,[\s\S]*?\{[\s\S]*?font-size:\s*(\d+)px/, "labels"),
        };

    expect(typography["--inspector-body-size"]).toBeGreaterThanOrEqual(13);
    expect(typography["--inspector-meta-size"]).toBeGreaterThanOrEqual(11);
    expect(typography["--inspector-label-size"]).toBeGreaterThanOrEqual(11);
  });

  it("keeps the model settings rail on the active theme palette", () => {
    expect(blockFor(reactCss, ".settings-dialog .model-profile-list")).toContain("background: transparent");
    expect(blockFor(reactCss, ".settings-dialog .model-profile-row:hover")).toContain("background: var(--hover)");
    const selected = blockFor(reactCss, ".settings-dialog .model-profile-row.is-selected");
    expect(selected).toContain("border-left-color: var(--accent)");
    expect(selected).toContain("background: var(--selected)");
    expect(blockFor(reactCss, ".settings-dialog .profile-state")).toContain("color: var(--success)");
  });
});
