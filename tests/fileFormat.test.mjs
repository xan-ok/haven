import test from "node:test";
import assert from "node:assert/strict";

import { fileTypeCategory, fileTypeLabel, formatFileSize, formatFileTime } from "../src/lib/fileFormat.js";

test("formatFileSize keeps small values in bytes", () => {
  assert.equal(formatFileSize(0), "0 B");
  assert.equal(formatFileSize(1023), "1023 B");
});

test("formatFileSize scales to KB and MB", () => {
  assert.equal(formatFileSize(1024), "1.0 KB");
  assert.equal(formatFileSize(1536), "1.5 KB");
  assert.equal(formatFileSize(5 * 1024 * 1024), "5.0 MB");
});

test("formatFileSize rejects invalid input", () => {
  assert.equal(formatFileSize(-1), "—");
  assert.equal(formatFileSize(Number.NaN), "—");
  assert.equal(formatFileSize(undefined), "—");
});

test("formatFileTime rejects missing timestamps", () => {
  assert.equal(formatFileTime(0), "—");
  assert.equal(formatFileTime(-10), "—");
});

test("fileTypeLabel 根据名称给出展示标签", () => {
  assert.equal(fileTypeLabel("photo.png"), "PNG");
  assert.equal(fileTypeLabel("photo.JPG"), "JPG");
  assert.equal(fileTypeLabel("photo.jpeg"), "JPEG");
  assert.equal(fileTypeLabel("doc.pdf"), "PDF");
  assert.equal(fileTypeLabel("archive.zip"), "文件");
  assert.equal(fileTypeLabel(undefined), "文件");
});

test("fileTypeCategory 与展示标签使用同一套扩展名规则", () => {
  assert.equal(fileTypeCategory("photo.png"), "image");
  assert.equal(fileTypeCategory("photo.JPG"), "image");
  assert.equal(fileTypeCategory("photo.jpeg"), "image");
  assert.equal(fileTypeCategory("doc.Pdf"), "pdf");
  assert.equal(fileTypeCategory("archive.zip"), "other");
  assert.equal(fileTypeCategory("README"), "other");
  assert.equal(fileTypeCategory(undefined), "other");
});
