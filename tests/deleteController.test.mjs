import test from "node:test";
import assert from "node:assert/strict";

import { createDeleteController } from "../src/lib/deleteController.js";
import { createFileListStore } from "../src/lib/fileListStore.js";

const fileA = { id: "a", name: "a.png", size: 10, importedAt: 100 };
const fileB = { id: "b", name: "b.pdf", size: 20, importedAt: 200 };

function deferred() {
  let resolve;
  let reject;
  const promise = new Promise((nextResolve, nextReject) => {
    resolve = nextResolve;
    reject = nextReject;
  });
  return { promise, resolve, reject };
}

const ids = (files) => files.map((file) => file.id);

test("取消删除不调用 IPC，取消后确认也是空操作", async () => {
  const calls = [];
  const states = [];
  const controller = createDeleteController({
    deleteById: async (id) => calls.push(id),
    notify: (state) => states.push(state),
  });

  assert.equal(controller.open(fileA), true);
  assert.equal(controller.cancel(), true);
  assert.equal(await controller.confirm(), null);

  assert.deepEqual(calls, []);
  assert.deepEqual(controller.getState(), { target: null, busy: false });
  assert.equal(states.length, 2);
});

test("确认成功返回提交目标并传递 cleanupPending", async () => {
  const calls = [];
  const controller = createDeleteController({
    deleteById: async (id) => {
      calls.push(id);
      return { cleanupPending: true };
    },
  });
  controller.open(fileA);

  const outcome = await controller.confirm();

  assert.deepEqual(calls, ["a"]);
  assert.deepEqual(outcome, {
    status: "committed",
    file: { id: "a", name: "a.png" },
    result: { cleanupPending: true },
  });
  assert.deepEqual(controller.getState(), { target: null, busy: false });
});

test("重复确认只调用一次，忙碌时取消与打开其他目标均无效", async () => {
  const request = deferred();
  const calls = [];
  const controller = createDeleteController({
    deleteById: (id) => {
      calls.push(id);
      return request.promise;
    },
  });
  controller.open(fileA);

  const pending = controller.confirm();
  assert.equal(await controller.confirm(), null);
  assert.equal(controller.cancel(), false);
  assert.equal(controller.open(fileB), false);
  assert.deepEqual(calls, ["a"]);
  assert.deepEqual(controller.getState(), {
    target: { id: "a", name: "a.png" },
    busy: true,
  });

  request.resolve({});
  const outcome = await pending;
  assert.equal(outcome.status, "committed");
  assert.equal(outcome.result.cleanupPending, false);
  assert.deepEqual(controller.getState(), { target: null, busy: false });
});

test("删除失败保留结果目标与列表数据，重新打开后重试只移除指定 ID", async () => {
  const failure = { code: "DELETE_FAILED", message: "删除失败。" };
  const calls = [];
  let shouldFail = true;
  const controller = createDeleteController({
    deleteById: async (id) => {
      calls.push(id);
      if (shouldFail) {
        shouldFail = false;
        throw failure;
      }
      return { cleanupPending: false };
    },
  });
  const store = createFileListStore({ fetchFiles: async () => [fileA, fileB] });
  await store.load();
  controller.open(fileA);

  const failed = await controller.confirm();

  assert.equal(failed.status, "failed");
  assert.equal(failed.error, failure);
  assert.deepEqual(failed.file, { id: "a", name: "a.png" });
  assert.deepEqual(ids(store.getState().items), ["b", "a"]);
  assert.equal(controller.getState().busy, false);

  assert.equal(controller.open(fileA), true);
  const committed = await controller.confirm();
  assert.equal(committed.status, "committed");
  assert.equal(store.remove(committed.file.id), true);

  assert.deepEqual(calls, ["a", "a"]);
  assert.deepEqual(ids(store.getState().items), ["b"]);
});

test("dispose 后迟到的删除结果不发布状态", async () => {
  const request = deferred();
  const states = [];
  const controller = createDeleteController({
    deleteById: () => request.promise,
    notify: (state) => states.push(state),
  });
  controller.open(fileA);
  const pending = controller.confirm();
  const publishedBeforeDispose = states.length;

  controller.dispose();
  request.resolve({ cleanupPending: true });

  assert.equal(await pending, null);
  assert.equal(states.length, publishedBeforeDispose);
  assert.deepEqual(controller.getState(), { target: null, busy: false });
  assert.equal(controller.open(fileB), false);
});
