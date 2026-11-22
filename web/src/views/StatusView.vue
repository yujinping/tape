<script setup lang="ts">
import { computed, onMounted, onUnmounted, ref, watch } from "vue";
import { api, formatBytes, formatDuration, formatUptime } from "../lib/api";
import type { SnapshotSummary, Status } from "../lib/types";

const props = defineProps<{ status: Status | null }>();
const emit = defineEmits<{ refresh: [] }>();

const snapshots = ref<SnapshotSummary[]>([]);
const busy = ref(false);
const error = ref("");

async function loadSnapshots() {
  try {
    snapshots.value = await api.snapshots();
  } catch {
    /* 列表失败不影响状态页 */
  }
}

async function act(fn: () => Promise<unknown>) {
  busy.value = true;
  error.value = "";
  try {
    await fn();
    emit("refresh");
    await loadSnapshots();
  } catch (e) {
    error.value = e instanceof Error ? e.message : String(e);
  } finally {
    busy.value = false;
  }
}

const running = computed(() => props.status?.running ?? null);

type Mode = "record" | "replay";
const mode = ref<Mode>("record");
const port = ref(8888);
const transparent = ref(false);
const allowPorts = ref("80");
const allowHosts = ref("");
const rewrite = ref("relative");
const absoluteBase = ref("");
const prefsKey = "tape.run.prefs";
const portTouched = ref(false);

// 状态是异步到的：运行中一律显示真实端口，空闲时用上次记住的端口。
watch(
  () => props.status,
  (s) => {
    if (!s) return;
    if (s.running && s.port) {
      port.value = s.port;
      if (s.running === "record" && s.transparent) {
        transparent.value = s.transparent.enabled;
      }
      return;
    }
    if (!localStorage.getItem(prefsKey) && !portTouched.value) {
      port.value = s.record_port ?? 8888;
    }
  },
  { immediate: true },
);

const modeLabel = computed(() => (mode.value === "record" ? "录制" : "回放"));
const visibleMode = computed<Mode>(() =>
  running.value === "replay" ? "replay" : running.value === "record" ? "record" : mode.value,
);

function splitList(text: string): string[] {
  return text
    .split(/[,，\s]+/)
    .map((s) => s.trim())
    .filter(Boolean);
}

function savePrefs() {
  localStorage.setItem(
    prefsKey,
    JSON.stringify({
      mode: mode.value,
      port: port.value,
      transparent: transparent.value,
      allowPorts: allowPorts.value,
      allowHosts: allowHosts.value,
      rewrite: rewrite.value,
      absoluteBase: absoluteBase.value,
    }),
  );
}

async function initPrefs() {
  const raw = localStorage.getItem(prefsKey);
  if (raw) {
    try {
      const p = JSON.parse(raw);
      if (p.mode === "record" || p.mode === "replay") mode.value = p.mode;
      if (typeof p.port === "number") port.value = p.port;
      transparent.value = !!p.transparent;
      allowPorts.value = p.allowPorts ?? "80";
      allowHosts.value = p.allowHosts ?? "";
      rewrite.value = p.rewrite ?? "relative";
      absoluteBase.value = p.absoluteBase ?? "";
      return;
    } catch {
      /* 坏数据就当没有 */
    }
  }
  port.value = props.status?.record_port ?? 8888;
  try {
    const cfg = await api.config();
    if (cfg.transparent) {
      transparent.value = cfg.transparent.enabled;
      allowPorts.value = (cfg.transparent.allow_ports ?? []).join(", ");
      allowHosts.value = (cfg.transparent.allow_hosts ?? []).join(", ");
    }
    rewrite.value = cfg.replay?.rewrite ?? "relative";
    absoluteBase.value = cfg.replay?.absolute_base ?? "";
  } catch {
    /* 配置文件还没生成，用默认值 */
  }
}

function start() {
  if (
    mode.value === "record" &&
    transparent.value &&
    splitList(allowPorts.value).length === 0
  ) {
    error.value = "透明模式需要至少一个端口，例如 80 或 8080-8090";
    return;
  }
  savePrefs();
  if (mode.value === "record") {
    void act(() =>
      api.startRecord({
        port: port.value,
        transparent: {
          enabled: transparent.value,
          allow_ports: splitList(allowPorts.value),
          allow_hosts: splitList(allowHosts.value),
        },
      }),
    );
  } else {
    void act(() =>
      api.startReplay({
        port: port.value,
        rewrite: rewrite.value,
        absolute_base:
          rewrite.value === "absolute" ? absoluteBase.value.trim() : undefined,
      }),
    );
  }
}

const stop = () =>
  act(() => (running.value === "record" ? api.stopRecord() : api.stopReplay()));

