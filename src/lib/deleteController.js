// 单文件删除确认状态：只协调界面与 IPC，不接触文件系统、密钥或明文。

const IDLE_STATE = Object.freeze({ target: null, busy: false });

function snapshot(state) {
  return {
    target: state.target ? { ...state.target } : null,
    busy: state.busy,
  };
}

export function createDeleteController({ deleteById, notify }) {
  let state = { ...IDLE_STATE };
  let disposed = false;
  let operationId = 0;

  function publish(next) {
    state = next;
    notify?.(snapshot(state));
  }

  function open(file) {
    if (
      disposed ||
      state.busy ||
      !file ||
      typeof file.id !== "string" ||
      file.id.length === 0
    ) {
      return false;
    }
    operationId += 1;
    publish({
      target: { id: file.id, name: typeof file.name === "string" ? file.name : "" },
      busy: false,
    });
    return true;
  }

  function cancel() {
    if (disposed || state.busy || !state.target) return false;
    operationId += 1;
    publish({ ...IDLE_STATE });
    return true;
  }

  async function confirm() {
    if (disposed || state.busy || !state.target) return null;
    const target = { ...state.target };
    const ticket = ++operationId;
    publish({ target, busy: true });

    try {
      const result = await deleteById(target.id);
      if (disposed || ticket !== operationId) return null;
      publish({ ...IDLE_STATE });
      return {
        status: "committed",
        file: target,
        result: { cleanupPending: Boolean(result?.cleanupPending) },
      };
    } catch (error) {
      if (disposed || ticket !== operationId) return null;
      publish({ ...IDLE_STATE });
      return { status: "failed", file: target, error };
    }
  }

  function dispose() {
    disposed = true;
    operationId += 1;
    state = { ...IDLE_STATE };
  }

  return {
    open,
    cancel,
    confirm,
    dispose,
    getState: () => snapshot(state),
  };
}
