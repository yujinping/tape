async function request<T>(path: string, init?: RequestInit): Promise<T> {
  const res = await fetch(path, {
    ...init,
    headers: { "Content-Type": "application/json", ...(init?.headers ?? {}) },
  });
  const text = await res.text();
  const data = text ? JSON.parse(text) : null;
  if (!res.ok) {
    const message = data?.error ?? `HTTP ${res.status}`;
    throw new Error(message);
  }
  return data as T;
}

export const api = {
  status: () => request<import("./types").Status>("/api/status"),
  snapshots: () =>
    request<import("./types").SnapshotSummary[]>("/api/snapshots"),
  snapshot: (id: string) =>
    request<import("./types").SnapshotDetail>(
      `/api/snapshots/${encodeURIComponent(id)}`,
    ),
  logs: (limit = 400) =>
    request<import("./types").LogLine[]>(`/api/logs?limit=${limit}`),
  config: () => request<import("./types").ConfigPayload>("/api/config"),
  saveConfig: (payload: import("./types").ConfigPayload) =>
    request<{ ok: boolean }>("/api/config", {
      method: "PUT",
      body: JSON.stringify(payload),
    }),
  startRecord: (opts: import("./types").RecordStartOptions) =>
    request<{ ok: boolean }>("/api/record/start", {
      method: "POST",
      body: JSON.stringify(opts),
    }),
  stopRecord: () =>
    request<{ ok: boolean }>("/api/record/stop", { method: "POST" }),
  startReplay: (opts: import("./types").ReplayStartOptions) =>
    request<{ ok: boolean }>("/api/replay/start", {
      method: "POST",
      body: JSON.stringify(opts),
    }),
  stopReplay: () =>
    request<{ ok: boolean }>("/api/replay/stop", { method: "POST" }),
};

export function formatBytes(n: number): string {
  if (n < 1024) return `${n} B`;
  if (n < 1024 * 1024) return `${(n / 1024).toFixed(1)} KB`;
  if (n < 1024 * 1024 * 1024) return `${(n / 1024 / 1024).toFixed(1)} MB`;
  return `${(n / 1024 / 1024 / 1024).toFixed(2)} GB`;
}

export function formatDuration(ms: number): string {
  if (ms < 1) return "<1 ms";
  if (ms < 1000) return `${ms} ms`;
  if (ms < 60_000) return `${(ms / 1000).toFixed(2)} s`;
  return `${Math.floor(ms / 60_000)}m ${Math.round((ms % 60_000) / 1000)}s`;
}

export function formatUptime(secs: number | null): string {
  if (secs == null) return "";
  const h = Math.floor(secs / 3600);
  const m = Math.floor((secs % 3600) / 60);
  const s = secs % 60;
  if (h > 0) return `${h} 小时 ${m} 分`;
  if (m > 0) return `${m} 分 ${s} 秒`;
  return `${s} 秒`;
}

export function shortPath(url: string): string {
  try {
    const u = new URL(url);
    return u.pathname + u.search;
  } catch {
    return url;
  }
}

export function clock(iso: string): string {
  const d = new Date(iso);
  if (Number.isNaN(d.getTime())) return iso;
  const pad = (n: number) => String(n).padStart(2, "0");
  return `${pad(d.getHours())}:${pad(d.getMinutes())}:${pad(d.getSeconds())}`;
}

export async function copyText(text: string): Promise<boolean> {
  try {
    await navigator.clipboard.writeText(text);
    return true;
  } catch {
    const el = document.createElement("textarea");
    el.value = text;
    document.body.appendChild(el);
    el.select();
    const ok = document.execCommand("copy");
    el.remove();
    return ok;
  }
}
export function formatClock(iso: string): string {
  const d = new Date(iso);
  if (Number.isNaN(d.getTime())) return "—";
  return d.toLocaleTimeString("zh-CN", { hour12: false });
}