// 盒子要填的 IP：优先非回环地址，没有就退回 127.0.0.1。
const primaryIp = computed(() => {
  const ips = props.status?.local_ips ?? [];
  return ips.find((ip) => !ip.startsWith("127.") && ip !== "::1") ?? "127.0.0.1";
});

/** 最近 30 分钟，每分钟一根柱。 */
const bars = computed(() => {
  const now = Date.now();
  const bucket = new Map<number, number>();
  for (const s of snapshots.value) {
    const t = new Date(s.recorded_at).getTime();
    if (Number.isNaN(t)) continue;
    const minute = Math.floor((now - t) / 60_000);
    if (minute < 0 || minute >= 30) continue;
    bucket.set(minute, (bucket.get(minute) ?? 0) + 1);
  }
  const values = Array.from({ length: 30 }, (_, i) => bucket.get(29 - i) ?? 0);
  const max = Math.max(1, ...values);
  return values.map((v, i) => ({
    value: v,
    height: v === 0 ? 2 : Math.max(6, Math.round((v / max) * 100)),
    live: i === 29,
  }));
});

const totals = computed(() => {
  const today = new Date();
  today.setHours(0, 0, 0, 0);
  let todayCount = 0;
  let bytes = 0;
  for (const s of snapshots.value) {
    bytes += s.req_bytes + s.resp_bytes;
    if (new Date(s.recorded_at).getTime() >= today.getTime()) todayCount += 1;
  }
  return { todayCount, bytes };
});

const slowest = computed(() => {
  const list = snapshots.value.slice().sort((a, b) => b.duration_ms - a.duration_ms);
  return list[0];
});

const last = computed(() => snapshots.value[snapshots.value.length - 1]);

let timer: number | undefined;
onMounted(() => {
  void loadSnapshots();
  void initPrefs();
  timer = window.setInterval(loadSnapshots, 5000);
});
onUnmounted(() => window.clearInterval(timer));
</script>

