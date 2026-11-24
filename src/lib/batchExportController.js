// FR-37 批量导出：串行调用现有单文件导出命令，复用导出事务与完整性校验。
//
// 安全边界：
// - 前端只提交目标路径，不读取明文、不写入文件；解密与校验都在 Rust 端完成。
// - 已存在的目标文件由后端拒绝，这里按单项失败继续，不覆盖任何文件。
// - 锁定或恢复状态不明时立即停止后续任务。

function uniqueItems(values) {
  const seen = new Set();
  const result = [];
  for (const item of Array.isArray(values) ? values : []) {
    if (!item || typeof item.id !== "string" || item.id.length === 0) continue;
    if (typeof item.name !== "string" || item.name.length === 0) continue;
    if (seen.has(item.id)) continue;
    seen.add(item.id);
    result.push({ id: item.id, name: item.name });
  }
  return result;
}

/** 目标目录下的同名文件路径；目录来自系统选择器，为绝对路径。 */
export function buildExportTarget(directory, name) {
  const base = String(directory).replace(/\/+$/, "");
  return `${base}/${name}`;
}

/**
 * FR-43：收集指定文件夹内的文件用于导出。
 * 只按稳定 folderId 匹配，不包含其他文件夹与根目录文件；空文件夹返回空数组。
 */
export function collectFolderExportItems(files, folderId) {
  if (typeof folderId !== "string" || folderId.length === 0) return [];
  return (Array.isArray(files) ? files : [])
    .filter(
      (file) =>
        file &&
        typeof file.id === "string" &&
        file.id.length > 0 &&
        typeof file.name === "string" &&
        file.name.length > 0 &&
        file.folderId === folderId
    )
    .map((file) => ({ id: file.id, name: file.name }));
}

const IDLE_STATE = Object.freeze({
  status: "idle",
  total: 0,
  processed: 0,
  succeeded: 0,
  failed: 0,
});

export function createBatchExportController({ exportById, notify } = {}) {
  let state = { ...IDLE_STATE };
  let disposed = false;
  let ticket = 0;
  let cancelRequested = false;

  function publish(next) {
    state = next;
    notify?.({ ...state });
  }

  async function start(items, directory) {
    if (disposed || state.status === "running") return null;
    const list = uniqueItems(items);
    if (list.length === 0) return null;
    if (typeof directory !== "string" || directory.length === 0) return null;

    const current = ++ticket;
    cancelRequested = false;
    publish({
      status: "running",
      total: list.length,
      processed: 0,
      succeeded: 0,
      failed: 0,
    });

    const succeeded = [];
    const failures = [];
    let fatal = null;

    for (const item of list) {
      if (disposed || current !== ticket) return null;
      if (cancelRequested) break;

      try {
        await exportById(item.id, buildExportTarget(directory, item.name));
        if (disposed || current !== ticket) return null;
        succeeded.push({ id: item.id, name: item.name });
      } catch (error) {
        if (disposed || current !== ticket) return null;
        const code = typeof error?.code === "string" ? error.code : "INTERNAL";
        if (code === "NOT_UNLOCKED" || code === "VAULT_RECOVERY_REQUIRED") {
          fatal = { id: item.id, name: item.name, code };
          break;
        }
        failures.push({
          id: item.id,
          name: item.name,
          code,
          message:
            typeof error?.message === "string" && error.message
              ? error.message
              : "导出失败，请重试。",
        });
      }

      publish({
        status: "running",
        total: list.length,
        processed: succeeded.length + failures.length,
        succeeded: succeeded.length,
        failed: failures.length,
      });
    }

    const wasCancelled = cancelRequested;
    publish({ ...IDLE_STATE });
    return {
      status: fatal
        ? "interrupted"
        : wasCancelled
          ? "cancelled"
          : failures.length > 0
            ? "partial"
            : "committed",
      succeeded,
      failures,
      fatal,
      total: list.length,
      unprocessed: list.length - succeeded.length - failures.length - (fatal ? 1 : 0),
    };
  }

  /** 只停止尚未开始的项；正在写出的单项会等待其完成。 */
  function cancel() {
    if (disposed || state.status !== "running") return false;
    cancelRequested = true;
    return true;
  }

  function dispose() {
    disposed = true;
    ticket += 1;
    state = { ...IDLE_STATE };
  }

  return {
    start,
    cancel,
    dispose,
    getState: () => ({ ...state }),
  };
}
