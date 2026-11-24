<script setup>
import { NConfigProvider, NMessageProvider, zhCN, dateZhCN } from "naive-ui";
import HomeView from "./views/HomeView.vue";
import { themeOverrides } from "./theme.js";
</script>

<template>
  <n-config-provider
    :theme-overrides="themeOverrides"
    :locale="zhCN"
    :date-locale="dateZhCN"
  >
    <n-message-provider>
      <HomeView />
    </n-message-provider>
  </n-config-provider>
</template>
