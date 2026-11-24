import test from "node:test";
import assert from "node:assert/strict";

import { createFileDetails } from "../src/lib/fileDetails.js";

const file = {
  id: "f1",
  name: "季度报告-01.txt",
  size: 2048,
  importedAt: 1760000000000,
  sha256: "should-not-leak",
  chunkSize: 8388608,
};

test("打开详情后只保留列表已有字段", () => {
  const details = createFileDetails();
  details.open(file);
  const state = details.getState();
  assert.deepEqual(state, {
    id: "f1",
    name: "季度报告-01.txt",
    size: 2048,
    importedAt: 1760000000000,
  });
  assert.equal("sha256" in state, false);
  assert.equal("chunkSize" in state, false);
});

test("打开详情时忽略无效输入", () => {
  const details = createFileDetails();
  details.open(null);
  details.open({ name: "无 id" });
  details.open("not-a-file");
  assert.equal(details.getState(), null);
});

test("关闭详情会清空状态并通知", () => {
  const seen = [];
  const details = createFileDetails({ onChange: (next) => seen.push(next) });
  details.open(file);
  details.close();
  assert.equal(details.getState(), null);
  assert.equal(seen.length, 2);
  assert.equal(seen[1], null);
});

test("打开另一个文件会替换当前详情", () => {
  const details = createFileDetails();
  details.open(file);
  details.open({ id: "f2", name: "photo.png", size: 10, importedAt: 1 });
  assert.equal(details.getState().id, "f2");
  assert.equal(details.getState().name, "photo.png");
});

test("列表刷新后目标不存在时自动关闭", () => {
  const seen = [];
  const details = createFileDetails({ onChange: (next) => seen.push(next) });
  details.open(file);
  details.sync([{ id: "f2", name: "photo.png", size: 10, importedAt: 1 }]);
  assert.equal(details.getState(), null);
  assert.equal(seen.at(-1), null);
});

test("列表刷新后目标存在时更新元数据", () => {
  const details = createFileDetails();
  details.open(file);
  details.sync([{ ...file, size: 4096, importedAt: 1760000009999 }]);
  const state = details.getState();
  assert.equal(state.size, 4096);
  assert.equal(state.importedAt, 1760000009999);
  assert.equal(state.name, "季度报告-01.txt");
});

test("列表未变化时不重复通知", () => {
  const seen = [];
  const details = createFileDetails({ onChange: (next) => seen.push(next) });
  details.open(file);
  details.sync([{ ...file }]);
  assert.equal(seen.length, 1);
});

test("刷新得到非数组时关闭详情", () => {
  const details = createFileDetails();
  details.open(file);
  details.sync(undefined);
  assert.equal(details.getState(), null);
});

test("获取的状态是副本，修改不会影响内部状态", () => {
  const details = createFileDetails();
  details.open(file);
  const state = details.getState();
  state.name = "被篡改";
  state.size = 0;
  assert.equal(details.getState().name, "季度报告-01.txt");
  assert.equal(details.getState().size, 2048);
});

test("未打开时关闭与同步都是空操作", () => {
  const seen = [];
  const details = createFileDetails({ onChange: (next) => seen.push(next) });
  details.close();
  details.sync([file]);
  assert.equal(details.getState(), null);
  assert.equal(seen.length, 0);
});

test("长文件名原样保留，不做截断", () => {
  const longName = "这是一个非常非常长的文件名".repeat(8) + ".pdf";
  const details = createFileDetails();
  details.open({ ...file, name: longName });
  assert.equal(details.getState().name, longName);
});
