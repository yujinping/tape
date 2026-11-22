<script setup lang="ts">
import { computed, onMounted, onUnmounted, ref, watch } from "vue";
import { api, copyText, formatBytes, formatClock, formatDuration } from "../lib/api";
import type { SnapshotDetail, SnapshotSummary } from "../lib/types";

defineProps<{ status: unknown }>();

const all = ref<SnapshotSummary[]>([]);
const loading = ref(false);
const error = ref("");
const query = ref("");
const method = ref("");
const kind = ref("");
const page = ref(0);
const pageSize = 50;

const detail = ref<SnapshotDetail | null>(null);
const detailLoading = ref(false);
const detailError = ref("");
const tab = ref<"request" | "response">("response");
const copied = ref("");

async function load() {
  loading.value = true;
  error.value = "";
  try {
    all.value = await api.snapshots();
  } catch (e) {
    error.value = e instanceof Error ? e.message : String(e);
  } finally {
    loading.value = false;
  }
}

const filtered = computed(() => {
  const q = query.value.trim().toLowerCase();
  return all.value.filter((s) => {
    if (method.value && s.method !== method.value) return false;
    if (kind.value === "ok" && (s.status < 200 || s.status >= 300)) return false;
    if (kind.value === "redirect" && (s.status < 300 || s.status >= 400)) return false;
    if (kind.value === "client" && (s.status < 400 || s.status >= 500)) return false;
    if (kind.value === "server" && s.status < 500) return false;
    if (!q) return true;
    return (
      s.url.toLowerCase().includes(q) ||
      s.host.toLowerCase().includes(q) ||
      s.id.includes(q)
    );
  });
});

const pages = computed(() => Math.max(1, Math.ceil(filtered.value.length / pageSize)));
const rows = computed(() => {
  const start = page.value * pageSize;
  return filtered.value.slice().reverse().slice(start, start + pageSize);
});
const methods = computed(() => Array.from(new Set(all.value.map((s) => s.method))).sort());

watch([query, method, kind], () => (page.value = 0));

function statusTone(status: number): string {
  if (status >= 500) return "text-rec";
  if (status >= 400) return "text-[#a35a00]";
  if (status >= 300) return "text-[#5d6b74]";
  return "text-ok";
}

async function open(id: string) {
  detailLoading.value = true;
  detailError.value = "";
  detail.value = null;
  tab.value = "response";
  try {
    detail.value = await api.snapshot(id);
  } catch (e) {
    detailError.value = e instanceof Error ? e.message : String(e);
  } finally {
    detailLoading.value = false;
  }
}

function close() {
  detail.value = null;
  detailError.value = "";
}

function onKey(e: KeyboardEvent) {
  if (e.key === "Escape") close();
}

onMounted(() => {
  void load();
  window.addEventListener("keydown", onKey);
});
onUnmounted(() => window.removeEventListener("keydown", onKey));

const prettyBody = computed(() => {
  const d = detail.value;
  if (!d) return "";
  const side = tab.value === "request" ? d.request : d.response;
  if (side.body_encoding !== "utf8") return "";
  const text = side.body;
  try {
    return JSON.stringify(JSON.parse(text), null, 2);
  } catch {
    return text;
  }
});

const isImage = computed(() =>
  (detail.value?.content_type ?? "").startsWith("image/"),
);

const bodySize = computed(() => {
  const d = detail.value;
  if (!d) return 0;
  return tab.value === "request" ? d.req_bytes : d.resp_bytes;
});

const headers = computed<[string, string][]>(() => {
  const d = detail.value;
  if (!d) return [];
  return tab.value === "request" ? d.request.headers : d.response.headers;
});

const bodyUrl = computed(() =>
  detail.value
    ? `/api/snapshots/${encodeURIComponent(detail.value.id)}/body?side=${tab.value}`
    : "",
);

async function copy(text: string, tag: string) {
  if (await copyText(text)) {
    copied.value = tag;
    window.setTimeout(() => (copied.value = ""), 1500);
  }
}
</script>

