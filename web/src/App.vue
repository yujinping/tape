<script setup lang="ts">
import { computed, onMounted, onUnmounted, ref } from "vue";
import { api, formatUptime } from "./lib/api";
import type { Status } from "./lib/types";
import StatusView from "./views/StatusView.vue";
import RecordsView from "./views/RecordsView.vue";
import SettingsView from "./views/SettingsView.vue";
import LogsView from "./views/LogsView.vue";
import HelpView from "./views/HelpView.vue";

const routes = [
  { key: "status", label: "运行", hint: "选模式、启动与停止" },
  { key: "records", label: "录制", hint: "已录制的请求与响应" },
  { key: "settings", label: "设置", hint: "按主机 / 路径过滤录制范围" },
  { key: "logs", label: "日志", hint: "录制进程的实时输出" },
  { key: "help", label: "帮助", hint: "盒子怎么接进来" },
];

const views: Record<string, unknown> = {
  status: StatusView,
  records: RecordsView,
  settings: SettingsView,
  logs: LogsView,
  help: HelpView,
};

function currentKey(): string {
  const hash = location.hash.replace(/^#\/?/, "");
  return hash.split("?")[0] || "status";
}

const route = ref(currentKey());
const status = ref<Status | null>(null);
const offline = ref(false);

const view = computed(() => views[route.value] ?? StatusView);
const meta = computed(
  () => routes.find((r) => r.key === route.value) ?? routes[0],
);

let timer: number | undefined;

async function refresh() {
  try {
    status.value = await api.status();
    offline.value = false;
  } catch {
    offline.value = true;
  }
}

function onHash() {
  route.value = currentKey();
  window.scrollTo({ top: 0 });
}

onMounted(() => {
  void refresh();
  timer = window.setInterval(refresh, 3000);
  window.addEventListener("hashchange", onHash);
});

onUnmounted(() => {
  window.clearInterval(timer);
  window.removeEventListener("hashchange", onHash);
});

const running = computed(() => status.value?.running ?? null);
const stateLabel = computed(() =>
  running.value === "record"
    ? "录制中"
    : running.value === "replay"
      ? "回放中"
      : "空闲",
);
const stateColor = computed(() =>
  running.value === "record"
    ? "var(--color-rec)"
    : running.value === "replay"
      ? "var(--color-ok)"
      : "#6d7d86",
);
</script>

<template>
  <div class="min-h-screen md:flex">
    <!-- 左栏：仪表机架 -->
    <aside
      class="sticky top-0 z-20 hidden h-screen w-[228px] shrink-0 flex-col bg-rail md:flex"
    >
      <div class="px-6 pt-7 pb-6">
        <div class="flex items-baseline gap-2">
          <span class="text-[27px] font-semibold tracking-[-0.05em] text-white"
            >tape</span
          >
          <span class="h-[7px] w-[7px] translate-y-[-2px] bg-amber"></span>
        </div>
        <p class="mt-1 text-[11.5px] tracking-[0.08em] text-[#7e8f9a]">
          盒子调试台
        </p>
      </div>

      <nav class="flex flex-col">
        <a
          v-for="r in routes"
          :key="r.key"
          :href="`#/${r.key}`"
          class="group flex items-center gap-3 border-l-2 py-[9px] pr-4 pl-[22px] text-[13.5px] transition-colors"
          :class="
            route === r.key
              ? 'border-amber bg-railsoft text-white'
              : 'border-transparent text-[#93a4ae] hover:text-[#d6e0e5]'
          "
        >
          <span
            class="h-[5px] w-[5px] shrink-0"
            :class="route === r.key ? 'bg-amber' : 'bg-[#42525d]'"
          ></span>
          {{ r.label }}
        </a>
      </nav>

      <div class="mt-auto border-t border-[#2a3944] px-6 py-5">
        <div class="flex items-center gap-2">
          <span
            class="h-[6px] w-[6px] rounded-full"
            :style="{ background: stateColor }"
          ></span>
          <span class="text-[12px] text-[#b9c7cf]">{{ stateLabel }}</span>
          <span v-if="status?.port" class="readout text-[11px] text-[#6f818c]"
            >:{{ status.port }}</span
          >
        </div>
        <p class="readout mt-2 text-[10.5px] text-[#5d707b]">
          v{{ status?.version ?? "0.1.3" }} · 127.0.0.1
        </p>
      </div>
    </aside>

    <!-- 移动端顶栏 -->
    <div class="sticky top-0 z-20 bg-rail md:hidden">
      <div class="flex items-center justify-between px-4 pt-3 pb-2">
        <div class="flex items-baseline gap-2">
          <span class="text-[20px] font-semibold tracking-[-0.04em] text-white"
            >tape</span
          >
          <span class="h-[6px] w-[6px] bg-amber"></span>
          <span class="text-[11px] text-[#7e8f9a]">盒子调试台</span>
        </div>
        <span class="flex items-center gap-1.5 text-[11.5px] text-[#b9c7cf]">
          <span
            class="h-[6px] w-[6px] rounded-full"
            :style="{ background: stateColor }"
          ></span>
          {{ stateLabel }}
        </span>
      </div>
      <div class="flex gap-1 overflow-x-auto px-3 pb-2">
        <a
          v-for="r in routes"
          :key="r.key"
          :href="`#/${r.key}`"
          class="shrink-0 px-2.5 py-1 text-[12.5px]"
          :class="
            route === r.key
              ? 'bg-railsoft text-white'
              : 'text-[#93a4ae]'
          "
          >{{ r.label }}</a
        >
      </div>
    </div>

    <div class="min-w-0 flex-1">
      <header
        class="flex flex-wrap items-end justify-between gap-3 border-b border-line px-5 py-5 md:px-8 md:py-6"
      >
        <div>
          <h1 class="text-[22px] leading-none font-semibold tracking-[-0.02em]">
            {{ meta.label }}
          </h1>
          <p class="mt-2 text-[12.5px] text-muted">{{ meta.hint }}</p>
        </div>
        <div class="flex items-center gap-4 text-[12px] text-muted">
          <span v-if="offline" class="text-rec">控制台连接中断</span>
          <span
            v-else-if="status?.uptime_secs != null"
            class="readout hidden sm:inline"
          >
            已运行 {{ formatUptime(status.uptime_secs) }}
          </span>
          <span
            v-if="status"
            class="readout hidden text-[11.5px] text-faint lg:inline"
            :title="status.dir"
            >{{ status.dir }}</span
          >
        </div>
      </header>

      <main class="px-5 pt-6 pb-16 md:px-8">
        <div class="mx-auto max-w-[1180px]">
          <component :is="view" :status="status" @refresh="refresh" />
        </div>
      </main>
    </div>
  </div>
</template>