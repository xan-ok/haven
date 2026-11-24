import test from "node:test";
import assert from "node:assert/strict";
import {
  VIDEO_PREVIEW_MAX_BYTES,
  createVideoPreviewController,
  isVideoName,
  videoTypeFor,
} from "../src/lib/videoPreviewController.js";

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

function setup({ loadBytes } = {}) {
  const states = [];
  const created = [];
  const revoked = [];
  const calls = [];
  const controller = createVideoPreviewController({
    loadBytes: (id) => {
      calls.push(id);
      if (loadBytes) return loadBytes(id);
      return Promise.resolve(new Uint8Array([1, 2, 3]));
    },
    createObjectUrl: (blob) => {
      const url = `blob:${created.length}:${blob.type}`;
      created.push(blob);
      return url;
    },
    revokeObjectUrl: (url) => revoked.push(url),
    notify: (state) => states.push(state),
  });
  return { controller, states, created, revoked, calls };
}

test("识别常见视频扩展名并映射 MIME", () => {
  assert.deepEqual(videoTypeFor("a.MP4"), { mime: "video/mp4" });
  assert.deepEqual(videoTypeFor("b.mov"), { mime: "video/quicktime" });
  assert.deepEqual(videoTypeFor("c.M4V"), { mime: "video/x-m4v" });
  assert.deepEqual(videoTypeFor("d.webm"), { mime: "video/webm" });
  assert.equal(videoTypeFor("e.txt"), null);
  assert.equal(isVideoName("a.mp4"), true);
  assert.equal(isVideoName("a.pdf"), false);
});

test("非视频文件不请求解密", async () => {
  const { controller, calls } = setup();

  await controller.open({ id: "1", name: "notes.txt", size: 10 });

  assert.equal(calls.length, 0);
  assert.equal(controller.getState().status, "error");
  assert.equal(controller.getState().code, "PREVIEW_UNSUPPORTED");
});

test("超出大小上限时提前拒绝且不解密", async () => {
  const { controller, calls } = setup();

  await controller.open({
    id: "1",
    name: "huge.mp4",
    size: VIDEO_PREVIEW_MAX_BYTES + 1,
  });

  assert.equal(calls.length, 0);
  assert.equal(controller.getState().status, "error");
  assert.equal(controller.getState().code, "PREVIEW_TOO_LARGE");
  assert.match(controller.getState().message, /64 MB/);
});

test("正常打开创建正确 MIME 的 Blob URL", async () => {
  const { controller, created, states } = setup();

  await controller.open({ id: "1", name: "clip.mov", size: 1024 });

  assert.equal(controller.getState().status, "ready");
  assert.equal(controller.getState().url, "blob:0:video/quicktime");
  assert.equal(created[0].type, "video/quicktime");
  assert.equal(states[0].status, "loading");
});

test("关闭预览撤销 Blob URL 并回到 idle", async () => {
  const { controller, revoked } = setup();

  await controller.open({ id: "1", name: "clip.mp4", size: 1024 });
  controller.close();

  assert.deepEqual(revoked, ["blob:0:video/mp4"]);
  assert.equal(controller.getState().status, "idle");
  assert.equal(controller.getState().url, null);
});

test("切换视频立即释放上一个 URL", async () => {
  const { controller, revoked } = setup();

  await controller.open({ id: "1", name: "a.mp4", size: 1 });
  await controller.open({ id: "2", name: "b.webm", size: 1 });

  assert.deepEqual(revoked, ["blob:0:video/mp4"]);
  assert.equal(controller.getState().id, "2");
  assert.equal(controller.getState().url, "blob:1:video/webm");
});

test("关闭后到达的旧结果不会回填", async () => {
  const gate = deferred();
  const { controller, created } = setup({ loadBytes: () => gate.promise });

  const opening = controller.open({ id: "1", name: "a.mp4", size: 1 });
  controller.close();
  gate.resolve(new Uint8Array([9]));
  await opening;

  assert.equal(created.length, 0);
  assert.equal(controller.getState().status, "idle");
});

test("后端错误码与消息透传，便于提示编码或完整性失败", async () => {
  const { controller } = setup({
    loadBytes: () => Promise.reject({ code: "PREVIEW_INVALID", message: "文件内容不是有效的视频。" }),
  });

  await controller.open({ id: "1", name: "broken.mp4", size: 10 });

  assert.equal(controller.getState().status, "error");
  assert.equal(controller.getState().code, "PREVIEW_INVALID");
  assert.match(controller.getState().message, /不是有效的视频/);
});

test("锁定错误透传 NOT_UNLOCKED 供上层进入锁定流程", async () => {
  const { controller } = setup({
    loadBytes: () => Promise.reject({ code: "NOT_UNLOCKED", message: "保险库已锁定。" }),
  });

  await controller.open({ id: "1", name: "a.mp4", size: 10 });

  assert.equal(controller.getState().code, "NOT_UNLOCKED");
});

test("dispose 撤销 URL 并丢弃后续结果", async () => {
  const gate = deferred();
  const { controller, revoked } = setup({ loadBytes: () => gate.promise });

  const opening = controller.open({ id: "1", name: "a.mp4", size: 1 });
  controller.dispose();
  gate.resolve(new Uint8Array([1]));
  await opening;

  assert.deepEqual(revoked, []);
  assert.equal(controller.getState().status, "idle");
});