<template>
  <div class="space-y-4">
    <div class="flex flex-wrap items-center gap-2">
      <input
        v-model="query"
        type="search"
        placeholder="搜索 URL、主机或编号"
        class="h-9 w-[150px] border border-line bg-panel px-3 text-[13px] outline-none focus:border-muted sm:w-[260px]"
      />
      <select
        v-model="method"
        class="h-9 border border-line bg-panel px-2 text-[13px] outline-none focus:border-muted"
      >
        <option value="">全部方法</option>
        <option v-for="m in methods" :key="m" :value="m">{{ m }}</option>
      </select>
      <select
        v-model="kind"
        class="h-9 border border-line bg-panel px-2 text-[13px] outline-none focus:border-muted"
      >
        <option value="">全部状态</option>
        <option value="ok">2xx</option>
        <option value="redirect">3xx</option>
        <option value="client">4xx</option>
        <option value="server">5xx</option>
      </select>
      <div class="flex w-full items-center gap-3 sm:ml-auto sm:w-auto">
        <span class="readout whitespace-nowrap text-[12px] text-faint">{{ filtered.length }} 条</span>
        <button
          class="ml-auto h-9 border border-line bg-panel px-3 text-[12.5px] hover:border-muted disabled:opacity-50 sm:ml-0"
          :disabled="loading"
          @click="load"
        >
          {{ loading ? "读取中…" : "刷新" }}
        </button>
      </div>
    </div>

    <p v-if="error" class="border-l-2 border-rec pl-3 text-[12.5px] text-rec">{{ error }}</p>

    <div class="hidden border border-line bg-panel md:block">
      <table class="w-full border-collapse text-[12.5px]">
        <thead>
          <tr class="border-b border-line text-left text-[11.5px] text-muted">
            <th class="px-4 py-2.5 font-normal">编号</th>
            <th class="px-2 py-2.5 font-normal">时间</th>
            <th class="px-2 py-2.5 font-normal">方法</th>
            <th class="px-2 py-2.5 font-normal">URL</th>
            <th class="hidden px-2 py-2.5 font-normal md:table-cell">主机</th>
            <th class="px-2 py-2.5 text-right font-normal">状态</th>
            <th class="hidden px-2 py-2.5 text-right font-normal sm:table-cell">耗时</th>
            <th class="hidden px-4 py-2.5 text-right font-normal md:table-cell">大小</th>
          </tr>
        </thead>
        <tbody>
          <tr
            v-for="s in rows"
            :key="s.id"
            class="cursor-pointer border-b border-line/70 last:border-0 hover:bg-[#f4f6f6]"
            @click="open(s.id)"
          >
            <td class="readout px-4 py-2.5 text-[11px] text-faint">{{ s.id }}</td>
            <td class="readout px-2 py-2.5 text-[11.5px] text-muted">{{ formatClock(s.recorded_at) }}</td>
            <td class="readout px-2 py-2.5 font-medium">{{ s.method }}</td>
            <td class="px-2 py-2.5">
              <span class="block truncate" :title="s.url">{{ s.path }}</span>
            </td>
            <td class="readout hidden truncate px-2 py-2.5 text-[11.5px] text-muted md:table-cell">{{ s.host }}</td>
            <td class="readout px-2 py-2.5 text-right" :class="statusTone(s.status)">{{ s.status }}</td>
            <td class="readout hidden px-2 py-2.5 text-right text-muted sm:table-cell">{{ formatDuration(s.duration_ms) }}</td>
            <td class="readout hidden px-4 py-2.5 text-right text-muted md:table-cell">{{ formatBytes(s.resp_bytes) }}</td>
          </tr>
          <tr v-if="rows.length === 0">
            <td colspan="8" class="px-4 py-12 text-center text-[13px] text-faint">
              没有匹配的请求
            </td>
          </tr>
        </tbody>
      </table>
    </div>

    <div class="divide-y divide-line border border-line bg-panel md:hidden">
      <button
        v-for="s in rows"
        :key="s.id"
        class="block w-full px-4 py-3 text-left active:bg-[#f4f6f6]"
        @click="open(s.id)"
      >
        <div class="flex items-center gap-2">
          <span class="readout text-[12px] font-medium">{{ s.method }}</span>
          <span class="min-w-0 flex-1 truncate text-[12.5px]">{{ s.path }}</span>
          <span class="readout text-[12px]" :class="statusTone(s.status)">{{ s.status }}</span>
        </div>
        <div class="mt-1 flex items-center gap-2 text-[11px] text-faint">
          <span class="readout">{{ formatClock(s.recorded_at) }}</span>
          <span class="readout min-w-0 flex-1 truncate">{{ s.host }}</span>
          <span class="readout">{{ formatDuration(s.duration_ms) }}</span>
        </div>
      </button>
      <p v-if="rows.length === 0" class="px-4 py-12 text-center text-[13px] text-faint">
        没有匹配的请求
      </p>
    </div>

    <div v-if="pages > 1" class="flex items-center justify-end gap-3 text-[12.5px]">
      <button
        class="border border-line bg-panel px-3 py-1 disabled:opacity-40"
        :disabled="page === 0"
        @click="page--"
      >
        上一页
      </button>
      <span class="readout text-faint">{{ page + 1 }} / {{ pages }}</span>
      <button
        class="border border-line bg-panel px-3 py-1 disabled:opacity-40"
        :disabled="page >= pages - 1"
        @click="page++"
      >
        下一页
      </button>
    </div>

    <!-- 详情抽屉 -->
    <div
      v-if="detail || detailLoading || detailError"
      class="fixed inset-0 z-40 flex justify-end bg-ink/30"
      @click.self="close"
    >
      <aside class="flex h-full w-full max-w-[680px] flex-col bg-panel shadow-[-8px_0_24px_rgba(16,24,32,0.12)]">
        <div class="flex items-center gap-3 border-b border-line px-5 py-3">
          <span v-if="detail" class="readout text-[12px] text-faint">{{ detail.id }}</span>
          <span v-if="detail" class="readout text-[13px] font-medium">{{ detail.request.method }}</span>
          <span v-if="detail" class="min-w-0 flex-1 truncate text-[12.5px] text-muted" :title="detail.url">
            {{ detail.request.url }}
          </span>
          <button class="text-[13px] text-muted hover:text-ink" @click="close">关闭</button>
        </div>

        <div v-if="detailLoading" class="p-6 text-[13px] text-faint">读取中…</div>
        <p v-else-if="detailError" class="p-6 text-[13px] text-rec">{{ detailError }}</p>

        <template v-else-if="detail">
          <div class="flex items-center gap-4 border-b border-line px-5 py-2 text-[12.5px]">
            <button
              class="pb-1"
              :class="tab === 'response' ? 'border-b-2 border-amber font-medium' : 'text-muted'"
              @click="tab = 'response'"
            >
              响应 {{ detail.response.status }}
            </button>
            <button
              class="pb-1"
              :class="tab === 'request' ? 'border-b-2 border-amber font-medium' : 'text-muted'"
              @click="tab = 'request'"
            >
              请求
            </button>
            <span class="readout ml-auto text-[11.5px] text-faint">
              {{ formatDuration(detail.duration_ms) }} · {{ formatBytes(bodySize) }}
            </span>
          </div>

          <div class="min-h-0 flex-1 overflow-y-auto">
            <section class="border-b border-line px-5 py-4">
              <h3 class="text-[12px] text-muted">头部</h3>
              <table class="mt-2 w-full text-[12px]">
                <tbody>
                  <tr v-for="[k, v] in headers" :key="k + v" class="align-top">
                    <td class="readout w-[150px] py-0.5 pr-3 text-[11px] text-faint">{{ k }}</td>
                    <td class="py-0.5 break-all text-ink">{{ v }}</td>
                  </tr>
                </tbody>
              </table>
            </section>

            <section class="px-5 py-4">
              <div class="flex items-center justify-between">
                <h3 class="text-[12px] text-muted">正文</h3>
                <button
                  class="text-[11.5px] text-amber hover:underline"
                  @click="copy(tab === 'request' ? detail.request.body : detail.response.body, 'body')"
                >
                  {{ copied === "body" ? "已复制" : "复制原文" }}
                </button>
              </div>

              <img
                v-if="isImage && tab === 'response'"
                :src="bodyUrl"
                alt="响应图片"
                class="mt-3 max-h-[420px] w-auto border border-line bg-[#f4f6f6] object-contain"
              />
              <pre
                v-else-if="prettyBody"
                class="mt-2 max-h-[520px] overflow-auto border border-line bg-[#f6f7f6] p-3 text-[11.5px] leading-[1.65] whitespace-pre-wrap break-all"
                >{{ prettyBody }}</pre
              >
              <p v-else class="mt-3 text-[12.5px] text-faint">
                二进制内容（{{ formatBytes(bodySize) }}），不在此处展开。
              </p>
            </section>
          </div>
        </template>
      </aside>
    </div>
  </div>
</template>