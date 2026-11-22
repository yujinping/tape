<script setup lang="ts">
import { onMounted, ref } from "vue";
import { api } from "../lib/api";
import type { ConfigPayload, Status } from "../lib/types";

const props = defineProps<{ status: Status | null }>();
const emit = defineEmits<{ refresh: [] }>();

const loaded = ref(false);
const busy = ref(false);
const error = ref("");
const saved = ref(false);

const includeHosts = ref("");
const includeRegex = ref("");
// 页面只编辑录制范围；保存时把透明模式 / 回放段原样带回去，避免被覆盖。
const fullConfig = ref<ConfigPayload | null>(null);

function lines(text: string): string[] {
  return text
    .split(/\r?\n/)
    .map((s) => s.trim())
    .filter(Boolean);
}

async function load() {
  try {
    const cfg: ConfigPayload = await api.config();
    fullConfig.value = cfg;
    includeHosts.value = (cfg.record?.include_hosts ?? []).join("\n");
    includeRegex.value = (cfg.record?.include_hosts_regex ?? []).join("\n");
    loaded.value = true;
  } catch (e) {
    error.value = e instanceof Error ? e.message : String(e);
  }
}

async function save() {
  error.value = "";
  saved.value = false;
  busy.value = true;
  try {
    await api.saveConfig({
      ...(fullConfig.value ?? {}),
      record: {
        include_hosts: lines(includeHosts.value),
        include_hosts_regex: lines(includeRegex.value),
      },
    });
    saved.value = true;
    emit("refresh");
    window.setTimeout(() => (saved.value = false), 2400);
  } catch (e) {
    error.value = e instanceof Error ? e.message : String(e);
  } finally {
    busy.value = false;
  }
}

onMounted(load);

function fieldClass() {
  return "mt-1 w-full border border-line bg-panel px-3 py-2 text-[13px] outline-none focus:border-muted";
}
</script>

<template>
  <div class="max-w-[760px] space-y-6">
    <p class="text-[12.5px] leading-6 text-muted">
      端口、透明模式、回放的选项在「运行」页设置；这里只保留录制范围（按主机 / 路径过滤）。
      设置保存在数据目录的 <code class="readout text-[11.5px]">tape-config.toml</code>，
      修改后需要重新开始录制才会生效。
    </p>

    <p
      v-if="status?.running"
      class="border-l-2 border-amber bg-amber/5 px-4 py-3 text-[12.5px] text-[#7a5410]"
    >
      当前正在{{ status.running === "record" ? "录制" : "回放" }}，保存后请到「运行」页停止再启动。
    </p>

    <section class="border border-line bg-panel">
      <div class="border-b border-line px-6 py-3.5">
        <h2 class="text-[13.5px] font-medium">录制范围</h2>
        <p class="mt-1 text-[11.5px] text-faint">不匹配的请求照常转发，只是不落快照。</p>
      </div>
      <div class="grid gap-4 px-6 py-5 md:grid-cols-2">
        <label class="block">
          <span class="text-[12.5px] text-muted">按主机名</span>
          <textarea v-model="includeHosts" :class="fieldClass()" rows="4" placeholder="10.1.2.3:8080&#10;api.example.com"></textarea>
        </label>
        <label class="block">
          <span class="text-[12.5px] text-muted">按正则</span>
          <textarea v-model="includeRegex" :class="fieldClass()" rows="4" placeholder="/api/.*"></textarea>
        </label>
      </div>
    </section>

    <p v-if="error" class="border-l-2 border-rec pl-3 text-[12.5px] text-rec">{{ error }}</p>

    <div class="flex items-center gap-3">
      <button
        class="bg-ink px-5 py-2 text-[13px] font-medium text-white hover:bg-[#1d2a33] disabled:opacity-50"
        :disabled="busy || !loaded"
        @click="save"
      >
        {{ busy ? "保存中…" : "保存设置" }}
      </button>
      <span v-if="saved" class="text-[12.5px] text-ok">已保存</span>
    </div>
  </div>
</template>