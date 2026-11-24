<script setup>
import { computed, nextTick, onBeforeUnmount, onMounted, ref } from "vue";
import { NButton, NInput } from "naive-ui";
import { formatFileSize } from "../lib/fileFormat.js";
import {
  chooseDirectory,
  normalizeError,
  onRestoreProgress,
  openVault,
  restoreVault,
  unlockVault,
  verifyBackupPassword,
} from "../lib/vaultClient.js";

const props = defineProps({
  vaultName: { type: String, default: "我的保险库" },
  vaultPath: { type: String, default: "" },
});
const emit = defineEmits(["unlocked", "unavailable", "relocate", "opened"]);

const UNAVAILABLE_CODES = new Set([
  "VAULT_NOT_FOUND",
  "METADATA_MISSING",
  "METADATA_INVALID",
  "UNSUPPORTED_VERSION",
]);

const password = ref("");
const submitting = ref(false);
const slow = ref(false);
const error = ref("");
const passwordInput = ref(null);
let slowTimer = null;

const canSubmit = computed(() => password.value.length > 0 && !submitting.value);

// FR-47：锁定屏的独立恢复流程，复用 FR-46 的验证与恢复命令。
const restoreOpen = ref(false);
const restoreBackupDir = ref("");
const restorePassword = ref("");
const restoreVerifying = ref(false);
const restoreDialogError = ref("");
const restoreError = ref("");
const restoreState = ref({ status: "idle", processed: 0, total: 0, bytes: 0 });
const restoreResult = ref(null);
const openingRestored = ref(false);
const restorePasswordInput = ref(null);
let unlistenRestore = null;

const restoreRunning = computed(() => restoreState.value.status === "running");

function clearRestorePassword() {
  restorePassword.value = "";
  restoreDialogError.value = "";
}

function handleRestoreProgress(progress) {
  if (restoreState.value.status !== "running") return;
  restoreState.value = {
    status: "running",
    processed: progress?.processed ?? 0,
    total: progress?.total ?? 0,
    bytes: progress?.bytes ?? 0,
  };
}

function closeRestoreDialog() {
  // 验证进行中不允许关闭，避免被误认为已取消。
  if (restoreVerifying.value) return;
  restoreOpen.value = false;
  restoreBackupDir.value = "";
  clearRestorePassword();
}

/** 选择备份目录后打开密码对话框；取消选择直接返回。 */
async function startRestore() {
  if (submitting.value || restoreRunning.value || restoreVerifying.value) return;
  const backupDir = await chooseDirectory("选择保险库备份目录");
  if (!backupDir) return;
  restoreError.value = "";
  restoreResult.value = null;
  clearRestorePassword();
  restoreBackupDir.value = backupDir;
  restoreOpen.value = true;
  await nextTick();
  restorePasswordInput.value?.focus();
}

function handleRestoreKeydown(event) {
  if (event.key === "Escape") {
    event.preventDefault();
    closeRestoreDialog();
    return;
  }
  if (event.key === "Enter" && !restoreVerifying.value) {
    event.preventDefault();
    confirmRestore();
  }
}

