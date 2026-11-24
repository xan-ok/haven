import test from "node:test";
import assert from "node:assert/strict";

import {
  createSelectionController,
  pruneSelection,
} from "../src/lib/selectionController.js";
import { createBatchDeleteController } from "../src/lib/batchDeleteController.js";
import { createFileBrowser } from "../src/lib/fileBrowser.js";
import { createFileListStore } from "../src/lib/fileListStore.js";

const FILES = [
  { id: "a", name: "alpha.jpg", size: 10, importedAt: 300 },
  { id: "b", name: "beta.pdf", size: 20, importedAt: 200 },
  { id: "c", name: "gamma.txt", size: 30, importedAt: 100 },
];

test("选择与取消选择单个文件", () => {
  const snapshots = [];
  const selection = createSelectionController({ notify: (next) => snapshots.push(next.count) });

  assert.equal(selection.toggle("a", true), true);
  assert.equal(selection.toggle("b", true), true);
  assert.equal(selection.has("a"), true);
  assert.equal(selection.getState().count, 2);

  assert.equal(selection.toggle("a", false), true);
  assert.equal(selection.has("a"), false);
  assert.deepEqual(selection.getState().ids, ["b"]);
  assert.deepEqual(snapshots, [1, 2, 1]);
});

test("全选当前列表与取消全选", () => {
  const selection = createSelectionController();
  assert.equal(selection.selectAll(FILES.map((file) => file.id)), true);
  assert.equal(selection.getState().count, 3);
  assert.equal(selection.selectAll(["a", "b", "c"]), false, "重复全选不重复通知");

  assert.equal(selection.clear(), true);
  assert.equal(selection.getState().count, 0);
  assert.equal(selection.clear(), false);
});

test("搜索或筛选变化后选择自动清理隐藏文件", () => {
  const selection = createSelectionController();
  let latest = null;
  const browser = createFileBrowser({
    onChange: (next) => {
      latest = next;
      selection.reconcile(next.visible.map((file) => file.id));
    },
  });

  browser.setFiles(FILES);
  assert.deepEqual(latest.visible.map((file) => file.id), ["a", "b", "c"]);
  selection.selectAll(latest.visible.map((file) => file.id));
  assert.equal(selection.getState().count, 3);

  browser.setQuery({ keyword: "alpha" });
  assert.deepEqual(selection.getState().ids, ["a"]);

  browser.setQuery({ keyword: "" });
  browser.setQuery({ type: "pdf" });
  assert.deepEqual(selection.getState().ids, []);

  browser.clearFilters();
  selection.toggle("c", true);
  browser.setQuery({ sortKey: "size", sortDirection: "asc" });
  assert.deepEqual(selection.getState().ids, ["c"], "仅排序不清理仍可见的选择");
});

test("pruneSelection 只保留可见 ID 并去重", () => {
  assert.deepEqual(pruneSelection(["a", "b", "a", "x"], ["b", "a"]), ["a", "b"]);
  assert.deepEqual(pruneSelection(["a"], []), []);
});

test("取消批量删除确认不产生任何删除", async () => {
  const calls = [];
  const controller = createBatchDeleteController({
    deleteById: async (id) => calls.push(id),
  });

  assert.equal(controller.request(["a", "b"]), true);
  assert.equal(controller.getState().total, 2);
  assert.equal(controller.cancel(), true);
  assert.equal(await controller.confirm(), null);
  assert.deepEqual(calls, []);
  assert.equal(controller.getState().open, false);
});

test("批量删除按顺序逐项执行并返回成功项", async () => {
  const calls = [];
  const controller = createBatchDeleteController({
    deleteById: async (id) => {
      calls.push(id);
      return { cleanupPending: id === "b" };
    },
  });
  controller.request(["a", "b", "c"]);

  const outcome = await controller.confirm();

  assert.deepEqual(calls, ["a", "b", "c"]);
  assert.equal(outcome.status, "committed");
  assert.deepEqual(outcome.succeeded, ["a", "b", "c"]);
  assert.deepEqual(outcome.failures, []);
  assert.equal(outcome.cleanupPending, true);
});

test("部分失败时准确返回成功与失败数量", async () => {
  const controller = createBatchDeleteController({
    deleteById: async (id) => {
      if (id === "b") {
        const error = new Error("删除未完成，文件仍保留在 Haven 中。");
        error.code = "DELETE_INCOMPLETE";
        throw error;
      }
      return { cleanupPending: false };
    },
  });
  controller.request(["a", "b", "c"]);

  const outcome = await controller.confirm();

  assert.equal(outcome.status, "partial");
  assert.deepEqual(outcome.succeeded, ["a", "c"]);
  assert.equal(outcome.failures.length, 1);
  assert.equal(outcome.failures[0].id, "b");
  assert.equal(outcome.failures[0].error.code, "DELETE_INCOMPLETE");
});

test("锁定或恢复状态不明时立即停止并保留未处理项", async () => {
  const calls = [];
  const controller = createBatchDeleteController({
    deleteById: async (id) => {
      calls.push(id);
      if (id === "b") {
        const error = new Error("保险库已锁定。");
        error.code = "NOT_UNLOCKED";
        throw error;
      }
      return { cleanupPending: false };
    },
  });
  controller.request(["a", "b", "c"]);

  const outcome = await controller.confirm();

  assert.deepEqual(calls, ["a", "b"], "触发中止后不再继续删除");
  assert.equal(outcome.status, "interrupted");
  assert.deepEqual(outcome.succeeded, ["a"]);
  assert.equal(outcome.fatal.code, "NOT_UNLOCKED");
  assert.equal(outcome.unprocessed, 1);
});

test("批量删除后本地列表与选择计数立即一致", async () => {
  const store = createFileListStore({ fetchFiles: async () => FILES });
  const selection = createSelectionController();
  await store.load();
  selection.selectAll(store.getState().items.map((file) => file.id));

  const controller = createBatchDeleteController({
    deleteById: async () => ({ cleanupPending: false }),
  });
  controller.request(selection.getState().ids);
  const outcome = await controller.confirm();
  for (const id of outcome.succeeded) store.remove(id);
  selection.clear();

  assert.equal(store.getState().items.length, 0);
  assert.equal(selection.getState().count, 0);
});
