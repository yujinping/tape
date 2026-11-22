export interface TransparentView {
  enabled: boolean;
  ports: number[];
}

export interface Status {
  running: "record" | "replay" | null;
  since: string | null;
  uptime_secs: number | null;
  port: number | null;
  record_port: number;
  dir: string;
  version: string;
  counts: { snapshots: number; origins: number; bytes: number };
  local_ips: string[];
  transparent: TransparentView | null;
  config_path: string | null;
  last_error: string | null;
}

export interface SnapshotSummary {
  id: string;
  method: string;
  url: string;
  host: string;
  path: string;
  status: number;
  duration_ms: number;
  recorded_at: string;
  req_bytes: number;
  resp_bytes: number;
  content_type: string;
}

export interface Header {
  name: string;
  value: string;
}

export interface SnapshotDetail extends SnapshotSummary {
  origin: string;
  request: {
    method: string;
    url: string;
    headers: [string, string][];
    body: string;
    body_encoding: string;
  };
  response: {
    status: number;
    headers: [string, string][];
    body: string;
    body_encoding: string;
  };
}

export interface LogLine {
  at: string;
  level: string;
  text: string;
}

export interface ConfigPayload {
  record?: {
    include_hosts?: string[];
    include_hosts_regex?: string[];
  } | null;
  replay?: {
    bind?: string | null;
    port?: number | null;
    rewrite?: string | null;
    absolute_base?: string | null;
  } | null;
  transparent?: {
    enabled?: boolean;
    allow_ports?: string[];
    allow_hosts?: string[];
  } | null;
}
export interface RecordStartOptions {
  port: number;
  transparent?: {
    enabled: boolean;
    allow_ports: string[];
    allow_hosts: string[];
  };
}

export interface ReplayStartOptions {
  port: number;
  rewrite?: string;
  absolute_base?: string;
}
