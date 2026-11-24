// 主密码相关的纯函数：码点计数、强度提示与创建校验。
// 规则与 Rust 后端保持一致：按 Unicode 码点计数，不 trim、不做 Unicode 归一化。

export const PASSWORD_MIN_CHARS = 12;
export const PASSWORD_MAX_CHARS = 1024;

/** 按 Unicode 码点统计字符数（与 Rust 的 chars().count() 对齐）。 */
export function countPasswordChars(value) {
  return Array.from(value ?? "").length;
}

function countCharacterClasses(value) {
  const classes = [/[a-z]/, /[A-Z]/, /[0-9]/, /[^\p{L}\p{N}]/u];
  return classes.filter((pattern) => pattern.test(value)).length;
}

/** 三档强度提示。不阻止创建，只给出可执行建议。 */
export function evaluateStrength(value) {
  const chars = countPasswordChars(value);
  if (chars === 0) {
    return {
      level: "empty",
      label: "尚未输入",
      hint: `建议使用至少 ${PASSWORD_MIN_CHARS} 个字符的密码或短语。`,
    };
  }
  if (chars < PASSWORD_MIN_CHARS) {
    return {
      level: "weak",
      label: "偏弱",
      hint: `还需要 ${PASSWORD_MIN_CHARS - chars} 个字符。`,
    };
  }

  const classes = countCharacterClasses(value);
  let score = 0;
  if (chars >= 16) score += 1;
  if (chars >= 24) score += 1;
  if (classes >= 3) score += 1;
  if (/\s/u.test(value) && chars >= 16) score += 1;

  if (score >= 3) {
    return {
      level: "strong",
      label: "较强",
      hint: "长度充足。请确保这段密码只用于 Haven，并妥善记住。",
    };
  }
  if (score >= 1) {
    return {
      level: "medium",
      label: "一般",
      hint: "可以通过增加长度或单词数继续提升强度。",
    };
  }
  return {
    level: "weak",
    label: "偏弱",
    hint: "建议增加长度，或改用多个单词组成的短语。",
  };
}

/** 创建保险库时的完整校验，返回禁用按钮所需的原因。 */
export function validateCreation({ password = "", confirm = "", acknowledged = false } = {}) {
  const chars = countPasswordChars(password);
  if (chars === 0) {
    return { valid: false, reason: "empty", message: "请输入主密码。" };
  }
  if (chars < PASSWORD_MIN_CHARS) {
    return {
      valid: false,
      reason: "short",
      message: `主密码至少需要 ${PASSWORD_MIN_CHARS} 个字符。`,
    };
  }
  if (chars > PASSWORD_MAX_CHARS) {
    return {
      valid: false,
      reason: "long",
      message: `主密码不能超过 ${PASSWORD_MAX_CHARS} 个字符。`,
    };
  }
  if (password !== confirm) {
    return { valid: false, reason: "mismatch", message: "两次输入的密码不一致。" };
  }
  if (!acknowledged) {
    return {
      valid: false,
      reason: "unacknowledged",
      message: "请先确认「忘记主密码将无法恢复」。",
    };
  }
  return { valid: true, reason: "ok", message: "" };
}
