import test from "node:test";
import assert from "node:assert/strict";
import {
  PREVIEW_MAX_BYTES,
  createPreviewController,
  previewTypeFor,
} from "../src/lib/previewController.js";

const PNG = new Uint8Array([0x89, 0x50, 0x4e, 0x47, 0x0d, 0x0a, 0x1a, 0x0a, 1, 2]);
const JPEG = new Uint8Array([0xff, 0xd8, 0xff, 1, 2]);

function setup({ loadImpl } = {}) {
  const created = [];
  const revoked = [];
  const states = [];
  let loads = 0;
  const controller = createPreviewController({
    loadBytes: (id) => {
      loads += 1;
      if (loadImpl) return loadImpl(id);
      return Promise.resolve(id.endsWith(".png") ? PNG : JPEG);
    },
    createObjectUrl: (blob) => {
      const url = `blob:mock-${created.length + 1}`;
      created.push({ url, blob });
      return url;
    },
    revokeObjectUrl: (url) => revoked.push(url),
    notify: (state) => states.push(state),
  });
  return { controller, created, revoked, states, loadCount: () => loads };
}

const tick = () => new Promise((resolve) => setTimeout(resolve, 0));

test("previewTypeFor 只识别 JPG/JPEG/PNG", () => {
  assert.deepEqual(previewTypeFor("a.png"), { mime: "image/png" });
  assert.deepEqual(previewTypeFor("a.JPG"), { mime: "image/jpeg" });
  assert.deepEqual(previewTypeFor("a.jpeg"), { mime: "image/jpeg" });
  assert.equal(previewTypeFor("a.pdf"), null);
  assert.equal(previewTypeFor("a.png.exe"), null);
});

test("非图片文件不请求解密", async () => {
  const { controller, states, loadCount } = setup();
  await controller.open({ id: "1", name: "notes.pdf", size: 10 });

  assert.equal(loadCount(), 0);
  assert.equal(states.at(-1).status, "error");
  assert.equal(states.at(-1).code, "PREVIEW_UNSUPPORTED");
});

test("超过预览上限时不请求解密", async () => {
  const { controller, states, loadCount } = setup();
  await controller.open({
    id: "1",
    name: "huge.png",
    size: PREVIEW_MAX_BYTES + 1,
  });

  assert.equal(loadCount(), 0);
  assert.equal(states.at(-1).status, "error");
  assert.equal(states.at(-1).code, "PREVIEW_TOO_LARGE");
});

test("PNG 预览生成 Blob URL", async () => {
  const { controller, created, states } = setup();
  await controller.open({ id: "1", name: "photo.png", size: 100 });

  const final = states.at(-1);
  assert.equal(final.status, "ready");
  assert.equal(final.url, "blob:mock-1");
  assert.equal(created[0].blob.type, "image/png");
});

test("JPEG 预览生成 Blob URL", async () => {
  const { controller, created, states } = setup();
  await controller.open({ id: "2", name: "photo.JPEG", size: 100 });

  assert.equal(states.at(-1).status, "ready");
  assert.equal(created[0].blob.type, "image/jpeg");
});

test("关闭预览会撤销 Blob URL 并回到空闲", async () => {
  const { controller, revoked, states } = setup();
  await controller.open({ id: "1", name: "photo.png", size: 100 });
  const url = states.at(-1).url;

  controller.close();

  assert.deepEqual(revoked, [url]);
  assert.equal(states.at(-1).status, "idle");
  assert.equal(states.at(-1).url, null);
});

test("解密失败显示错误且不创建 URL", async () => {
  const { controller, created, states } = setup({
    loadImpl: async () => {
      throw { code: "INTEGRITY_ERROR", message: "文件完整性校验失败。" };
    },
  });
  await controller.open({ id: "1", name: "photo.png", size: 100 });

  assert.equal(created.length, 0);
  assert.equal(states.at(-1).status, "error");
  assert.equal(states.at(-1).code, "INTEGRITY_ERROR");
  assert.equal(states.at(-1).message, "文件完整性校验失败。");
});

test("后端像素超限时展示尺寸错误且不创建 URL", async () => {
  const { controller, created, states } = setup({
    loadImpl: async () => {
      throw {
        code: "PREVIEW_TOO_LARGE",
        message: "图片解码尺寸超过预览上限（最大 1600 万像素、单边 8192 px）。",
      };
    },
  });
  await controller.open({ id: "1", name: "wide.png", size: 100 });

  assert.equal(created.length, 0);
  assert.equal(states.at(-1).status, "error");
  assert.equal(states.at(-1).code, "PREVIEW_TOO_LARGE");
  assert.match(states.at(-1).message, /1600 万像素/);
});

test("连续切换图片时旧请求不能覆盖新请求", async () => {
  const pending = new Map();
  const { controller, created, states } = setup({
    loadImpl: (id) =>
      new Promise((resolve, reject) => pending.set(id, { resolve, reject })),
  });

  const first = controller.open({ id: "1", name: "one.png", size: 10 });
  const second = controller.open({ id: "2", name: "two.png", size: 10 });

  pending.get("2").resolve(PNG);
  await tick();
  pending.get("1").resolve(PNG);
  await first;
  await second;

  assert.equal(states.at(-1).id, "2");
  assert.equal(states.at(-1).status, "ready");
  assert.equal(created.length, 1, "旧请求不应创建 Blob URL");
});

test("加载中关闭预览，迟到的结果被忽略", async () => {
  let resolveLoad;
  const { controller, created, states } = setup({
    loadImpl: () => new Promise((resolve) => (resolveLoad = resolve)),
  });

  const pending = controller.open({ id: "1", name: "photo.png", size: 10 });
  controller.close();
  resolveLoad(PNG);
  await pending;

  assert.equal(created.length, 0);
  assert.equal(states.at(-1).status, "idle");
});

test("切换图片时撤销上一张的 Blob URL", async () => {
  const { controller, revoked, states } = setup();
  await controller.open({ id: "1", name: "one.png", size: 10 });
  const firstUrl = states.at(-1).url;

  await controller.open({ id: "2", name: "two.png", size: 10 });

  assert.deepEqual(revoked, [firstUrl]);
  assert.equal(states.at(-1).status, "ready");
  assert.notEqual(states.at(-1).url, firstUrl);
});

test("dispose 会撤销 URL 并忽略进行中的请求", async () => {
  let resolveLoad;
  const { controller, created, revoked, states } = setup({
    loadImpl: () => new Promise((resolve) => (resolveLoad = resolve)),
  });

  const pending = controller.open({ id: "1", name: "photo.png", size: 10 });
  controller.dispose();
  resolveLoad(PNG);
  await pending;

  assert.equal(created.length, 0);
  assert.equal(revoked.length, 0, "尚未创建 URL 时无需撤销");
  assert.equal(states.at(-1).status, "loading");
});

test("已展示图片后 dispose 会撤销 URL", async () => {
  const { controller, revoked, states } = setup();
  await controller.open({ id: "1", name: "photo.png", size: 10 });
  const url = states.at(-1).url;

  controller.dispose();

  assert.deepEqual(revoked, [url]);
});
