import test from "node:test";
import assert from "node:assert/strict";
import { createFileListStore, sortFilesDesc } from "../src/lib/fileListStore.js";

const fileA = { id: "a", name: "a.png", size: 10, importedAt: 100 };
const fileB = { id: "b", name: "b.pdf", size: 20, importedAt: 300 };
const fileC = { id: "c", name: "c.jpg", size: 30, importedAt: 200 };

function collect() {
  const states = [];
  return { states, notify: (state) => states.push(state) };
}

test("sortFilesDesc 按导入时间倒序排列", () => {
  assert.deepEqual(
    sortFilesDesc([fileA, fileB, fileC]).map((file) => file.id),
    ["b", "c", "a"]
  );
});

test("列表加载成功后发布排序结果", async () => {
  const { states, notify } = collect();
  const store = createFileListStore({
    fetchFiles: async () => [fileA, fileB, fileC],
    notify,
  });

  await store.load();

  assert.equal(states[0].loading, true);
  const final = states.at(-1);
  assert.equal(final.loading, false);
  assert.equal(final.error, null);
  assert.deepEqual(
    final.items.map((file) => file.id),
    ["b", "c", "a"]
  );
});

test("空列表返回空数组且无错误", async () => {
  const { states, notify } = collect();
  const store = createFileListStore({ fetchFiles: async () => [], notify });

  await store.load();

  const final = states.at(-1);
  assert.deepEqual(final.items, []);
  assert.equal(final.error, null);
});

test("列表加载失败发布错误状态", async () => {
  const { states, notify } = collect();
  const store = createFileListStore({
    fetchFiles: async () => {
      throw { code: "VAULT_UNAVAILABLE", message: "外置磁盘不可用。" };
    },
    notify,
  });

  await store.load();

  const final = states.at(-1);
  assert.equal(final.loading, false);
  assert.equal(final.error.code, "VAULT_UNAVAILABLE");
  assert.equal(final.error.message, "外置磁盘不可用。");
  assert.deepEqual(final.items, [], "保险库不可用时不得保留旧列表");
});

test("保险库不可用时刷新会清空已展示的文件项", async () => {
  const { states, notify } = collect();
  let call = 0;
  const store = createFileListStore({
    fetchFiles: async () => {
      call += 1;
      if (call === 1) return [fileA, fileB];
      throw { code: "VAULT_UNAVAILABLE", message: "保险库所在磁盘不可用，请重新连接后解锁。" };
    },
    notify,
  });

  await store.load();
  assert.equal(states.at(-1).items.length, 2);

  await store.load();
  const final = states.at(-1);
  assert.equal(final.error.code, "VAULT_UNAVAILABLE");
  assert.deepEqual(final.items, [], "磁盘不可用后旧文件名必须立即消失");
});

test("clear 立即清空列表并作废旧加载结果", async () => {
  const { states, notify } = collect();
  let resolveRefresh;
  let calls = 0;
  const store = createFileListStore({
    fetchFiles: () => {
      calls += 1;
      if (calls === 1) return Promise.resolve([fileA, fileB]);
      return new Promise((resolve) => {
        resolveRefresh = resolve;
      });
    },
    notify,
  });
  await store.load();

  const pending = store.load();
  store.clear();
  assert.deepEqual(store.getState().items, []);

  resolveRefresh([fileA]);
  await pending;
  assert.deepEqual(store.getState().items, [], "clear 之后旧请求不得回填列表");
  assert.equal(states.at(-1).items.length, 0);
});

test("导入后再次加载会刷新列表", async () => {
  const { states, notify } = collect();
  let call = 0;
  const store = createFileListStore({
    fetchFiles: async () => {
      call += 1;
      return call === 1 ? [] : [fileB];
    },
    notify,
  });

  await store.load();
  assert.equal(states.at(-1).items.length, 0);

  await store.load();
  assert.equal(states.at(-1).items.length, 1);
  assert.equal(states.at(-1).items[0].id, "b");
});

test("旧请求不能覆盖新请求的列表结果", async () => {
  const { states, notify } = collect();
  let resolveFirst;
  let calls = 0;
  const store = createFileListStore({
    fetchFiles: () =>
      new Promise((resolve) => {
        calls += 1;
        if (calls === 1) resolveFirst = resolve;
        else resolve([fileB]);
      }),
    notify,
  });

  const first = store.load();
  await store.load();
  resolveFirst([fileA]);
  await first;

  const final = states.at(-1);
  assert.deepEqual(
    final.items.map((file) => file.id),
    ["b"]
  );
});

test("dispose 后不再发布列表状态", async () => {
  const { states, notify } = collect();
  const store = createFileListStore({ fetchFiles: async () => [fileA], notify });

  const pending = store.load();
  store.dispose();
  await pending;

  assert.equal(states.at(-1).loading, true);
  assert.deepEqual(states.at(-1).items, []);
});

test("remove 只移除指定 ID，旧加载结果不能复活已删除项", async () => {
  const initialItems = [fileA, fileB, fileC];
  const { states, notify } = collect();
  let resolveRefresh;
  let calls = 0;
  const store = createFileListStore({
    fetchFiles: () => {
      calls += 1;
      if (calls === 1) return Promise.resolve(initialItems);
      return new Promise((resolve) => {
        resolveRefresh = resolve;
      });
    },
    notify,
  });
  await store.load();

  const pending = store.load();
  assert.equal(store.remove("c"), true);
  assert.deepEqual(
    store.getState().items.map((file) => file.id),
    ["b", "a"]
  );
  assert.equal(store.getState().loading, false);
  const publishedAfterRemove = states.length;

  resolveRefresh(initialItems);
  await pending;

  assert.deepEqual(
    store.getState().items.map((file) => file.id),
    ["b", "a"]
  );
  assert.equal(store.getState().error, null);
  assert.equal(states.length, publishedAfterRemove, "失效请求不应再发布状态");
});