/** 验证备份自身的主密码；通过后再选择恢复位置并执行恢复。 */
async function confirmRestore() {
  if (restoreVerifying.value || !restoreOpen.value) return;
  if (!restorePassword.value) {
    restoreDialogError.value = "请输入创建该备份的保险库的主密码。";
    return;
  }

  const backupDir = restoreBackupDir.value;
  const backupPassword = restorePassword.value;
  restoreVerifying.value = true;
  restoreDialogError.value = "";
  try {
    await verifyBackupPassword(backupDir, backupPassword);
  } catch (err) {
    const normalized = normalizeError(err);
    restoreVerifying.value = false;
    // 密码错误可重试；清空输入避免旧密码滞留。
    clearRestorePassword();
    restoreDialogError.value = normalized.message || "主密码验证未通过。";
    await nextTick();
    restorePasswordInput.value?.focus();
    return;
  }

  // 验证通过：关闭并清空密码状态，再选择目标目录。
  restoreVerifying.value = false;
  restoreOpen.value = false;
  restoreBackupDir.value = "";
  clearRestorePassword();

  const targetDir = await chooseDirectory("选择恢复位置（将在其中创建「我的保险库」）");
  if (!targetDir) return;

  restoreState.value = { status: "running", processed: 0, total: 0, bytes: 0 };
  try {
    const summary = await restoreVault(backupDir, targetDir, backupPassword);
    restoreState.value = { status: "idle", processed: 0, total: 0, bytes: 0 };
    restoreResult.value = {
      destination: summary.destination,
      fileCount: summary.fileCount,
      totalBytes: summary.totalBytes,
    };
    restoreError.value = "";
  } catch (err) {
    restoreState.value = { status: "idle", processed: 0, total: 0, bytes: 0 };
    const normalized = normalizeError(err);
    restoreError.value = normalized.message || "恢复失败，未生成完整保险库。";
  }
}

/** 复用现有打开流程进入恢复后的保险库，随后输入其主密码解锁。 */
async function openRestoredVault() {
  if (!restoreResult.value || openingRestored.value) return;
  openingRestored.value = true;
  try {
    const next = await openVault(restoreResult.value.destination);
    restoreResult.value = null;
    emit("opened", next);
  } catch (err) {
    restoreError.value = normalizeError(err).message;
  } finally {
    openingRestored.value = false;
  }
}

async function submit() {
  if (!canSubmit.value) return;
  submitting.value = true;
  error.value = "";
  slow.value = false;
  slowTimer = window.setTimeout(() => {
    slow.value = true;
  }, 2000);

  try {
    const status = await unlockVault(password.value);
    password.value = "";
    emit("unlocked", status);
  } catch (err) {
    const normalized = normalizeError(err);
    password.value = "";
    if (UNAVAILABLE_CODES.has(normalized.code)) {
      emit("unavailable", normalized.message);
    } else {
      error.value = normalized.message;
    }
    await nextTick();
    passwordInput.value?.focus();
  } finally {
    submitting.value = false;
    slow.value = false;
    if (slowTimer !== null) {
      window.clearTimeout(slowTimer);
      slowTimer = null;
    }
  }
}

onMounted(async () => {
  unlistenRestore = await onRestoreProgress(handleRestoreProgress);
});

onBeforeUnmount(() => {
  if (slowTimer !== null) window.clearTimeout(slowTimer);
  if (typeof unlistenRestore === "function") unlistenRestore();
  unlistenRestore = null;
  // 卸载时清空密码与恢复状态；后端任务由现有退出/取消机制清理。
  password.value = "";
  restoreOpen.value = false;
  restoreBackupDir.value = "";
  clearRestorePassword();
  restoreState.value = { status: "idle", processed: 0, total: 0, bytes: 0 };
  restoreResult.value = null;
});
</script>

