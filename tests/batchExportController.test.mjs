import test from "node:test";
import assert from "node:assert/strict";
import {
  buildExportTarget,
  collectFolderExportItems,
  createBatchExportController,
} from "../src/lib/batchExportController.js";

function setup({ exportImpl } = {}) {
  const states = [];
  const calls = [];
  const controller = createBatchExportController({
    exportById: (id, target) => {
      calls.push({ id, target });
      if (exportImpl) return exportImpl(id, target);
      return Promise.resolve({ name: id, size: 1 });
    },
    notify: (state) => states.push(state),
  });
  return { controller, states, calls };
}

const tick = () => new Promise((resolve) => setTimeout(resolve, 0));

function deferred() {
  let resolve;
  const promise = new Promise((done) => {
    resolve = done;
  });
  return { promise, resolve };
}

test("buildExportTarget 处理目录尾部斜杠", () => {
  assert.equal(buildExportTarget("/Users/me/out", "报告.pdf"), "/Users/me/out/报告.pdf");
  assert.equal(buildExportTarget("/Users/me/out/", "a.txt"), "/Users/me/out/a.txt");
  assert.equal(buildExportTarget("/", "a.txt"), "/a.txt");
});

test("文件夹导出只收集该文件夹的文件，不含其他文件夹与根目录", () => {
  const files = [
    { id: "a", name: "a.txt", folderId: "f1" },
    { id: "b", name: "b.pdf", folderId: "f1" },
    { id: "c", name: "c.jpg", folderId: "f2" },
    { id: "d", name: "root.txt", folderId: null },
    { id: "e", name: "legacy.txt" },
    { id: "f", name: "", folderId: "f1" },
    { id: "", name: "empty-id.txt", folderId: "f1" },
  ];
  assert.deepEqual(collectFolderExportItems(files, "f1"), [
    { id: "a", name: "a.txt" },
    { id: "b", name: "b.pdf" },
  ]);
  assert.deepEqual(collectFolderExportItems(files, "missing"), []);
  assert.deepEqual(collectFolderExportItems(files, null), []);
  assert.deepEqual(collectFolderExportItems(undefined, "f1"), []);
});

test("取消目录选择（空目录）不执行任何导出", async () => {
  const { controller, calls, states } = setup();
  const outcome = await controller.start([{ id: "a", name: "a.txt" }], "");
  assert.equal(outcome, null);
  assert.deepEqual(calls, []);
  assert.equal(states.length, 0);
});

test("单个文件导出保持原始名称", async () => {
  const { controller, states, calls } = setup();

  const outcome = await controller.start([{ id: "1", name: "照片.jpg" }], "/tmp/out");

  assert.deepEqual(calls, [{ id: "1", target: "/tmp/out/照片.jpg" }]);
  assert.equal(outcome.status, "committed");
  assert.deepEqual(outcome.succeeded, [{ id: "1", name: "照片.jpg" }]);
  assert.equal(outcome.failures.length, 0);
  assert.deepEqual(states.at(-1), {
    status: "idle",
    total: 0,
    processed: 0,
    succeeded: 0,
    failed: 0,
  });
});

test("多个文件严格串行导出并实时更新进度", async () => {
  let active = 0;
  let maxActive = 0;
  const order = [];
  const { controller, states } = setup({
    exportImpl: async (id) => {
      active += 1;
      maxActive = Math.max(maxActive, active);
      order.push(id);
      await tick();
      active -= 1;
    },
  });

  const items = [
    { id: "1", name: "a.txt" },
    { id: "2", name: "b.txt" },
    { id: "3", name: "c.txt" },
  ];
  const outcome = await controller.start(items, "/tmp/out");

  assert.deepEqual(order, ["1", "2", "3"]);
  assert.equal(maxActive, 1, "同一时间只允许一个导出任务");
  assert.equal(outcome.status, "committed");
  assert.equal(outcome.succeeded.length, 3);
  const running = states.filter((state) => state.status === "running");
  assert.deepEqual(
    running.map((state) => state.processed),
    [0, 1, 2, 3]
  );
});

