import test from "node:test";
import assert from "node:assert/strict";

import {
  FILE_NAME_MAX_LENGTH,
  baseNameSelection,
  createRenameController,
  validateFileName,
} from "../src/lib/renameController.js";
import { createFileListStore } from "../src/lib/fileListStore.js";

const fileA = { id: "a", name: "photo.jpg", size: 10, importedAt: 100 };
const fileB = { id: "b", name: "notes", size: 20, importedAt: 200 };

test("validateFileName 接受正常名称并去除首尾空格", () => {
  assert.deepEqual(validateFileName("  假期 照片.jpg  ", "photo.jpg"), {
    ok: true,
    name: "假期 照片.jpg",
    code: null,
    message: "",
  });
  assert.equal(validateFileName("photo.JPG", "photo.jpg").ok, true);
});

test("空名称与纯空白被拒绝", () => {
  for (const value of ["", "   ", "\t\n", undefined, null]) {
    const result = validateFileName(value, "photo.jpg");
    assert.equal(result.ok, false);
    assert.equal(result.code, "FILE_NAME_EMPTY");
  }
});

test("长度上限为 100 个字符", () => {
  const max = `${"长".repeat(FILE_NAME_MAX_LENGTH - 4)}.jpg`;
  assert.equal([...max].length, 100);
  assert.equal(validateFileName(max, "a.jpg").ok, true);

  const tooLong = `${"长".repeat(FILE_NAME_MAX_LENGTH - 3)}.jpg`;
  const result = validateFileName(tooLong, "a.jpg");
  assert.equal(result.ok, false);
  assert.equal(result.code, "FILE_NAME_TOO_LONG");
});

test("非法字符与方向控制符被拒绝", () => {
  for (const value of ["a/b.jpg", "a\\b.jpg", "a:b.jpg", "a\u202Eb.jpg", "a\nb.jpg", ".", ".."]) {
    const result = validateFileName(value, "a.jpg");
    assert.equal(result.ok, false, value);
    assert.equal(result.code, "FILE_NAME_INVALID");
  }
});

test("扩展名变化被拒绝，保持文件类型分类不变", () => {
  for (const value of ["photo.png", "photo", "photo.jpg.txt"]) {
    const result = validateFileName(value, "photo.jpg");
    assert.equal(result.ok, false, value);
    assert.equal(result.code, "FILE_NAME_EXTENSION_LOCKED");
  }
  assert.equal(validateFileName("notes.txt", "notes").ok, false);
  assert.equal(validateFileName("我的笔记", "notes").ok, true);
});

test("baseNameSelection 默认只选中主文件名", () => {
  assert.deepEqual(baseNameSelection("photo.jpg"), { start: 0, end: 5 });
  assert.deepEqual(baseNameSelection("archive.tar.gz"), { start: 0, end: 11 });
  assert.deepEqual(baseNameSelection("notes"), { start: 0, end: 5 });
  assert.deepEqual(baseNameSelection(".env"), { start: 0, end: 4 });
});

test("取消重命名不调用 IPC，确认后也是空操作", async () => {
  const calls = [];
  const controller = createRenameController({
    renameById: async (id, name) => calls.push([id, name]),
  });

  assert.equal(controller.open(fileA), true);
  assert.equal(controller.getState().draft, "photo.jpg");
  assert.equal(controller.cancel(), true);
  assert.equal(await controller.confirm(), null);
  assert.deepEqual(calls, []);
  assert.deepEqual(controller.getState(), {
    target: null,
    draft: "",
    busy: false,
    error: null,
  });
});

test("确认成功只调用一次 IPC 并返回后端名称", async () => {
  const calls = [];
  const controller = createRenameController({
    renameById: async (id, name) => {
      calls.push([id, name]);
      return { id, name, size: 10, importedAt: 100 };
    },
  });
  controller.open(fileA);
  controller.setDraft("  假期 照片.jpg  ");

  const outcome = await controller.confirm();

  assert.deepEqual(calls, [["a", "假期 照片.jpg"]]);
  assert.equal(outcome.status, "committed");
  assert.deepEqual(outcome.file, { id: "a", name: "假期 照片.jpg" });
  assert.equal(controller.getState().target, null);
});

test("前端校验失败不调用 IPC 并保留对话框", () => {
  let called = false;
  const controller = createRenameController({
    renameById: async () => {
      called = true;
    },
  });
  controller.open(fileA);
  controller.setDraft("   ");

  return controller.confirm().then((outcome) => {
    assert.equal(called, false);
    assert.equal(outcome.status, "invalid");
    assert.equal(outcome.error.code, "FILE_NAME_EMPTY");
    assert.equal(controller.getState().target.id, "a");
    assert.deepEqual(controller.getState().error, {
      code: "FILE_NAME_EMPTY",
      message: "文件名不能为空。",
    });
  });
});

test("后端失败保留草稿并返回 failed，可再次提交", async () => {
  let attempts = 0;
  const controller = createRenameController({
    renameById: async () => {
      attempts += 1;
      if (attempts === 1) {
        const error = new Error("已存在同名文件，请换一个名称。");
        error.code = "FILE_NAME_EXISTS";
        throw error;
      }
      return { id: "a", name: "another.jpg" };
    },
  });
  controller.open(fileA);
  controller.setDraft("another.jpg");

  const failed = await controller.confirm();
  assert.equal(failed.status, "failed");
  assert.equal(failed.error.code, "FILE_NAME_EXISTS");
  assert.equal(controller.getState().draft, "another.jpg");

  const retried = await controller.confirm();
  assert.equal(retried.status, "committed");
  assert.equal(retried.file.name, "another.jpg");
});

test("fileListStore.replace 立即更新名称并保持排序", async () => {
  const store = createFileListStore({
    fetchFiles: async () => [
      { ...fileA, name: "old.jpg" },
      { ...fileB },
    ],
  });
  await store.load();
  assert.deepEqual(
    store.getState().items.map((item) => item.name),
    ["notes", "old.jpg"]
  );

  assert.equal(store.replace({ id: "a", name: "new.jpg" }), true);
  assert.deepEqual(
    store.getState().items.map((item) => item.name),
    ["notes", "new.jpg"]
  );
  assert.equal(store.replace({ id: "missing", name: "x.jpg" }), false);
});
