import test from "node:test";
import assert from "node:assert/strict";

import {
  applyQuery,
  countFilesByFolder,
  createFileBrowser,
  filterFiles,
  matchesFolder,
  matchesKeyword,
  matchesType,
  normalizeKeyword,
  sortFiles,
} from "../src/lib/fileBrowser.js";
import { createFileListStore } from "../src/lib/fileListStore.js";

const files = [
  { id: "a", name: "Photo.JPG", size: 300, importedAt: 100 },
  { id: "b", name: "photo.png", size: 100, importedAt: 300 },
  { id: "c", name: "季度报告.pdf", size: 200, importedAt: 200 },
  { id: "d", name: "notes", size: 50, importedAt: 200 },
  { id: "e", name: "Archive.PDF", size: 100, importedAt: 400 },
  { id: "f", name: "photo.jpeg", size: 100, importedAt: 400 },
];

const ids = (list) => list.map((file) => file.id);

test("normalizeKeyword 去除首尾空格并兼容非字符串", () => {
  assert.equal(normalizeKeyword("  photo  "), "photo");
  assert.equal(normalizeKeyword("   "), "");
  assert.equal(normalizeKeyword(undefined), "");
  assert.equal(normalizeKeyword(123), "");
});

test("空关键词返回全部文件的副本", () => {
  const result = filterFiles(files, { keyword: "   " });
  assert.equal(result.length, files.length);
  assert.notEqual(result, files);
  assert.deepEqual(ids(result), ids(files));
});

test("中文关键词按文件名匹配", () => {
  assert.deepEqual(ids(filterFiles(files, { keyword: "季度" })), ["c"]);
  assert.equal(matchesKeyword("季度报告.pdf", "报告"), true);
});

test("英文搜索不区分大小写", () => {
  assert.deepEqual(ids(filterFiles(files, { keyword: "PHOTO" })), ["a", "b", "f"]);
  assert.equal(matchesKeyword("Photo.JPG", "photo.j"), true);
});

test("无扩展名文件归入其他分类并可被搜索", () => {
  assert.equal(matchesType("notes", "other"), true);
  assert.deepEqual(ids(filterFiles(files, { keyword: "notes" })), ["d"]);
});

test("扩展名大小写不影响分类", () => {
  assert.equal(matchesType("Photo.JPG", "image"), true);
  assert.equal(matchesType("Archive.PDF", "pdf"), true);
  assert.equal(matchesType("archive.Pdf", "pdf"), true);
  assert.equal(matchesType("notes", "image"), false);
});

test("图片、PDF 与其他分类互斥且覆盖全部文件", () => {
  const images = filterFiles(files, { type: "image" });
  const pdfs = filterFiles(files, { type: "pdf" });
  const others = filterFiles(files, { type: "other" });
  assert.deepEqual(ids(images), ["a", "b", "f"]);
  assert.deepEqual(ids(pdfs), ["c", "e"]);
  assert.deepEqual(ids(others), ["d"]);
  assert.equal(images.length + pdfs.length + others.length, files.length);
});

test("搜索与类型筛选可以组合", () => {
  assert.deepEqual(ids(filterFiles(files, { keyword: "photo", type: "image" })), [
    "a",
    "b",
    "f",
  ]);
  assert.deepEqual(ids(filterFiles(files, { keyword: "photo", type: "pdf" })), []);
});

test("导入时间排序：新到旧与旧到新", () => {
  assert.deepEqual(ids(sortFiles(files)), ["e", "f", "b", "d", "c", "a"]);
  assert.deepEqual(
    ids(sortFiles(files, { key: "importedAt", direction: "asc" })),
    ["a", "d", "c", "b", "e", "f"]
  );
});

test("文件大小排序：大到小与小到大", () => {
  assert.deepEqual(ids(sortFiles(files, { key: "size", direction: "desc" })), [
    "a",
    "c",
    "e",
    "f",
    "b",
    "d",
  ]);
  assert.deepEqual(ids(sortFiles(files, { key: "size", direction: "asc" })), [
    "d",
    "e",
    "f",
    "b",
    "c",
    "a",
  ]);
});

test("同时间与同大小使用确定性兜底排序", () => {
  const first = sortFiles(files, { key: "size", direction: "desc" });
  const second = sortFiles([...files].reverse(), { key: "size", direction: "desc" });
  assert.deepEqual(ids(first), ids(second));
  assert.deepEqual(ids(first), ["a", "c", "e", "f", "b", "d"]);
});