<template>
  <section class="haven-card" aria-labelledby="unlock-title">
    <p class="haven-eyebrow">已锁定</p>
    <h1 id="unlock-title" class="haven-section-title">{{ props.vaultName }}</h1>
    <p class="haven-lead">输入主密码，打开你的私人抽屉。</p>
    <p v-if="props.vaultPath" class="haven-path">{{ props.vaultPath }}</p>

    <div class="haven-field">
      <span class="haven-label">主密码</span>
      <n-input
        ref="passwordInput"
        v-model:value="password"
        type="password"
        show-password-on="click"
        size="large"
        placeholder="输入主密码"
        :disabled="submitting"
        :input-props="{ autocomplete: 'current-password', spellcheck: 'false' }"
        @keyup.enter="submit"
      />
    </div>

    <div v-if="error" class="haven-error" role="alert">{{ error }}</div>

    <div
      v-if="restoreState.status === 'running'"
      class="haven-import-progress"
      role="status"
      aria-live="polite"
    >
      <div>
        <p class="haven-import-progress__title">正在恢复保险库…</p>
        <p class="haven-import-progress__meta">
          已复制 {{ restoreState.processed }} / {{ restoreState.total }} 个加密文件（{{
            formatFileSize(restoreState.bytes)
          }}）
        </p>
      </div>
    </div>

    <div v-if="restoreError" class="haven-error" role="alert">{{ restoreError }}</div>

    <div v-if="restoreResult" class="haven-import-result">
      <p class="haven-import-result__head">
        恢复完成：{{ restoreResult.fileCount }} 个加密文件，共
        {{ formatFileSize(restoreResult.totalBytes) }}
      </p>
      <p class="haven-import-progress__meta haven-backup__path">
        恢复位置：{{ restoreResult.destination }}
      </p>
      <p class="haven-import-progress__meta">
        原备份未改变。进入恢复后的保险库后，请输入该备份所属保险库的主密码解锁。
      </p>
      <div class="haven-actions__row" style="justify-content: flex-start; margin-top: 10px">
        <n-button
          type="primary"
          size="small"
          :loading="openingRestored"
          :disabled="openingRestored"
          @click="openRestoredVault"
        >
          打开恢复后的保险库
        </n-button>
        <n-button size="small" :disabled="openingRestored" @click="restoreResult = null">
          稍后处理
        </n-button>
      </div>
    </div>

    <div class="haven-actions">
      <div class="haven-actions__row">
        <n-button
          quaternary
          size="large"
          :disabled="submitting || restoreRunning"
          @click="emit('relocate')"
        >
          打开其他保险库
        </n-button>
        <n-button
          quaternary
          size="large"
          :disabled="submitting || restoreRunning || restoreVerifying"
          @click="startRestore"
        >
          从备份恢复
        </n-button>
        <n-button
          type="primary"
          size="large"
          :loading="submitting"
          :disabled="!canSubmit || restoreRunning"
          @click="submit"
        >
          {{ slow ? "正在解锁…" : "解锁" }}
        </n-button>
      </div>
      <p class="haven-hint">Haven 不保存主密码，也无法找回或重置。</p>
    </div>

    <Teleport to="body">
      <div
        v-if="restoreOpen"
        class="haven-delete"
        role="dialog"
        aria-modal="true"
        aria-labelledby="haven-restore-title"
        :aria-busy="restoreVerifying"
        @click.self="closeRestoreDialog"
      >
        <div class="haven-delete__panel">
          <p class="haven-eyebrow">从备份恢复</p>
          <h2 id="haven-restore-title" class="haven-delete__title">
            输入该备份的主密码
          </h2>
          <n-input
            ref="restorePasswordInput"
            class="haven-rename__field"
            type="password"
            :value="restorePassword"
            :disabled="restoreVerifying"
            show-password-on="mousedown"
            placeholder="创建该备份的保险库的主密码"
            :input-props="{
              'aria-label': '备份主密码',
              autocomplete: 'off',
              spellcheck: 'false',
              onKeydown: handleRestoreKeydown,
            }"
            @update:value="restorePassword = $event"
          />
          <p class="haven-rename__hint">
            验证只读取备份中的加密索引，不会解密任何文件；通过后再选择恢复位置。
          </p>
          <p v-if="restoreDialogError" class="haven-rename__error" role="alert">
            {{ restoreDialogError }}
          </p>
          <p v-if="restoreVerifying" class="haven-delete__status" role="status">
            正在验证主密码…请稍候。
          </p>
          <div class="haven-delete__actions">
            <n-button quaternary size="small" @click="emit('relocate')">打开其他保险库</n-button>
            <div class="haven-delete__confirm-actions">
              <n-button size="small" :disabled="restoreVerifying" @click="closeRestoreDialog">
                取消
              </n-button>
              <n-button
                type="primary"
                size="small"
                :loading="restoreVerifying"
                :disabled="restoreVerifying"
                @click="confirmRestore"
              >
                验证密码
              </n-button>
            </div>
          </div>
        </div>
      </div>
    </Teleport>
  </section>
</template>