test("目标已存在时按单项失败继续，不中断其余文件", async () => {
  const { controller, calls } = setup({
    exportImpl: async (id) => {
      if (id === "2") throw { code: "FILE_EXISTS", message: "目标位置已存在同名文件。" };
    },
  });

  const outcome = await controller.start(
    [
      { id: "1", name: "a.txt" },
      { id: "2", name: "b.txt" },
      { id: "3", name: "c.txt" },
    ],
    "/tmp/out"
  );

  assert.equal(calls.length, 3);
  assert.equal(outcome.status, "partial");
  assert.equal(outcome.succeeded.length, 2);
  assert.equal(outcome.failures.length, 1);
  assert.equal(outcome.failures[0].id, "2");
  assert.equal(outcome.failures[0].code, "FILE_EXISTS");
  assert.match(outcome.failures[0].message, /同名文件/);
});

test("取消后停止后续任务，已成功的文件保留", async () => {
  const gate = deferred();
  const { controller } = setup({
    exportImpl: async (id) => {
      if (id === "1") await gate.promise;
    },
  });

  const running = controller.start(
    [
      { id: "1", name: "a.txt" },
      { id: "2", name: "b.txt" },
      { id: "3", name: "c.txt" },
    ],
    "/tmp/out"
  );
  await tick();

  assert.equal(controller.getState().status, "running");
  assert.equal(controller.cancel(), true);
  gate.resolve();
  const outcome = await running;

  assert.equal(outcome.status, "cancelled");
  assert.deepEqual(outcome.succeeded, [{ id: "1", name: "a.txt" }]);
  assert.equal(outcome.failures.length, 0);
  assert.equal(outcome.unprocessed, 2);
});

test("锁定错误立即停止后续任务并标记 interrupted", async () => {
  const { controller, calls } = setup({
    exportImpl: async (id) => {
      if (id === "1") throw { code: "NOT_UNLOCKED", message: "保险库已锁定。" };
    },
  });

  const outcome = await controller.start(
    [
      { id: "1", name: "a.txt" },
      { id: "2", name: "b.txt" },
    ],
    "/tmp/out"
  );

  assert.equal(calls.length, 1);
  assert.equal(outcome.status, "interrupted");
  assert.equal(outcome.fatal.code, "NOT_UNLOCKED");
  assert.equal(outcome.unprocessed, 1);
});

test("恢复状态不明时同样立即停止", async () => {
  const { controller, calls } = setup({
    exportImpl: async () => {
      throw { code: "VAULT_RECOVERY_REQUIRED", message: "需要恢复。" };
    },
  });

  const outcome = await controller.start(
    [
      { id: "1", name: "a.txt" },
      { id: "2", name: "b.txt" },
    ],
    "/tmp/out"
  );

  assert.equal(calls.length, 1);
  assert.equal(outcome.status, "interrupted");
  assert.equal(outcome.fatal.code, "VAULT_RECOVERY_REQUIRED");
});

test("重复 ID 与非法规格被去重过滤", async () => {
  const { controller, calls } = setup();

  const outcome = await controller.start(
    [
      { id: "1", name: "a.txt" },
      { id: "1", name: "a.txt" },
      { id: "", name: "empty.txt" },
      { id: "2", name: "" },
      null,
    ],
    "/tmp/out"
  );

  assert.equal(calls.length, 1);
  assert.equal(outcome.total, 1);
});

test("运行中重复 start 与取消请求返回空/false", async () => {
  const gate = deferred();
  const { controller } = setup({
    exportImpl: () => gate.promise,
  });

  const running = controller.start([{ id: "1", name: "a.txt" }], "/tmp/out");
  await tick();

  assert.equal(await controller.start([{ id: "2", name: "b.txt" }], "/tmp/out"), null);
  gate.resolve();
  await running;

  assert.equal(controller.cancel(), false, "空闲时取消无效");
});