test("排序与筛选都不修改输入数组", () => {
  const snapshot = files.map((file) => ({ ...file }));
  const order = ids(files);
  sortFiles(files, { key: "size", direction: "asc" });
  filterFiles(files, { keyword: "photo", type: "image" });
  applyQuery(files, { keyword: "photo", type: "image", sortKey: "size" });
  assert.deepEqual(ids(files), order);
  assert.deepEqual(files, snapshot);
});

test("非法排序键回退到导入时间，非法方向按倒序处理", () => {
  assert.deepEqual(ids(sortFiles(files, { key: "name", direction: "asc" })), [
    "a",
    "d",
    "c",
    "b",
    "e",
    "f",
  ]);
  assert.deepEqual(ids(sortFiles(files, { key: "size", direction: "sideways" })), [
    "a",
    "c",
    "e",
    "f",
    "b",
    "d",
  ]);
});

test("applyQuery 按查询条件筛选并排序", () => {
  assert.deepEqual(
    ids(applyQuery(files, { keyword: "", type: "all", sortKey: "size", sortDirection: "desc" })),
    ["a", "c", "e", "f", "b", "d"]
  );
  assert.deepEqual(
    ids(applyQuery(files, { keyword: "photo", type: "image", sortKey: "size", sortDirection: "asc" })),
    ["f", "b", "a"]
  );
});

test("浏览器切换排序后可见列表顺序随之变化", () => {
  const browser = createFileBrowser({ pageSize: 60 });
  browser.setFiles(files);
  assert.deepEqual(ids(browser.snapshot().visible), ["e", "f", "b", "d", "c", "a"]);

  browser.setQuery({ sortKey: "size", sortDirection: "asc" });
  const view = browser.snapshot();
  assert.deepEqual(ids(view.visible), ["d", "e", "f", "b", "c", "a"]);
  assert.equal(view.query.sortKey, "size");
  assert.equal(view.query.sortDirection, "asc");
});

function manyFiles(count = 130) {
  return Array.from({ length: count }, (_, index) => ({
    id: String(index),
    name: `file-${String(index).padStart(3, "0")}.txt`,
    size: index,
    importedAt: index + 1,
  }));
}

test("分页默认为 60 条并可显示更多", () => {
  const browser = createFileBrowser({ pageSize: 60 });
  browser.setFiles(manyFiles());
  let view = browser.snapshot();
  assert.equal(view.visible.length, 60);
  assert.equal(view.hiddenCount, 70);

  browser.showMore();
  view = browser.snapshot();
  assert.equal(view.visible.length, 120);
  assert.equal(view.hiddenCount, 10);

  browser.showMore();
  view = browser.snapshot();
  assert.equal(view.visible.length, 130);
  assert.equal(view.hiddenCount, 0);
});

test("查询条件变化后分页重置为 60", () => {
  const browser = createFileBrowser({ pageSize: 60 });
  browser.setFiles(manyFiles());
  browser.showMore();
  assert.equal(browser.snapshot().visibleCount, 120);

  browser.setQuery({ keyword: "file-0" });
  const view = browser.snapshot();
  assert.equal(view.visibleCount, 60);
  assert.equal(view.query.keyword, "file-0");
  assert.ok(view.matched.length > 0);
});

test("清除筛选恢复完整列表并重置分页", () => {
  const browser = createFileBrowser({ pageSize: 60 });
  browser.setFiles(manyFiles());
  browser.setQuery({ keyword: "不存在的文件", type: "pdf" });
  assert.equal(browser.snapshot().matched.length, 0);

  browser.clearFilters();
  const view = browser.snapshot();
  assert.equal(view.query.keyword, "");
  assert.equal(view.query.type, "all");
  assert.equal(view.matched.length, 130);
  assert.equal(view.visible.length, 60);
  assert.equal(view.isFiltering, false);
});

test("导入刷新新文件列表时重置分页但保留查询条件", () => {
  const browser = createFileBrowser({ pageSize: 60 });
  browser.setFiles(manyFiles());
  browser.setQuery({ type: "image" });
  browser.showMore();

  const next = manyFiles(70);
  browser.setFiles(next);
  const view = browser.snapshot();
  assert.equal(view.query.type, "image");
  assert.equal(view.visibleCount, 60);
  assert.equal(view.total, 70);
});

