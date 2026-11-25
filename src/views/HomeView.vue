<script setup>
import { computed, onBeforeUnmount, onMounted, ref } from "vue";
import { NButton, useMessage } from "naive-ui";
import CreateVaultPanel from "../components/CreateVaultPanel.vue";
import UnlockPanel from "../components/UnlockPanel.vue";
import VaultPanel from "../components/VaultPanel.vue";
import {
  chooseDirectory,
  getVaultStatus,
  isTauri,
  lockVault,
  normalizeError,
  openVault,
} from "../lib/vaultClient.js";

const AUTO_LOCK_MS = 5 * 60 * 1000;
const IDLE_CHECK_MS = 15 * 1000;
const UNAVAILABLE_CODES = new Set([
  "VAULT_NOT_FOUND",
  "METADATA_MISSING",
  "METADATA_INVALID",
  "UNSUPPORTED_VERSION",
]);
const ACTIVITY_EVENTS = ["mousedown", "mousemove", "keydown", "wheel", "touchstart"];

const message = useMessage();

const view = ref("loading");
const status = ref(null);
const unavailableMessage = ref("");
const opening = ref(false);

const vaultName = computed(() => status.value?.vaultName || "我的保险库");
const vaultPath = computed(() => status.value?.vaultPath || "");

let idleTimer = null;
let lastActivity = Date.now();

function resetIdle() {
  lastActivity = Date.now();
}

function onActivity() {
  if (view.value === "unlocked") resetIdle();
}

function applyStatus(next) {
  status.value = next;
  if (!next) {
    view.value = "loading";
    return;
  }
  if (next.state === "unlocked") {
    view.value = "unlocked";
  } else if (next.state === "locked") {
    view.value = "locked";
  } else if (next.state === "unavailable") {
    unavailableMessage.value = next.message || "找不到你的保险库。";
    view.value = "unavailable";
  } else {
    view.value = "uninitialized";
  }
  if (next.message && next.state !== "unavailable") message.info(next.message);
  resetIdle();
}

function showUnavailable(text) {
  unavailableMessage.value = text || "找不到你的保险库。";
  view.value = "unavailable";
}

async function refreshStatus() {
  if (!isTauri()) {
    view.value = "unsupported";
    return;
  }
  try {
    applyStatus(await getVaultStatus());
  } catch {
    showUnavailable("无法读取保险库状态，请重新启动 Haven。");
  }
}

async function handleOpenExisting() {
  if (!isTauri() || opening.value) return;
  opening.value = true;
  try {
    const dir = await chooseDirectory("选择已有保险库目录（其中包含 vault.json）");
    if (!dir) return;
    const next = await openVault(dir);
    applyStatus(next);
    if (next.state === "locked") {
      message.success("已打开保险库，请输入主密码。");
    }
  } catch (err) {
    const normalized = normalizeError(err);
    if (UNAVAILABLE_CODES.has(normalized.code)) {
      showUnavailable(normalized.message);
    } else {
      message.error(normalized.message);
    }
  } finally {
    opening.value = false;
  }
}

function startCreate() {
  view.value = "creating";
}

function handleCreated(next) {
  applyStatus(next);
  message.success("保险库已创建并解锁。");
}

function handleCancelCreate() {
  refreshStatus();
}

async function handleLock() {
  try {
    applyStatus(await lockVault());
    message.info("保险库已锁定。");
  } catch (err) {
    message.error(normalizeError(err).message);
  }
}

/**
 * 后端因磁盘不可用已清空主密钥，这里只从后端读取真实状态并同步界面，
 * 不在前端伪造锁定。
 */
async function handleDisconnected() {
  try {
    applyStatus(await getVaultStatus());
  } catch {
    showUnavailable("无法读取保险库状态，请重新启动 Haven。");
  }
}

/** FR-47：锁定屏恢复完成后，复用现有打开流程切换到恢复后的保险库。 */
function handleRestoredOpened(next) {
  applyStatus(next);
  if (next?.state === "locked") {
    message.success("已打开恢复后的保险库，请输入该备份所属保险库的主密码。");
  }
}

async function checkIdle() {
  if (view.value !== "unlocked") return;
  if (Date.now() - lastActivity < AUTO_LOCK_MS) return;
  try {
    applyStatus(await lockVault());
    message.info("长时间未操作，保险库已自动锁定。");
  } catch {
    // 自动锁定失败时保留当前界面，用户仍可手动锁定。
  }
}

onMounted(async () => {
  ACTIVITY_EVENTS.forEach((name) =>
    window.addEventListener(name, onActivity, { passive: true })
  );
  idleTimer = window.setInterval(checkIdle, IDLE_CHECK_MS);
  await refreshStatus();
});

