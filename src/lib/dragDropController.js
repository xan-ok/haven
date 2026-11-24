// FR-35 拖拽提示状态机：只描述是否有拖拽悬停以及数量。
//
// 安全边界：
// - 这里只处理路径字符串，不读取、不解密任何文件内容。
// - 实际导入仍由 Rust 端校验普通文件、符号链接与路径，并完成加密。

const IDLE_STATE = Object.freeze({ active: false, count: 0 });

/** 仅保留非空字符串路径，并去掉同一批次中的重复项。 */
export function normalizeDroppedPaths(paths) {
  if (!Array.isArray(paths)) return [];
  const seen = new Set();
  const result = [];
  for (const path of paths) {
    if (typeof path !== "string") continue;
    const trimmed = path.trim();
    if (!trimmed || seen.has(trimmed)) continue;
    seen.add(trimmed);
    result.push(trimmed);
  }
  return result;
}

export function createDragDropController({ notify } = {}) {
  let state = { ...IDLE_STATE };
  let disposed = false;

  function publish(next) {
    if (next.active === state.active && next.count === state.count) return;
    state = next;
    notify?.({ ...state });
  }

  function enter(paths) {
    if (disposed) return;
    const count = normalizeDroppedPaths(paths).length;
    publish(count > 0 ? { active: true, count } : { ...IDLE_STATE });
  }

  function over() {
    // 悬停期间位置变化不影响提示内容。
  }

  function leave() {
    if (disposed) return;
    publish({ ...IDLE_STATE });
  }

  /** 结束提示并返回去重后的路径；调用方负责进入现有导入流程。 */
  function drop(paths) {
    publish({ ...IDLE_STATE });
    if (disposed) return [];
    return normalizeDroppedPaths(paths);
  }

  function dispose() {
    disposed = true;
    state = { ...IDLE_STATE };
  }

  return {
    enter,
    over,
    leave,
    drop,
    dispose,
    getState: () => ({ ...state }),
  };
}
