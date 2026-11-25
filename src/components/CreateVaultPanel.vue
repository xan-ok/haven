<script setup>
import { computed, ref } from "vue";
import { NButton, NCheckbox, NInput } from "naive-ui";
import { chooseDirectory, createVault, normalizeError } from "../lib/vaultClient.js";
import {
  PASSWORD_MAX_CHARS,
  PASSWORD_MIN_CHARS,
  countPasswordChars,
  evaluateStrength,
  validateCreation,
} from "../lib/password.js";

const emit = defineEmits(["created", "cancel"]);

const parentDir = ref("");
const password = ref("");
const confirm = ref("");
const acknowledged = ref(false);
const submitting = ref(false);
const error = ref("");

const charCount = computed(() => countPasswordChars(password.value));
const strength = computed(() => evaluateStrength(password.value));
const validation = computed(() =>
  validateCreation({
    password: password.value,
    confirm: confirm.value,
    acknowledged: acknowledged.value,
  })
);
const canSubmit = computed(
  () => Boolean(parentDir.value) && validation.value.valid && !submitting.value
);
const showMismatch = computed(
  () => confirm.value.length > 0 && password.value !== confirm.value
);

const strengthWidth = computed(() => {
  if (charCount.value === 0) return "0%";
  if (strength.value.level === "strong") return "100%";
  if (strength.value.level === "medium") return "66%";
  return "33%";
});
const strengthColor = computed(
  () =>
    ({
      weak: "var(--haven-danger)",
      medium: "var(--haven-warning)",
      strong: "var(--haven-accent)",
    })[strength.value.level] || "transparent"
);

async function pickDirectory() {
  error.value = "";
  try {
    const dir = await chooseDirectory("选择保险库的存放位置");
    if (dir) parentDir.value = dir;
  } catch {
    error.value = "无法打开位置选择器，请重试。";
  }
}

async function submit() {
  if (!canSubmit.value) return;
  submitting.value = true;
  error.value = "";
  try {
    const status = await createVault(parentDir.value, password.value);
    password.value = "";
    confirm.value = "";
    emit("created", status);
  } catch (err) {
    error.value = normalizeError(err).message;
  } finally {
    submitting.value = false;
  }
}
</script>

<template>
  <section class="haven-card" aria-labelledby="create-title">
    <p class="haven-eyebrow">创建保险库</p>
    <h1 id="create-title" class="haven-section-title">给抽屉上一把锁</h1>
    <p class="haven-lead">
      Haven 会在你选择的位置创建「我的保险库」。主密码只用于加密，不会被保存。
    </p>

    <div class="haven-field">
      <span class="haven-label">存放位置</span>
      <div class="haven-actions__row" style="justify-content: flex-start">
        <n-button size="small" secondary :disabled="submitting" @click="pickDirectory">
          选择位置
        </n-button>
        <span class="haven-help">{{ parentDir ? "将在此处创建「我的保险库」" : "尚未选择位置" }}</span>
      </div>
      <p v-if="parentDir" class="haven-path">{{ parentDir }}</p>
    </div>

    <div class="haven-field">
      <div class="haven-field__head">
        <span class="haven-label">主密码</span>
        <span class="haven-counter">{{ charCount }} 个字符</span>
      </div>
      <n-input
        v-model:value="password"
        type="password"
        show-password-on="click"
        size="large"
        placeholder="至少 12 个字符"
        :disabled="submitting"
        :input-props="{ autocomplete: 'new-password', spellcheck: 'false' }"
      />
      <div class="haven-strength">
        <div class="haven-strength__track">
          <div
            class="haven-strength__bar"
            :style="{ width: strengthWidth, backgroundColor: strengthColor }"
          />
        </div>
        <div class="haven-strength__meta">
          <span>强度：{{ strength.label }}</span>
          <span>{{ strength.hint }}</span>
        </div>
      </div>
    </div>

    <div class="haven-field">
      <span class="haven-label">确认主密码</span>
      <n-input
        v-model:value="confirm"
        type="password"
        show-password-on="click"
        size="large"
        placeholder="再输入一次"
        :disabled="submitting"
        :input-props="{ autocomplete: 'new-password', spellcheck: 'false' }"
      />
      <p v-if="showMismatch" class="haven-help" style="color: var(--haven-danger)">
        两次输入的密码不一致。
      </p>
    </div>

    <ul class="haven-rule-list">
      <li>至少 {{ PASSWORD_MIN_CHARS }} 个字符，最多 {{ PASSWORD_MAX_CHARS }} 个字符。</li>
      <li>按 Unicode 字符计数，区分大小写；空格有效，Haven 不会自动去除或改写。</li>
      <li>忘记主密码将无法恢复保险库内的任何内容。</li>
    </ul>

    <div class="haven-checkline">
      <n-checkbox v-model:checked="acknowledged" :disabled="submitting">
        我已了解：忘记主密码将无法恢复任何文件，Haven 不提供找回方式。
      </n-checkbox>
    </div>

    <div v-if="error" class="haven-error" role="alert">{{ error }}</div>

    <div class="haven-actions">
      <div class="haven-actions__row">
        <n-button quaternary size="large" :disabled="submitting" @click="emit('cancel')">
          取消
        </n-button>
        <n-button
          type="primary"
          size="large"
          :loading="submitting"
          :disabled="!canSubmit"
          @click="submit"
        >
          {{ submitting ? "正在创建保险库…" : "创建保险库" }}
        </n-button>
      </div>
      <p class="haven-hint">
        创建过程通常在一秒内完成，请不要关闭窗口。
      </p>
    </div>
  </section>
</template>
