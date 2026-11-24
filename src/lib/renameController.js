// FR-32 文件重命名状态：只做前端预校验与 IPC 协调。
//
// 安全边界：
// - 这里只发送新的显示名称，不接触密文、密钥或文件路径。
// - 前端校验只是提前反馈，最终以 Rust 校验与索引原子写入为准。
// - 名称规则与后端保持一致；不一致时以后端错误码覆盖前端判断。

export const FILE_NAME_MAX_LENGTH = 100;

const ILLEGAL_CHARS = /[/\\:]/;
const UNSAFE_CHARS = /[\u0000-\u001F\u007F\u061C\u200E\u200F\u202A-\u202E\u2066-\u2069]/;

function extensionOf(name) {
  const dot = name.lastIndexOf(".");
  return dot > 0 && dot < name.length - 1 ? name.slice(dot + 1) : null;
}

/**
 * 校验重命名后的完整名称。
 * 传入 originalName 时要求扩展名不变，避免重命名改变文件类型分类。
 */
export function validateFileName(value, originalName) {
  const name = typeof value === "string" ? value.trim() : "";
  if (!name) {
    return { ok: false, name, code: "FILE_NAME_EMPTY", message: "文件名不能为空。" };
  }
  if ([...name].length > FILE_NAME_MAX_LENGTH) {
    return {
      ok: false,
      name,
      code: "FILE_NAME_TOO_LONG",
      message: `文件名不能超过 ${FILE_NAME_MAX_LENGTH} 个字符。`,
    };
  }
  if (
    ILLEGAL_CHARS.test(name) ||
    UNSAFE_CHARS.test(name) ||
    name === "." ||
    name === ".."
  ) {
    return {
      ok: false,
      name,
      code: "FILE_NAME_INVALID",
      message: "文件名不能包含 / \\ : 等非法字符。",
    };
  }
  if (typeof originalName === "string" && originalName.length > 0) {
    const before = extensionOf(originalName);
    const after = extensionOf(name);
    if ((before ?? "").toLowerCase() !== (after ?? "").toLowerCase()) {
      return {
        ok: false,
        name,
        code: "FILE_NAME_EXTENSION_LOCKED",
        message: "重命名需要保留原扩展名。",
      };
    }
  }
  return { ok: true, name, code: null, message: "" };
}

/** 编辑时默认选中主文件名，保留扩展名。 */
export function baseNameSelection(name) {
  const text = typeof name === "string" ? name : "";
  const dot = text.lastIndexOf(".");
  return { start: 0, end: dot > 0 ? dot : text.length };
}

const IDLE_STATE = Object.freeze({
  target: null,
  draft: "",
  busy: false,
  error: null,
});

export function createRenameController({ renameById, notify } = {}) {
  let state = { ...IDLE_STATE };
  let disposed = false;
  let operationId = 0;

  function snapshot() {
    return {
      target: state.target ? { ...state.target } : null,
      draft: state.draft,
      busy: state.busy,
      error: state.error ? { ...state.error } : null,
    };
  }

  function publish(next) {
    state = next;
    notify?.(snapshot());
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
    const name = typeof file.name === "string" ? file.name : "";
    publish({ target: { id: file.id, name }, draft: name, busy: false, error: null });
    return true;
  }

  function setDraft(value) {
    if (disposed || state.busy || !state.target) return;
    publish({ ...state, draft: typeof value === "string" ? value : "", error: null });
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
    const validation = validateFileName(state.draft, target.name);
    if (!validation.ok) {
      const error = { code: validation.code, message: validation.message };
      publish({ ...state, error });
      return { status: "invalid", file: target, error };
    }

    const ticket = ++operationId;
    publish({ ...state, busy: true, error: null });
    try {
      const result = await renameById(target.id, validation.name);
      if (disposed || ticket !== operationId) return null;
      const name =
        typeof result?.name === "string" && result.name.length > 0
          ? result.name
          : validation.name;
      publish({ ...IDLE_STATE });
      return { status: "committed", file: { id: target.id, name }, result };
    } catch (error) {
      if (disposed || ticket !== operationId) return null;
      // 保留草稿，让用户能修改后重试。
      publish({ ...state, busy: false, error: null });
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
    setDraft,
    cancel,
    confirm,
    dispose,
    getState: snapshot,
  };
}
