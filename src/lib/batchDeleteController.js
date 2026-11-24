// FR-33 批量删除：逐项调用现有单文件删除命令，复用 5.4 删除事务。
//
// 不并行、不重试、不合并事务；某一项失败时保留其余文件并继续，
// 只有锁定或恢复状态不明才立即停止，避免掩盖成功/失败数量。

const IDLE_STATE = Object.freeze({
  open: false,
  ids: [],
  total: 0,
  busy: false,
  processed: 0,
});

function uniqueIds(values) {
  return [...new Set(
    (Array.isArray(values) ? values : []).filter(
      (value) => typeof value === "string" && value.length > 0
    )
  )];
}

export function createBatchDeleteController({ deleteById, notify } = {}) {
  let state = { ...IDLE_STATE };
  let disposed = false;
  let operationId = 0;

  function snapshot() {
    return { ...state, ids: [...state.ids] };
  }

  function publish(next) {
    state = next;
    notify?.(snapshot());
  }

  function request(ids) {
    if (disposed || state.busy) return false;
    const unique = uniqueIds(ids);
    if (unique.length === 0) return false;
    operationId += 1;
    publish({
      open: true,
      ids: unique,
      total: unique.length,
      busy: false,
      processed: 0,
    });
    return true;
  }

  function cancel() {
    if (disposed || state.busy || !state.open) return false;
    operationId += 1;
    publish({ ...IDLE_STATE });
    return true;
  }

  async function confirm() {
    if (disposed || state.busy || !state.open) return null;

    const ids = [...state.ids];
    const ticket = ++operationId;
    publish({ ...state, busy: true });

    const succeeded = [];
    const failures = [];
    let cleanupPending = false;
    let fatal = null;

    for (const id of ids) {
      try {
        const result = await deleteById(id);
        if (disposed || ticket !== operationId) return null;
        succeeded.push(id);
        if (result?.cleanupPending) cleanupPending = true;
      } catch (error) {
        if (disposed || ticket !== operationId) return null;
        const code = typeof error?.code === "string" ? error.code : "INTERNAL";
        if (code === "NOT_UNLOCKED" || code === "VAULT_RECOVERY_REQUIRED") {
          fatal = { id, code, error };
          break;
        }
        failures.push({ id, error });
      }
      publish({ ...state, processed: succeeded.length + failures.length });
    }

    const unprocessed = ids.length - succeeded.length - failures.length - (fatal ? 1 : 0);
    publish({ ...IDLE_STATE });
    return {
      status: fatal ? "interrupted" : failures.length > 0 ? "partial" : "committed",
      succeeded,
      failures,
      fatal,
      unprocessed,
      cleanupPending,
    };
  }

  function dispose() {
    disposed = true;
    operationId += 1;
    state = { ...IDLE_STATE };
  }

  return {
    request,
    cancel,
    confirm,
    dispose,
    getState: snapshot,
  };
}
