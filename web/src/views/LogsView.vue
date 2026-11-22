<script setup lang="ts">
import { onMounted, onUnmounted, ref } from "vue";
import { api } from "../lib/api";
import type { LogLine } from "../lib/types";

defineProps<{ status: unknown }>();

const logs = ref<LogLine[]>([]);
const paused = ref(false);
const follow = ref(true);
const box = ref<HTMLElement | null>(null);

async function load() {
  if (paused.value) return;
  try {
    logs.value = await api.logs(600);
    if (follow.value && box.value) {
      requestAnimationFrame(() => {
        box.value?.scrollTo({ top: box.value.scrollHeight });
      });
    }
  } catch {
    /* 下一轮再试 */
  }
}

let timer: number | undefined;
onMounted(() => {
  void load();
  timer = window.setInterval(load, 2000);
});
onUnmounted(() => window.clearInterval(timer));

function tone(line: LogLine): string {
  const t = line.text;
  if (line.level === "ERROR" || t.includes("ERROR") || t.includes("失败"))
    return "text-rec";
  if (line.level === "WARN" || t.includes("WARN")) return "text-[#a35a00]";
  return "text-[#41505a]";
}
</script>

<template>
  <div class="space-y-3">
    <div class="flex flex-wrap items-center gap-2">
      <label class="flex items-center gap-2 text-[12.5px] text-muted">
        <input v-model="paused" type="checkbox" class="h-3.5 w-3.5 accent-[#c07d12]" />
        暂停刷新
      </label>
      <label class="flex items-center gap-2 text-[12.5px] text-muted">
        <input v-model="follow" type="checkbox" class="h-3.5 w-3.5 accent-[#c07d12]" />
        自动滚动
      </label>
      <span class="readout ml-1 text-[11.5px] text-faint">{{ logs.length }} 行</span>
      <button
        class="ml-auto h-8 border border-line bg-panel px-3 text-[12px] hover:border-muted"
        @click="logs = []"
      >
        清空显示
      </button>
    </div>

    <div
      ref="box"
      class="h-[calc(100vh-260px)] min-h-[360px] overflow-y-auto border border-line bg-[#f7f8f7] px-4 py-3"
    >
      <p v-if="logs.length === 0" class="text-[12.5px] text-faint">
        暂无输出。录制进程启动后，请求日志会实时出现在这里。
      </p>
      <pre
        v-for="(l, i) in logs"
        :key="i"
        class="readout text-[11px] leading-[1.75] whitespace-pre-wrap break-all"
        :class="tone(l)"
        ><span class="text-[#9aa8af]">{{ l.at }}</span>  {{ l.text }}</pre
      >
    </div>
  </div>
</template>