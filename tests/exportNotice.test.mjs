import test from "node:test";
import assert from "node:assert/strict";
import {
  EXPORT_PLAINTEXT_NOTICE,
  batchExportPartial,
  batchExportSuccess,
  exportDurabilityWarning,
  singleExportSuccess,
} from "../src/lib/exportNotice.js";

test("单文件导出成功提示包含未加密副本警告", () => {
  const text = singleExportSuccess("照片.jpg");
  assert.match(text, /导出成功/);
  assert.match(text, /照片\.jpg/);
  assert.ok(text.includes(EXPORT_PLAINTEXT_NOTICE));
  assert.match(text, /未加密副本/);
  assert.match(text, /不再受 Haven 保护/);
});

test("批量导出成功提示包含未加密副本警告", () => {
  const text = batchExportSuccess("批量导出", 3);
  assert.match(text, /批量导出完成/);
  assert.match(text, /3 个文件/);
  assert.ok(text.includes(EXPORT_PLAINTEXT_NOTICE));
});

test("文件夹导出成功提示包含未加密副本警告", () => {
  const text = batchExportSuccess("文件夹导出", 1);
  assert.match(text, /文件夹导出完成/);
  assert.match(text, /1 个文件/);
  assert.ok(text.includes(EXPORT_PLAINTEXT_NOTICE));
});

test("警告文案强调明文且不弱化保护范围", () => {
  assert.match(EXPORT_PLAINTEXT_NOTICE, /未加密副本/);
  assert.match(EXPORT_PLAINTEXT_NOTICE, /不再受 Haven 保护/);
  assert.match(EXPORT_PLAINTEXT_NOTICE, /妥善保管/);
});

test("批量部分成功保留统计并追加明文警告", () => {
  const text = batchExportPartial("批量导出", 2, 1);
  assert.match(text, /批量导出完成/);
  assert.match(text, /成功 2 个/);
  assert.match(text, /失败 1 个/);
  assert.ok(text.includes(EXPORT_PLAINTEXT_NOTICE));
  assert.match(text, /未加密副本/);
});

test("批量全部失败只保留统计，不显示明文警告", () => {
  const text = batchExportPartial("文件夹导出", 0, 3);
  assert.match(text, /文件夹导出完成/);
  assert.match(text, /成功 0 个/);
  assert.match(text, /失败 3 个/);
  assert.ok(!text.includes("未加密副本"));
  assert.ok(!text.includes("不再受 Haven 保护"));
});

test("持久化未确认时同时保留耐久性告警与明文警告", () => {
  const warning = "文件已生成，但系统尚未确认持久化完成；请稍后检查目标位置。";
  const text = exportDurabilityWarning(warning);
  assert.ok(text.startsWith(warning));
  assert.match(text, /尚未确认持久化完成/);
  assert.ok(text.includes(EXPORT_PLAINTEXT_NOTICE));
  assert.ok(!/导出失败/.test(text));
  assert.ok(!/导出成功/.test(text));
});
