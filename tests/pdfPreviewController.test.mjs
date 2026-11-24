import test from "node:test";
import assert from "node:assert/strict";
import {
  PDF_PREVIEW_MAX_BYTES,
  createPdfPreviewController,
  hasPdfHeader,
  isPdfName,
} from "../src/lib/pdfPreviewController.js";

function pdfBytes(tag = "hello") {
  const head = new TextEncoder().encode("%PDF-1.7\n");
  const tail = new TextEncoder().encode(`\n${tag}\n%%EOF`);
  const bytes = new Uint8Array(head.length + tail.length);
  bytes.set(head, 0);
  bytes.set(tail, head.length);
  return bytes;
}

function setup({ loadImpl } = {}) {
  const states = [];
  let loads = 0;
  const controller = createPdfPreviewController({
    loadBytes: (id) => {
      loads += 1;
      if (loadImpl) return loadImpl(id);
      return Promise.resolve(pdfBytes(id));
    },
    notify: (state) => states.push(state),
  });
  return { controller, states, loadCount: () => loads };
}

const tick = () => new Promise((resolve) => setTimeout(resolve, 0));

test("isPdfName 只识别 .pdf 扩展名", () => {
  assert.equal(isPdfName("report.pdf"), true);
  assert.equal(isPdfName("REPORT.PDF"), true);
  assert.equal(isPdfName("report.pdf.exe"), false);
  assert.equal(isPdfName("report.png"), false);
  assert.equal(isPdfName(""), false);
});

test("hasPdfHeader 允许文件头出现在前 1024 字节内", () => {
  assert.equal(hasPdfHeader(pdfBytes()), true);
  assert.equal(hasPdfHeader(new Uint8Array([0x25, 0x50, 0x44, 0x46])), false);

  const shifted = new Uint8Array(1024 + 8);
  shifted.set(pdfBytes(), 1000);
  assert.equal(hasPdfHeader(shifted), true);

  const late = new Uint8Array(2048);
  late.set(pdfBytes(), 1100);
  assert.equal(hasPdfHeader(late), false);
});

test("非 PDF 文件不请求解密", async () => {
  const { controller, states, loadCount } = setup();
  await controller.open({ id: "1", name: "notes.txt", size: 10 });

  assert.equal(loadCount(), 0);
  assert.equal(states.at(-1).status, "error");
  assert.equal(states.at(-1).code, "PREVIEW_UNSUPPORTED");
});

test("超过预览上限时不请求解密", async () => {
  const { controller, states, loadCount } = setup();
  await controller.open({
    id: "1",
    name: "huge.pdf",
    size: PDF_PREVIEW_MAX_BYTES + 1,
  });

  assert.equal(loadCount(), 0);
  assert.equal(states.at(-1).status, "error");
  assert.equal(states.at(-1).code, "PREVIEW_TOO_LARGE");
});

test("正常 PDF 预览只持有内存字节", async () => {
  const { controller, states } = setup();
  await controller.open({ id: "1", name: "report.pdf", size: 100 });

  const final = states.at(-1);
  assert.equal(final.status, "ready");
  assert.equal(final.id, "1");
  assert.ok(final.bytes instanceof Uint8Array);
  assert.equal(hasPdfHeader(final.bytes), true);
});

test("空内容与缺少 PDF 文件头都按无效内容拒绝", async () => {
  const empty = setup({ loadImpl: async () => new Uint8Array(0) });
  await empty.controller.open({ id: "1", name: "empty.pdf", size: 0 });
  assert.equal(empty.states.at(-1).status, "error");
  assert.equal(empty.states.at(-1).code, "PREVIEW_INVALID");

  const junk = setup({ loadImpl: async () => new Uint8Array([1, 2, 3, 4]) });
  await junk.controller.open({ id: "2", name: "junk.pdf", size: 4 });
  assert.equal(junk.states.at(-1).status, "error");
  assert.equal(junk.states.at(-1).code, "PREVIEW_INVALID");
});

test("后端错误保留错误码和提示", async () => {
  const { controller, states } = setup({
    loadImpl: async () => {
      throw { code: "INTEGRITY_ERROR", message: "文件完整性校验失败。" };
    },
  });
  await controller.open({ id: "1", name: "report.pdf", size: 100 });

  assert.equal(states.at(-1).status, "error");
  assert.equal(states.at(-1).code, "INTEGRITY_ERROR");
  assert.equal(states.at(-1).message, "文件完整性校验失败。");
  assert.equal(states.at(-1).bytes, null);
});

test("连续切换 PDF 时旧请求不能覆盖新请求", async () => {
  const pending = new Map();
  const { controller, states } = setup({
    loadImpl: (id) =>
      new Promise((resolve, reject) => pending.set(id, { resolve, reject })),
  });

  const first = controller.open({ id: "1", name: "one.pdf", size: 10 });
  const second = controller.open({ id: "2", name: "two.pdf", size: 10 });

  pending.get("2").resolve(pdfBytes("two"));
  await tick();
  pending.get("1").resolve(pdfBytes("one"));
  await first;
  await second;

  assert.equal(states.at(-1).id, "2");
  assert.equal(states.at(-1).status, "ready");
});

test("加载中关闭预览，迟到的结果被忽略且不保留字节", async () => {
  let resolveLoad;
  const { controller, states } = setup({
    loadImpl: () => new Promise((resolve) => (resolveLoad = resolve)),
  });

  const pending = controller.open({ id: "1", name: "report.pdf", size: 10 });
  controller.close();
  resolveLoad(pdfBytes());
  await pending;

  assert.equal(states.at(-1).status, "idle");
  assert.equal(states.at(-1).bytes, null);
});

test("已展示 PDF 后关闭与 dispose 都会清空字节", async () => {
  const first = setup();
  await first.controller.open({ id: "1", name: "report.pdf", size: 100 });
  first.controller.close();
  assert.equal(first.states.at(-1).bytes, null);
  assert.equal(first.controller.getState().status, "idle");

  const second = setup();
  await second.controller.open({ id: "1", name: "report.pdf", size: 100 });
  second.controller.dispose();
  assert.equal(second.controller.getState().bytes, null);
  assert.equal(second.controller.getState().status, "idle");
});