onBeforeUnmount(() => {
  ACTIVITY_EVENTS.forEach((name) => window.removeEventListener(name, onActivity));
  if (idleTimer !== null) window.clearInterval(idleTimer);
});
</script>

<template>
  <main class="haven-screen">
    <div class="haven-screen__content">
    <section v-if="view === 'loading'" class="haven-card">
      <p class="haven-quiet">正在检查保险库…</p>
    </section>

    <section v-else-if="view === 'unsupported'" class="haven-card" aria-labelledby="browser-title">
      <div class="haven-mark" aria-hidden="true">
        <svg viewBox="0 0 48 48" fill="none">
          <path
            d="M12 41V22a12 12 0 0 1 24 0v19"
            stroke="currentColor"
            stroke-width="1.8"
            stroke-linecap="round"
          />
          <path
            d="M24 41V26"
            stroke="currentColor"
            stroke-width="1.8"
            stroke-linecap="round"
            opacity="0.38"
          />
        </svg>
      </div>
      <h1 id="browser-title" class="haven-title">Haven</h1>
      <p class="haven-quiet" style="margin-top: 18px">
        当前是浏览器预览，无法访问本地保险库。
        <br />
        请通过桌面应用运行 Haven 后再创建或解锁保险库。
      </p>
    </section>

    <CreateVaultPanel
      v-else-if="view === 'creating'"
      @created="handleCreated"
      @cancel="handleCancelCreate"
    />

    <UnlockPanel
      v-else-if="view === 'locked'"
      :vault-name="vaultName"
      :vault-path="vaultPath"
      @unlocked="applyStatus"
      @unavailable="showUnavailable"
      @relocate="handleOpenExisting"
      @opened="handleRestoredOpened"
    />

    <VaultPanel
      v-else-if="view === 'unlocked'"
      :vault-name="vaultName"
      :vault-path="vaultPath"
      @lock="handleLock"
      @disconnected="handleDisconnected"
    />

    <section
      v-else-if="view === 'unavailable'"
      class="haven-card"
      aria-labelledby="unavailable-title"
    >
      <p class="haven-eyebrow">保险库不可用</p>
      <h1 id="unavailable-title" class="haven-section-title">找不到你的保险库</h1>
      <p class="haven-lead">{{ unavailableMessage }}</p>
      <p v-if="vaultPath" class="haven-path">{{ vaultPath }}</p>
      <p class="haven-help" style="margin-top: 14px">
        如果保险库位于外置磁盘，请先连接磁盘再重试。Haven 不会自动创建新的保险库替代原位置；
        如果原数据已丢失，可以创建一个新的保险库。
      </p>
      <div class="haven-actions">
        <div class="haven-actions__row">
          <n-button type="primary" size="large" @click="startCreate">创建新保险库</n-button>
          <n-button quaternary size="large" :loading="opening" @click="handleOpenExisting">
            重新选择位置
          </n-button>
          <n-button size="large" @click="refreshStatus">重试</n-button>
        </div>
      </div>
    </section>

    <section v-else class="haven-card" aria-labelledby="haven-title">
      <div class="haven-mark" aria-hidden="true">
        <svg viewBox="0 0 48 48" fill="none">
          <path
            d="M12 41V22a12 12 0 0 1 24 0v19"
            stroke="currentColor"
            stroke-width="1.8"
            stroke-linecap="round"
          />
          <path
            d="M24 41V26"
            stroke="currentColor"
            stroke-width="1.8"
            stroke-linecap="round"
            opacity="0.38"
          />
        </svg>
      </div>

      <h1 id="haven-title" class="haven-title">Haven</h1>
      <p class="haven-tagline">你的文件，只属于你。</p>
      <p class="haven-intro">
        把不想被随手看到的照片和文件，收进一个只有你能打开的地方。
        <br />
        数据保存在本机，不上传，不同步，无需联网。
      </p>

      <div class="haven-actions">
        <div class="haven-actions__row">
          <n-button type="primary" size="large" @click="startCreate">创建保险库</n-button>
          <n-button size="large" quaternary :loading="opening" @click="handleOpenExisting">
            打开已有保险库
          </n-button>
        </div>
        <p class="haven-hint">忘记主密码将无法恢复，请把密码记在心里或安全的地方。</p>
      </div>
    </section>

    <footer v-if="view !== 'unlocked'" class="haven-footer">
      <span>v0.1.2</span>
      <span class="haven-dot" aria-hidden="true">·</span>
      <span>数据保存在本机</span>
      <span class="haven-dot" aria-hidden="true">·</span>
      <span>无需联网</span>
    </footer>
    </div>
  </main>
</template>
