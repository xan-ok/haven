import test from "node:test";
import assert from "node:assert/strict";
import { createThumbnailController } from "../src/lib/thumbnailController.js";

const tick = () => new Promise((resolve) => setTimeout(resolve, 0));

function deferred() {
  let resolve;
  let reject;
  const promise = new Promise((done, fail) => {
    resolve = done;
    reject = fail;
  });
  return { promise, resolve, reject };
}

function setup({ loads = {}, concurrency = 2 } = {}) {
  const states = [];
  const created = [];
  const revoked = [];
  const calls = [];
  const controller = createThumbnailController({
    loadBytes: (id) => {
      calls.push(id);
      return loads[id] ?? Promise.resolve(new Uint8Array([1, 2, 3]));
    },
    createObjectUrl: (blob) => {
      const url = `blob:${created.length}:${blob.type}`;
      created.push(blob);
      return url;
    },
    revokeObjectUrl: (url) => revoked.push(url),
    isImage: (name) => /\.(png|jpe?g)$/i.test(name || ""),
    notify: (state) => states.push(state),
    concurrency,
  });
  return { controller, states, created, revoked, calls };
}

test("非图片文件不请求缩略图", () => {
  const { controller, states, calls } = setup();

  controller.request({ id: "1", name: "notes.txt" });
  controller.request({ id: "2", name: "document.pdf" });

  assert.equal(calls.length, 0);
  assert.equal(states.length, 0);
});

test("图片加载成功后缓存 Blob URL，重复请求不重复解密", async () => {
  const { controller, states, created, calls } = setup();

  controller.request({ id: "1", name: "photo.png" });
  assert.deepEqual(controller.getState()["1"], { status: "loading", url: null });
  controller.request({ id: "1", name: "photo.png" });
  await tick();

  assert.deepEqual(calls, ["1"]);
  assert.equal(controller.getState()["1"].status, "ready");
  assert.match(controller.getState()["1"].url, /^blob:/);
  assert.equal(created[0].type, "image/png");
  assert.deepEqual(states.at(-1)["1"], controller.getState()["1"]);
});

test("加载失败时降级为 failed，不产生 Blob URL", async () => {
  const failure = deferred();
  failure.reject(new Error("decrypt failed"));
  const { controller, created } = setup({ loads: { "1": failure.promise } });

  controller.request({ id: "1", name: "broken.jpg" });
  await tick();

  assert.deepEqual(controller.getState()["1"], { status: "failed", url: null });
  assert.equal(created.length, 0);
  // 失败结果会被保留，避免每个卡片反复请求。
  controller.request({ id: "1", name: "broken.jpg" });
  await tick();
  assert.equal(controller.getState()["1"].status, "failed");
});

test("并发生效：同时最多加载指定数量", async () => {
  const gates = { "1": deferred(), "2": deferred(), "3": deferred() };
  const { controller, calls } = setup({
    loads: { "1": gates["1"].promise, "2": gates["2"].promise, "3": gates["3"].promise },
    concurrency: 2,
  });

  controller.request({ id: "1", name: "a.png" });
  controller.request({ id: "2", name: "b.png" });
  controller.request({ id: "3", name: "c.png" });
  await tick();

  assert.deepEqual(calls, ["1", "2"]);
  gates["1"].resolve(new Uint8Array([1]));
  await tick();
  assert.deepEqual(calls, ["1", "2", "3"]);
  gates["2"].resolve(new Uint8Array([2]));
  gates["3"].resolve(new Uint8Array([3]));
  await tick();
});

test("release 撤销单个 URL 并丢弃晚到结果", async () => {
  const gate = deferred();
  const { controller, revoked } = setup({ loads: { "1": gate.promise } });

  controller.request({ id: "1", name: "photo.png" });
  controller.release("1");
  gate.resolve(new Uint8Array([1]));
  await tick();

  assert.deepEqual(revoked, []);
  assert.equal(controller.getState()["1"], undefined);

  controller.request({ id: "2", name: "ready.png" });
  await tick();
  assert.equal(controller.getState()["2"].status, "ready");
  controller.release("2");
  assert.deepEqual(revoked, ["blob:0:image/png"]);
  assert.equal(controller.getState()["2"], undefined);
});

test("releaseAll 撤销全部 URL、清空队列并阻止晚到结果回填", async () => {
  const ready = Promise.resolve(new Uint8Array([1]));
  const late = deferred();
  const { controller, revoked, calls } = setup({
    loads: { "1": ready, "2": late.promise },
    concurrency: 1,
  });

  controller.request({ id: "1", name: "a.png" });
  controller.request({ id: "2", name: "b.png" });
  controller.request({ id: "3", name: "c.png" });
  await tick();
  assert.equal(controller.getState()["1"].status, "ready");
  assert.deepEqual(calls, ["1", "2"], "第三个任务仍在队列中");

  controller.releaseAll();
  assert.deepEqual(revoked, ["blob:0:image/png"]);
  assert.deepEqual(controller.getState(), {});

  late.resolve(new Uint8Array([2]));
  await tick();

  assert.equal(controller.getState()["2"], undefined);
  assert.equal(controller.getState()["3"], undefined);
  assert.deepEqual(calls, ["1", "2"], "清空队列后不应发起第三个请求");
});

test("dispose 后不再发起加载或通知", async () => {
  const { controller, states, calls } = setup();

  controller.dispose();
  const before = states.length;
  controller.request({ id: "1", name: "photo.png" });
  await tick();

  assert.equal(calls.length, 0);
  assert.equal(states.length, before);
});
