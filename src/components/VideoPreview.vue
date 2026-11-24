<script setup>
import { onBeforeUnmount, ref, watch } from "vue";

const props = defineProps({
  url: { type: String, required: true },
  name: { type: String, default: "视频" },
});

const player = ref(null);
const failed = ref(false);
const failureText = "该视频编码不受支持，或文件已损坏，暂时无法播放。";

watch(
  () => props.url,
  () => {
    failed.value = false;
  }
);

function handleError() {
  failed.value = true;
}

/** 卸载时停止播放并断开媒体源，Blob URL 由预览控制器负责撤销。 */
function teardown() {
  const element = player.value;
  if (!element) return;
  try {
    element.pause();
  } catch {
    // 播放器已释放时 pause 可能失败，忽略即可。
  }
  element.removeAttribute("src");
  element.load();
}

onBeforeUnmount(teardown);
</script>

<template>
  <div class="video-preview">
    <video
      v-if="!failed"
      ref="player"
      class="video-preview__player"
      :src="url"
      :aria-label="`播放 ${name}`"
      controls
      playsinline
      preload="metadata"
      @error="handleError"
    ></video>
    <p v-else class="video-preview__error" role="alert">{{ failureText }}</p>
  </div>
</template>
