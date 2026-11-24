<script setup>
import { computed } from "vue";
import { NButton } from "naive-ui";
import { fileTypeLabel, formatFileSize, formatFileTime } from "../lib/fileFormat.js";

const props = defineProps({
  file: { type: Object, required: true },
});
defineEmits(["close"]);

const typeLabel = computed(() => fileTypeLabel(props.file.name));
const sizeLabel = computed(() => formatFileSize(props.file.size));
const timeLabel = computed(() => formatFileTime(props.file.importedAt));
</script>

<template>
  <Teleport to="body">
    <div
      class="haven-details"
      role="dialog"
      aria-modal="true"
      aria-labelledby="haven-details-title"
      @click.self="$emit('close')"
    >
      <div class="haven-details__panel">
        <div class="haven-details__head">
          <p class="haven-eyebrow">文件详情</p>
          <n-button quaternary size="small" @click="$emit('close')">关闭</n-button>
        </div>
        <h2 id="haven-details-title" class="haven-details__name" :title="file.name">
          {{ file.name }}
        </h2>
        <dl class="haven-details__list">
          <div class="haven-details__row">
            <dt>文件类型</dt>
            <dd>{{ typeLabel }}</dd>
          </div>
          <div class="haven-details__row">
            <dt>文件大小</dt>
            <dd>{{ sizeLabel }}</dd>
          </div>
          <div class="haven-details__row">
            <dt>导入时间</dt>
            <dd>{{ timeLabel }}</dd>
          </div>
        </dl>
        <p class="haven-details__note">以上信息来自保险库索引，不会解密文件内容。</p>
      </div>
    </div>
  </Teleport>
</template>
