// FR-33 多选状态：只保存文件 ID，不接触名称、路径、密钥或任何文件数据。
//
// 选择必须始终限定在当前可见（筛选后、已分页显示）的文件内，
// 避免搜索或筛选隐藏文件后仍能批量删除不可见条目。

function validIds(values) {
  const seen = new Set();
  const ids = [];
  for (const value of Array.isArray(values) ? values : []) {
    if (typeof value !== "string" || value.length === 0 || seen.has(value)) continue;
    seen.add(value);
    ids.push(value);
  }
  return ids;
}

/** 返回与可见列表的交集，保持原有选中顺序。 */
export function pruneSelection(ids, visibleIds) {
  const visible = new Set(validIds(visibleIds));
  return validIds(ids).filter((id) => visible.has(id));
}

export function createSelectionController({ notify } = {}) {
  let selected = new Set();
  let disposed = false;

  function snapshot() {
    const ids = [...selected];
    return { ids, idSet: new Set(selected), count: ids.length };
  }

  function publish() {
    notify?.(snapshot());
  }

  function toggle(id, checked) {
    if (disposed || typeof id !== "string" || id.length === 0) return false;
    const next = checked === true;
    if (next === selected.has(id)) return false;
    if (next) selected.add(id);
    else selected.delete(id);
    publish();
    return true;
  }

  function selectAll(ids) {
    if (disposed) return false;
    const next = new Set(validIds(ids));
    if (next.size === selected.size && [...next].every((id) => selected.has(id))) return false;
    selected = next;
    publish();
    return true;
  }

  function clear() {
    if (disposed || selected.size === 0) return false;
    selected = new Set();
    publish();
    return true;
  }

  /** 列表、搜索、筛选或分页变化后调用；不在可见集合中的选择立即失效。 */
  function reconcile(visibleIds) {
    if (disposed) return false;
    const kept = pruneSelection([...selected], visibleIds);
    if (kept.length === selected.size) return false;
    selected = new Set(kept);
    publish();
    return true;
  }

  function has(id) {
    return selected.has(id);
  }

  function dispose() {
    disposed = true;
    selected = new Set();
  }

  return {
    toggle,
    selectAll,
    clear,
    reconcile,
    has,
    dispose,
    getState: snapshot,
  };
}
