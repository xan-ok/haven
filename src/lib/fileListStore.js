// 文件列表状态机：只做排序、加载与错误状态，不接触任何密钥或解密逻辑。

export function sortFilesDesc(items) {
  return [...items].sort(
    (left, right) => (Number(right.importedAt) || 0) - (Number(left.importedAt) || 0)
  );
}

export function createFileListStore({ fetchFiles, notify }) {
  let state = { items: [], loading: false, error: null };
  let disposed = false;
  let requestId = 0;

  function publish(next) {
    state = next;
    notify?.({ ...state });
  }

  async function load() {
    if (disposed) return;
    const ticket = ++requestId;
    publish({ ...state, loading: true, error: null });
    try {
      const items = await fetchFiles();
      if (disposed || ticket !== requestId) return;
      publish({
        items: sortFilesDesc(Array.isArray(items) ? items : []),
        loading: false,
        error: null,
      });
    } catch (error) {
      if (disposed || ticket !== requestId) return;
      const code = typeof error?.code === "string" ? error.code : "INTERNAL";
      publish({
        // 保险库不可用时旧列表已不可信，必须立即清空，避免继续展示已无法读取的文件名。
        items: code === "VAULT_UNAVAILABLE" ? [] : state.items,
        loading: false,
        error: {
          code,
          message:
            typeof error?.message === "string" ? error.message : "无法读取文件列表，请重试。",
        },
      });
    }
  }

  /** 保险库不可用等致命错误后立即清空列表，旧条目已不可信。 */
  function clear() {
    if (disposed) return;
    requestId += 1;
    publish({ items: [], loading: false, error: null });
  }

  /** 删除已提交后本地移除，并使任何旧加载结果失效。 */
  function remove(id) {
    if (disposed) return false;
    requestId += 1;
    const items = state.items.filter((file) => file?.id !== id);
    const removed = items.length !== state.items.length;
    publish({
      items: removed ? items : state.items,
      loading: false,
      error: null,
    });
    return removed;
  }

  /** 重命名、收藏或移动成功后按 id 原位更新，并按现有排序重新派生列表。 */
  function replace(file) {
    if (disposed || !file || typeof file.id !== "string") return false;
    requestId += 1;
    let updated = false;
    const items = state.items.map((item) => {
      if (item?.id !== file.id) return item;
      updated = true;
      const next = {
        ...item,
        name: typeof file.name === "string" ? file.name : item.name,
        favorite: typeof file.favorite === "boolean" ? file.favorite : item.favorite,
      };
      // 仅在后端明确返回 folderId 时更新归属，避免重命名等响应意外清空文件夹。
      if (Object.prototype.hasOwnProperty.call(file, "folderId")) {
        next.folderId = typeof file.folderId === "string" ? file.folderId : null;
      }
      return next;
    });
    publish({
      items: updated ? sortFilesDesc(items) : state.items,
      loading: false,
      error: null,
    });
    return updated;
  }

  function dispose() {
    disposed = true;
    requestId += 1;
  }

  return {
    load,
    clear,
    remove,
    replace,
    dispose,
    getState: () => ({ ...state }),
  };
}