test("清空浏览器状态会移除文件与查询条件", () => {
  const browser = createFileBrowser({ pageSize: 60 });
  browser.setFiles(manyFiles());
  browser.setQuery({ keyword: "file", type: "other" });
  browser.reset();
  const view = browser.snapshot();
  assert.equal(view.total, 0);
  assert.equal(view.matched.length, 0);
  assert.equal(view.query.keyword, "");
  assert.equal(view.query.type, "all");
  assert.equal(view.visibleCount, 60);
});

test("收藏筛选只保留收藏文件，可与搜索、类型和排序组合", () => {
  const favoriteFiles = [
    { id: "a", name: "Photo.JPG", size: 300, importedAt: 100, favorite: true },
    { id: "b", name: "photo.png", size: 100, importedAt: 300, favorite: false },
    { id: "c", name: "季度报告.pdf", size: 200, importedAt: 200, favorite: true },
    { id: "d", name: "notes.txt", size: 50, importedAt: 200, favorite: false },
  ];

  assert.deepEqual(ids(filterFiles(favoriteFiles, { favoriteOnly: true })), ["a", "c"]);
  assert.deepEqual(
    ids(filterFiles(favoriteFiles, { favoriteOnly: true, keyword: "photo", type: "image" })),
    ["a"]
  );
  assert.deepEqual(
    ids(
      applyQuery(favoriteFiles, {
        favoriteOnly: true,
        sortKey: "size",
        sortDirection: "asc",
      })
    ),
    ["c", "a"]
  );
});

test("收藏筛选状态变化时浏览器立即重新计算且清除筛选会重置", () => {
  const favoriteFiles = [
    { id: "a", name: "a.txt", size: 1, importedAt: 3, favorite: true },
    { id: "b", name: "b.txt", size: 2, importedAt: 2, favorite: false },
    { id: "c", name: "c.txt", size: 3, importedAt: 1, favorite: true },
  ];
  const browser = createFileBrowser({ pageSize: 60 });
  browser.setFiles(favoriteFiles);

  browser.setQuery({ favoriteOnly: true });
  let view = browser.snapshot();
  assert.deepEqual(ids(view.matched), ["a", "c"]);
  assert.equal(view.isFiltering, true);

  // 取消收藏后通过替换文件列表重算，收藏项立即消失。
  browser.setFiles(favoriteFiles.map((file) => ({ ...file, favorite: file.id === "c" })));
  view = browser.snapshot();
  assert.deepEqual(ids(view.matched), ["c"]);

  browser.clearFilters();
  view = browser.snapshot();
  assert.equal(view.query.favoriteOnly, false);
  assert.equal(view.matched.length, 3);
});

test("store 删除与刷新失败时 browser 更新派生结果并保留查询和分页", async () => {
  const sourceFiles = [
    { id: "a", name: "keep-a.txt", size: 50, importedAt: 100 },
    { id: "b", name: "keep-b.txt", size: 40, importedAt: 200 },
    { id: "c", name: "keep-c.txt", size: 30, importedAt: 300 },
    { id: "d", name: "keep-d.txt", size: 20, importedAt: 400 },
    { id: "e", name: "keep-e.txt", size: 10, importedAt: 500 },
    { id: "x", name: "other.pdf", size: 5, importedAt: 600 },
  ];
  let failLoads = false;
  let lastItems = null;
  const browser = createFileBrowser({ pageSize: 2 });
  const store = createFileListStore({
    fetchFiles: async () => {
      if (failLoads) throw { code: "DISK_ERROR", message: "列表刷新失败。" };
      return sourceFiles;
    },
    notify: (next) => {
      // 与 VaultPanel 一致：加载中或错误状态复用 items 时不重置浏览分页。
      if (next.items !== lastItems) {
        lastItems = next.items;
        browser.setFiles(next.items);
      }
    },
  });
  await store.load();
  browser.setQuery({
    keyword: "keep",
    type: "other",
    sortKey: "size",
    sortDirection: "asc",
  });
  browser.showMore();
  assert.equal(browser.snapshot().visibleCount, 4);

  assert.equal(store.remove("c"), true);
  let view = browser.snapshot();
  assert.equal(view.total, 5);
  assert.deepEqual(ids(view.matched), ["e", "d", "b", "a"]);
  assert.deepEqual(ids(view.visible), ["e", "d"]);
  assert.equal(view.visibleCount, 2);
  assert.equal(view.hiddenCount, 2);
  assert.deepEqual(view.query, {
    keyword: "keep",
    type: "other",
    sortKey: "size",
    sortDirection: "asc",
    favoriteOnly: false,
    folderId: null,
  });

  browser.showMore();
  assert.deepEqual(ids(browser.snapshot().visible), ["e", "d", "b", "a"]);
  failLoads = true;
  await store.load();

  assert.equal(store.getState().error.code, "DISK_ERROR");
  assert.deepEqual(ids(store.getState().items), ["x", "e", "d", "b", "a"]);
  view = browser.snapshot();
  assert.equal(view.total, 5);
  assert.deepEqual(ids(view.matched), ["e", "d", "b", "a"]);
  assert.deepEqual(ids(view.visible), ["e", "d", "b", "a"]);
  assert.equal(view.visibleCount, 4);
  assert.equal(view.hiddenCount, 0);
  assert.deepEqual(view.query, {
    keyword: "keep",
    type: "other",
    sortKey: "size",
    sortDirection: "asc",
    favoriteOnly: false,
    folderId: null,
  });
});

