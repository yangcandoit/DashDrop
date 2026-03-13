import test from "node:test";
import assert from "node:assert/strict";
import fs from "node:fs/promises";
import path from "node:path";
import { fileURLToPath } from "node:url";

const __dirname = path.dirname(fileURLToPath(import.meta.url));
const workspaceRoot = path.resolve(__dirname, "..");

async function loadTauriConfig() {
  const configPath = path.join(workspaceRoot, "src-tauri", "tauri.conf.json");
  const raw = await fs.readFile(configPath, "utf8");
  return JSON.parse(raw);
}

test("tauri bundle registers alternate Open With associations for common share targets", async () => {
  const config = await loadTauriConfig();
  const associations = config.bundle?.fileAssociations;

  assert.ok(Array.isArray(associations), "bundle.fileAssociations should exist");

  const expected = [
    ["DashDrop Plain Text Share", "text/plain", ["txt"]],
    ["DashDrop Markdown Share", "text/markdown", ["md"]],
    ["DashDrop CSV Share", "text/csv", ["csv"]],
    ["DashDrop JSON Share", "application/json", ["json"]],
    ["DashDrop Rich Text Share", "application/rtf", ["rtf"]],
    ["DashDrop PDF Share", "application/pdf", ["pdf"]],
    ["DashDrop Word Share", "application/msword", ["doc"]],
    [
      "DashDrop Word OpenXML Share",
      "application/vnd.openxmlformats-officedocument.wordprocessingml.document",
      ["docx"],
    ],
    ["DashDrop Excel Share", "application/vnd.ms-excel", ["xls"]],
    [
      "DashDrop Excel OpenXML Share",
      "application/vnd.openxmlformats-officedocument.spreadsheetml.sheet",
      ["xlsx"],
    ],
    ["DashDrop PowerPoint Share", "application/vnd.ms-powerpoint", ["ppt"]],
    [
      "DashDrop PowerPoint OpenXML Share",
      "application/vnd.openxmlformats-officedocument.presentationml.presentation",
      ["pptx"],
    ],
    ["DashDrop Image Share", "image/*", ["png", "jpg", "heic"]],
    ["DashDrop Audio Share", "audio/*", ["mp3", "wav"]],
    ["DashDrop Video Share", "video/*", ["mp4", "mov"]],
    ["DashDrop Zip Share", "application/zip", ["zip"]],
    ["DashDrop 7-Zip Share", "application/x-7z-compressed", ["7z"]],
    ["DashDrop Tar Share", "application/x-tar", ["tar"]],
    ["DashDrop Gzip Share", "application/gzip", ["gz", "tgz"]],
  ];

  for (const [name, mimeType, extSample] of expected) {
    const association = associations.find((entry) => entry.name === name);
    assert.ok(association, `missing association ${name}`);
    assert.equal(association.role, "Viewer");
    assert.equal(association.rank, "Alternate");
    assert.equal(association.mimeType, mimeType);
    for (const ext of extSample) {
      assert.ok(
        association.ext.includes(ext),
        `${name} should include .${ext}`,
      );
    }
  }
});

test("tauri bundle metadata explains desktop handoff intent", async () => {
  const config = await loadTauriConfig();

  assert.equal(config.bundle?.category, "Utility");
  assert.match(config.bundle?.shortDescription ?? "", /Open With/i);
  assert.match(config.bundle?.longDescription ?? "", /Finder|Explorer|share queue/i);
});
