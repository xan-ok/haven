import test from "node:test";
import assert from "node:assert/strict";
import {
  createDragDropController,
  normalizeDroppedPaths,
} from "../src/lib/dragDropController.js";

function setup() {
  const states = [];
  const controller = createDragDropController({
    notify: (state) => states.push(state),
  });
  return { controller, states };
}

test("normalizeDroppedPaths 过滤空值并去掉同一批次的重复路径", () => {
  assert.deepEqual(normalizeDroppedPaths(null), []);
  assert.deepEqual(normalizeDroppedPaths([1, "", "  ", "a.txt"]), ["a.txt"]);
  assert.deepEqual(normalizeDroppedPaths(["/a.txt", "/a.txt", "/b.pdf"]), [
    "/a.txt",
    "/b.pdf",
  ]);
});

test("拖入文件时显示提示，悬停不重复通知", () => {
  const { controller, states } = setup();

  controller.enter(["/a.txt", "/b.pdf"]);
  assert.deepEqual(controller.getState(), { active: true, count: 2 });
  assert.equal(states.length, 1);

  controller.over();
  assert.equal(states.length, 1, "over 不改变提示");
  assert.deepEqual(controller.getState(), { active: true, count: 2 });
});

test("拖拽离开窗口时取消提示", () => {
  const { controller, states } = setup();

  controller.enter(["/a.txt"]);
  controller.leave();

  assert.deepEqual(controller.getState(), { active: false, count: 0 });
  assert.deepEqual(states.at(-1), { active: false, count: 0 });
});

test("拖入空路径或非法载荷时不显示提示", () => {
  const { controller, states } = setup();

  controller.enter([]);
  controller.enter(undefined);
  controller.enter([""]);

  assert.deepEqual(controller.getState(), { active: false, count: 0 });
  assert.equal(states.length, 0, "空闲状态无需重复通知");
});

test("松开后返回去重路径并立即结束提示", () => {
  const { controller, states } = setup();

  controller.enter(["/a.txt", "/b.pdf"]);
  const dropped = controller.drop(["/a.txt", "/a.txt", "/b.pdf"]);

  assert.deepEqual(dropped, ["/a.txt", "/b.pdf"]);
  assert.deepEqual(controller.getState(), { active: false, count: 0 });
  assert.deepEqual(states.at(-1), { active: false, count: 0 });
});

test("松开空载荷返回空数组且不产生导入", () => {
  const { controller } = setup();

  controller.enter(["/a.txt"]);
  const dropped = controller.drop([]);

  assert.deepEqual(dropped, []);
  assert.deepEqual(controller.getState(), { active: false, count: 0 });
});

test("dispose 后不再通知，也不会返回可导入路径", () => {
  const { controller, states } = setup();

  controller.enter(["/a.txt"]);
  const count = states.length;
  controller.dispose();

  controller.enter(["/b.txt"]);
  controller.leave();
  const dropped = controller.drop(["/b.txt"]);

  assert.equal(states.length, count);
  assert.deepEqual(dropped, []);
  assert.deepEqual(controller.getState(), { active: false, count: 0 });
});