test("文件夹筛选与搜索、类型、收藏和排序正确组合", () => {
  const scoped = [
    { id: "a", name: "Photo.JPG", size: 300, importedAt: 100, folderId: "f1", favorite: true },
    { id: "b", name: "photo.png", size: 100, importedAt: 300, folderId: "f1", favorite: false },
    { id: "c", name: "季度报告.pdf", size: 200, importedAt: 200, folderId: "f2", favorite: true },
    { id: "d", name: "notes.txt", size: 50, importedAt: 200, folderId: null, favorite: true },
  ];

  assert.equal(matchesFolder(scoped[0], "f1"), true);
  assert.equal(matchesFolder(scoped[0], "f2"), false);
  assert.equal(matchesFolder(scoped[0], null), true);
  assert.deepEqual(ids(filterFiles(scoped, { folderId: "f1" })), ["a", "b"]);
  assert.deepEqual(
    ids(filterFiles(scoped, { folderId: "f1", keyword: "photo", type: "image" })),
    ["a", "b"]
  );
  assert.deepEqual(ids(filterFiles(scoped, { folderId: "f1", favoriteOnly: true })), ["a"]);
  assert.deepEqual(ids(filterFiles(scoped, { folderId: "f1", type: "pdf" })), []);
  assert.deepEqual(ids(filterFiles(scoped, { folderId: null })), ["a", "b", "c", "d"]);
  assert.deepEqual(
    ids(applyQuery(scoped, { folderId: "f1", sortKey: "size", sortDirection: "asc" })),
    ["b", "a"]
  );
});

test("countFilesByFolder 只统计有文件夹归属的文件", () => {
  const counts = countFilesByFolder([
    { id: "a", folderId: "f1" },
    { id: "b", folderId: "f1" },
    { id: "c", folderId: "f2" },
    { id: "d", folderId: null },
    { id: "e" },
  ]);
  assert.deepEqual(counts, { f1: 2, f2: 1 });
  assert.deepEqual(countFilesByFolder(undefined), {});
});

test("store.replace 更新文件夹归属并触发 browser 重算", async () => {
  const source = [
    { id: "a", name: "a.txt", size: 10, importedAt: 2, favorite: false, folderId: null },
    { id: "b", name: "b.txt", size: 20, importedAt: 1, favorite: false, folderId: null },
  ];
  let lastItems = null;
  const browser = createFileBrowser({ pageSize: 60 });
  const store = createFileListStore({
    fetchFiles: async () => source,
    notify: (next) => {
      if (next.items !== lastItems) {
        lastItems = next.items;
        browser.setFiles(next.items);
      }
    },
  });
  await store.load();
  browser.setQuery({ folderId: "f1" });
  assert.equal(browser.snapshot().matched.length, 0);

  assert.equal(
    store.replace({
      id: "a",
      name: "a.txt",
      size: 10,
      importedAt: 2,
      favorite: false,
      folderId: "f1",
    }),
    true
  );
  assert.equal(store.getState().items.find((file) => file.id === "a").folderId, "f1");
  assert.deepEqual(ids(browser.snapshot().matched), ["a"]);

  // 仅重命名（响应不带 folderId）时保留原归属。
  store.replace({ id: "a", name: "renamed.txt", size: 10, importedAt: 2, favorite: false });
  assert.equal(store.getState().items.find((file) => file.id === "a").folderId, "f1");

  // 显式 null 表示移回根目录。
  store.replace({
    id: "a",
    name: "renamed.txt",
    size: 10,
    importedAt: 2,
    favorite: false,
    folderId: null,
  });
  assert.equal(store.getState().items.find((file) => file.id === "a").folderId, null);
  assert.equal(browser.snapshot().matched.length, 0);
});
