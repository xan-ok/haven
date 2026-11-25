<script setup>
import { NConfigProvider, NMessageProvider, darkTheme, dateZhCN, zhCN } from "naive-ui";
import HomeView from "./views/HomeView.vue";
import { initTheme, isDarkTheme, naiveOverrides } from "./theme/manager.js";

initTheme();
</script>

<template>
  <n-config-provider
    :theme="isDarkTheme ? darkTheme : null"
    :theme-overrides="naiveOverrides"
    :locale="zhCN"
    :date-locale="dateZhCN"
  >
    <n-message-provider>
      <HomeView />
    </n-message-provider>
  </n-config-provider>
</template>