<template>
  <div class="space-y-6">
    <!-- 控制台：选模式、设选项、启动；接入地址收在同一块里 -->
    <section class="border border-line bg-panel">
      <div class="flex flex-wrap items-center justify-between gap-x-6 gap-y-3 px-6 py-4">
        <div class="flex flex-wrap items-center gap-x-4 gap-y-2">
          <div class="inline-flex border border-line">
            <button
              v-for="m in (['record', 'replay'] as const)"
              :key="m"
              class="px-4 py-1.5 text-[13px] font-medium transition-colors"
              :class="
                visibleMode === m
                  ? 'bg-ink text-white'
                  : 'text-muted hover:bg-canvas disabled:opacity-40'
              "
              :disabled="!!running || busy"
              @click="mode = m"
            >
              {{ m === "record" ? "录制" : "回放" }}
            </button>
          </div>
          <span class="text-[11.5px] text-faint">
            {{ visibleMode === "record" ? "抓盒子请求，落成快照" : "用快照离线返回，盒子不用联网" }}
          </span>
        </div>

        <div class="flex items-center gap-4">
          <span class="flex items-center gap-1.5 text-[12px]">
            <span
              class="h-[6px] w-[6px] rounded-full"
              :class="running ? (running === 'record' ? 'bg-rec' : 'bg-ok') : 'bg-faint'"
            ></span>
            <template v-if="running">
              {{ running === "record" ? "录制中" : "回放中" }} ·
              {{ formatUptime(status?.uptime_secs ?? null) }}
            </template>
            <template v-else>未运行</template>
          </span>
          <button
            class="min-w-[112px] px-5 py-2 text-[13px] font-medium text-white transition-colors disabled:opacity-50"
            :class="running ? 'bg-rec hover:bg-[#a02c23]' : 'bg-ok hover:bg-[#186a42]'"
            :disabled="busy"
            @click="running ? stop() : start()"
          >
            {{ running ? "停止" + (running === "record" ? "录制" : "回放") : "启动" + modeLabel }}
          </button>
        </div>
      </div>

      <div class="grid gap-x-10 gap-y-6 border-t border-line px-6 py-5 lg:grid-cols-2">
        <!-- 接入：盒子要填的两个值 -->
        <div class="space-y-3">
          <label class="block text-[12px] text-muted">
            IP 地址
            <input
              :value="primaryIp"
              readonly
              tabindex="-1"
              class="readout mt-1 block h-9 w-full border border-line bg-canvas px-2 text-[13px] text-ink outline-none"
            />
          </label>
          <label class="block text-[12px] text-muted">
            端口
            <input
              v-model.number="port"
              type="number"
              min="1"
              max="65535"
              :disabled="!!running"
              class="readout mt-1 block h-9 w-full border border-line bg-panel px-2 text-[13px] outline-none focus:border-muted disabled:bg-canvas disabled:text-faint"
              @input="portTouched = true"
            />
          </label>
          <p class="text-[11.5px] leading-5 text-faint">
            盒子与本机需在同一局域网，IP 会按当前网络自动读取；换网络后刷新页面即可更新。
          </p>
        </div>

        <!-- 模式选项 -->
        <div v-if="visibleMode === 'record'" class="space-y-3">
          <div class="block text-[12px] text-muted">
            透明模式
            <div
              class="mt-1 flex h-9 items-center gap-2.5 border border-line bg-panel px-2.5"
              :class="running ? '' : 'cursor-pointer hover:border-muted'"
              @click="running || (transparent = !transparent)"
            >
              <button
                type="button"
                role="switch"
                :aria-checked="transparent"
                :disabled="!!running"
                class="relative h-5 w-9 shrink-0 rounded-full border transition-colors"
                :class="transparent ? 'border-ink bg-ink' : 'border-line bg-canvas'"
              >
                <span
                  class="absolute top-[2px] h-[14px] w-[14px] rounded-full bg-panel transition-all"
                  :class="transparent ? 'left-[18px]' : 'left-[2px]'"
                ></span>
              </button>
              <span class="text-[12px]">{{ transparent ? "开" : "关" }}</span>
              <span class="ml-auto text-[10.5px] text-faint">{{
                transparent ? "盒子不用改 App" : "盒子用代理接入"
              }}</span>
            </div>
          </div>

          <template v-if="transparent">
            <label class="block text-[12px] text-muted">
              目标端口白名单
              <input
                v-model="allowPorts"
                :disabled="!!running"
                placeholder="80, 8080, 8080-8090"
                class="readout mt-1 block h-9 w-full border border-line bg-panel px-2 text-[13px] outline-none focus:border-muted disabled:bg-canvas"
              />
            </label>
            <label class="block text-[12px] text-muted">
              目标主机（可选）
              <input
                v-model="allowHosts"
                :disabled="!!running"
                placeholder="*.company.com, 10.1.2.3"
                class="readout mt-1 block h-9 w-full border border-line bg-panel px-2 text-[13px] outline-none focus:border-muted disabled:bg-canvas"
              />
            </label>
          </template>
          <p v-else class="text-[11.5px] leading-5 text-faint">
            盒子能改 App 时，把左边的 IP 与端口填进「使用 TAPE 代理」；不能改包时打开透明模式，把盒子网关指向本机。
          </p>
        </div>

        <div v-else class="space-y-3">
          <label class="block text-[12px] text-muted">
            响应改写
            <select
              v-model="rewrite"
              :disabled="!!running"
              class="mt-1 block h-9 w-full border border-line bg-panel px-2 text-[13px] outline-none focus:border-muted disabled:bg-canvas"
            >
              <option value="relative">相对路径（推荐）</option>
              <option value="absolute">绝对地址</option>
              <option value="none">不改写</option>
            </select>
          </label>
          <label v-if="rewrite === 'absolute'" class="block text-[12px] text-muted">
            绝对基地址
            <input
              v-model="absoluteBase"
              :disabled="!!running"
              placeholder="http://127.0.0.1:8888/"
              class="readout mt-1 block h-9 w-full border border-line bg-panel px-2 text-[13px] outline-none focus:border-muted disabled:bg-canvas"
            />
          </label>
          <p v-else class="text-[11.5px] leading-5 text-faint">
            让盒子请求左边的地址，tape 会按 URL 从快照返回；relative 会把页面里的链接改回 tape 地址。
          </p>
        </div>
      </div>

      <div
        v-if="error || status?.last_error"
        class="border-t border-line bg-rec/5 px-6 py-3 text-[12px] leading-5 text-rec"
      >
        <p v-if="error">{{ error }}</p>
        <p v-if="status?.last_error">{{ status.last_error }}</p>
      </div>

    </section>

    <!-- 顶部读数 + 信号条 -->
    <section class="grid gap-6 lg:grid-cols-[1.25fr_1fr]">
      <div class="border border-line bg-panel px-6 pt-6 pb-5">
        <p class="text-[12.5px] text-muted">今日录制</p>
        <div class="mt-1 flex items-baseline gap-3">
          <span class="readout text-[54px] leading-none font-medium">{{
            totals.todayCount
          }}</span>
          <span class="text-[13px] text-muted">条请求</span>
        </div>

        <div class="mt-6 flex h-[92px] items-end gap-[3px]" aria-hidden="true">
          <div
            v-for="(b, i) in bars"
            :key="i"
            class="flex-1 transition-[height] duration-500"
            :class="b.live ? 'bg-amber' : 'bg-amber/45'"
            :style="{ height: `${b.height}%` }"
            :title="`${b.value} 条`"
          ></div>
        </div>
        <div class="mt-2 flex justify-between text-[10.5px] text-faint readout">
          <span>30 分钟前</span>
          <span>现在</span>
        </div>

        <div class="mt-5 grid grid-cols-3 gap-3 border-t border-line pt-4">
          <div>
            <p class="text-[11.5px] text-muted">快照总数</p>
            <p class="readout mt-0.5 text-[17px]">
              {{ status?.counts.snapshots ?? snapshots.length }}
            </p>
          </div>
          <div>
            <p class="text-[11.5px] text-muted">站点</p>
            <p class="readout mt-0.5 text-[17px]">
              {{ status?.counts.origins ?? "—" }}
            </p>
          </div>
          <div>
            <p class="text-[11.5px] text-muted">占用</p>
            <p class="readout mt-0.5 text-[17px]">
              {{ formatBytes(status?.counts.bytes ?? totals.bytes) }}
            </p>
          </div>
        </div>
      </div>

      <div class="border border-line bg-panel px-6 pt-6 pb-5">
        <div class="flex items-center justify-between">
          <p class="text-[12.5px] text-muted">运行状态</p>
          <span class="flex items-center gap-1.5 text-[12px]">
            <span
              class="h-[6px] w-[6px] rounded-full"
              :class="running === 'record' ? 'bg-rec' : running === 'replay' ? 'bg-ok' : 'bg-faint'"
            ></span>
            {{ running === "record" ? "录制中" : running === "replay" ? "回放中" : "未运行" }}
          </span>
        </div>

        <dl class="mt-4 space-y-2.5 text-[12.5px]">
          <div class="flex justify-between gap-4">
            <dt class="text-muted">监听端口</dt>
            <dd class="readout">{{ status?.port ?? "—" }}</dd>
          </div>
          <div v-if="running === 'record'" class="flex justify-between gap-4">
            <dt class="text-muted">透明模式</dt>
            <dd>
              <template v-if="status?.transparent?.enabled">
                开 · {{ status.transparent.ports.slice(0, 6).join(", ") }}{{
                  status.transparent.ports.length > 6 ? " …" : ""
                }}
              </template>
              <template v-else>关</template>
            </dd>
          </div>
          <div v-if="status?.uptime_secs != null" class="flex justify-between gap-4">
            <dt class="text-muted">已运行</dt>
            <dd class="readout">{{ formatUptime(status.uptime_secs) }}</dd>
          </div>
          <div class="flex justify-between gap-4">
            <dt class="text-muted">最近一条</dt>
            <dd class="readout">
              {{ last ? new Date(last.recorded_at).toLocaleTimeString("zh-CN", { hour12: false }) : "—" }}
            </dd>
          </div>
          <div class="flex justify-between gap-4">
            <dt class="text-muted">最慢请求</dt>
            <dd class="readout">
              {{ slowest ? formatDuration(slowest.duration_ms) : "—" }}
            </dd>
          </div>
        </dl>

        <p class="mt-5 text-[11.5px] leading-relaxed text-faint">
          录制与回放共用 {{ status?.record_port ?? 8888 }} 端口，同一时间只能运行一个。
          停止后可以修改上面的选项，下次启动生效。
        </p>
      </div>
    </section>

    <!-- 最近录制 -->
    <section class="border border-line bg-panel">
      <div class="flex items-center justify-between border-b border-line px-6 py-3.5">
        <h2 class="text-[13.5px] font-medium">最近录制</h2>
        <a href="#/records" class="text-[12px] text-amber hover:underline">查看全部</a>
      </div>
      <div v-if="snapshots.length === 0" class="px-6 py-10 text-[13px] text-faint">
        还没有录制到请求。启动录制后，在盒子上操作一次即可。
      </div>
      <ul v-else class="divide-y divide-line">
        <li
          v-for="s in snapshots.slice(-6).reverse()"
          :key="s.id"
          class="flex items-center gap-3 px-6 py-2.5 text-[12.5px]"
        >
          <span class="readout w-[52px] shrink-0 text-[11px] text-faint">{{ s.id }}</span>
          <span class="readout w-[46px] shrink-0 font-medium">{{ s.method }}</span>
          <span class="min-w-0 flex-1 truncate text-muted" :title="s.url">{{ s.path }}</span>
          <span
            class="readout w-[38px] shrink-0 text-right"
            :class="s.status >= 400 ? 'text-rec' : 'text-ok'"
            >{{ s.status }}</span
          >
          <span class="readout w-[64px] shrink-0 text-right text-faint">{{
            formatDuration(s.duration_ms)
          }}</span>
        </li>
      </ul>
    </section>
  </div>
</template>
