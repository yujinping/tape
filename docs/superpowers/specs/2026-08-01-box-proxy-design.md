# tape 设计文档（2026-08-01）

> 依据 PRD《专网HTTP接口录制与离线重放代理工具需求文档》设计。

## 1. 目标与范围

一个纯 HTTP 的正向代理录制 + 离线重放工具，单二进制 `tape`，两个子命令：

- `tape record`：本地正向 HTTP 代理，录制 APP 全部 HTTP 流量到磁盘（原样保留，不做任何改写）。
- `tape replay`：本地离线重放服务，按 method+path 匹配快照，返回改写后的响应与本地静态资源；零外网、零专网。

明确不做：HTTPS/证书/CONNECT、HTTP/2（纯 HTTP 场景客户端均为 HTTP/1.1）、可视化界面。

## 2. 技术底座

- Rust 2024 edition，rustup stable（1.97.x）。
- tokio（异步 runtime）+ hyper（HTTP/1.1 server/client）。
- 放弃 PRD 建议的 wiretap-rs：已核实其为 WireGuard VPN 隧道代理，与 HTTP 代理录制需求不符，crates.io 无实际使用者。从零实现更符合"纯 HTTP、轻量化"约束。
- 依赖：tokio、hyper、hyper-util、http-body-util、bytes、clap、serde/serde_json、regex、sha2、base64、mime_guess、tracing/tracing-subscriber、anyhow。

## 3. CLI 与配置

```
tape record [-p/--port 8888] [-d/--dir ./tape-api] [--rewrite-on-record] [-v]
tape replay [-p/--port 8080] [-d/--dir ./tape-api]
                 [--rewrite relative|absolute|none] [--absolute-base http://127.0.0.1:8080/] [-v]
```

- `--dir` 两个命令均支持，默认 `./tape-api`（相对当前工作目录）。
- 目录支持软链接：代码只按路径读写（`std::fs` 默认跟随软链接），用户可通过把 `tape-api` 软链到不同已录制目录来切换数据源。
- `record` 在目录不存在时创建；`replay` 目录不存在时直接报错退出。
- `-v` 可重复，控制 tracing 日志级别。

## 4. 磁盘布局

```
tape-api/
├── session.json                  # 会话元数据：录制时间、工具版本、origin 列表、快照数
├── snapshots/
│   └── <origin_host_port>/       # origin 目录名：host_port（冒号替换为下划线）
│       └── <seq>-<METHOD>-<sha256(path+query)>.json   # 单接口快照
└── resources/
    ├── index.json                # 资源索引：[{hash, origin, path, content_type, size}]
    ├── blobs/<sha256>            # 去重 blob（内容哈希）
    └── <origin_host_port>/<相对路径>  # 硬链接副本，保留原始目录结构，便于人工查看
```

- 快照 JSON 记录原始 request/response 全量（method、url、headers、body、status、duration）。
- body 编码：UTF-8 可解码存 `body_encoding: "utf8"`，否则 base64。
- 资源去重：内容 sha256 相同只存一份 blob，路径处建硬链接（同目录内跨设备问题不存在）。

## 5. 快照模型（snapshot.rs）

```rust
pub struct Snapshot {
    pub id: String,               // 6 位序号
    pub origin: String,           // http://host:port
    pub recorded_at: String,      // RFC3339
    pub duration_ms: u64,
    pub request: RequestRecord,
    pub response: ResponseRecord,
}
pub struct RequestRecord {
    pub method: String,
    pub url: String,              // 原始绝对 URL
    pub headers: Vec<(String, String)>,
    pub body: String,
    pub body_encoding: String,    // "utf8" | "base64"
}
pub struct ResponseRecord {
    pub status: u16,
    pub headers: Vec<(String, String)>,
    pub body: String,
    pub body_encoding: String,
}
```

## 6. 录制代理（record.rs）

- hyper server 监听端口；客户端请求行为 absolute-form（`GET http://host:port/path HTTP/1.1`）。
- 解析目标：仅接受 `http` scheme；`https` 或其他返回 501/400 说明性错误（PRD：无 HTTPS）。
- 通过 hyper-util client 以 origin-form 转发到目标 host:port，保留 method/headers/body，覆盖 Host 头为目标 authority。
- 响应整体缓冲后：原样保存快照 → 原样回传（保真优先，PRD 约束"100% 保留原始数据"）。
- `--rewrite-on-record`：若开启，回传前按相对模式改写响应体（仅影响回传，快照仍存原始数据）。默认关闭（避免破坏公司网络下 APP 的实时会话）。
- 转发超时 30s；每个请求独立记录 seq（原子计数器），日志输出 `seq method origin path status duration`。
- 请求体也整体缓冲并记录（GET 多为空）。

## 7. 改写引擎（rewrite.rs）

- 统一基于正则的原始文本改写（不做 JSON 解析重序列化），保证响应体格式逐字节不变，满足 PRD"JSON 数据结构、字段内容、格式完全不变"。
- 正则：匹配 `http(s)://host(:port)?(/path?query)?`，任意 host。
- 三种模式：
  - `relative`（默认）：`http://10.1.2.3:8080/api/x?a=1` → `/api/x?a=1`（保留 path+query，丢弃 fragment）。
  - `absolute`：替换为 `--absolute-base` 拼接 path+query。
  - `none`：不改写。
- 幂等保护：host 为 localhost/127.0.0.1/[::1] 或等于 `--absolute-base` 的 host 时不改写。
- 提供 `extract_http_urls(text)` 供资源下载模块提取链接（JSON 字符串、HTML src/href、CSS url() 统一由正则覆盖）。

## 8. 资源下载（download.rs）

- 对录制响应中文本类 body（json/html/css/js 等）调用 `extract_http_urls` 收集候选 URL。
- 资源判定：路径扩展名白名单（png/jpg/jpeg/gif/webp/svg/css/js/woff/woff2/ttf/eot/ico/mp4/mp3 等），或按响应 Content-Type 判定（image/*、text/css、application/javascript、font/*、audio/*、video/*）。
- 跳过 text/html 与 application/json（避免把接口/页面当资源）。
- 逐 URL 下载（信号量限并发 8，超时 15s），sha256 去重：blob 已存在则跳过；否则写 blob + 到 `resources/<origin>/<path>` 建硬链接（已存在则跳过），并写 index.json。
- 失败仅告警，不影响录制主流程。

## 9. 重放服务（replay.rs）

- 启动时扫描快照目录构建内存索引：`(origin, method, path, query)` → snapshot 列表。
- 匹配顺序：
  1. 精确：请求 Host header 对应 origin + method + path + query；
  2. 兜底：忽略 origin，method + path + query（覆盖 APP 改 IP 后 Host 变成本地地址的场景）。
  3. 有歧义时取最新录制（seq 最大）。
- 命中后返回原状态码 + 改写后的响应体（按启动参数决定改写模式），Content-Type 取快照响应头，否则按路径 mime_guess 推断；剥离 hop-by-hop 头。
- 未命中：尝试按 path 查 resources 索引（index.json）提供静态资源，mime_guess 定 Content-Type。
- 仍未命中：404（零网络）。
- 静态资源索引在启动时从 index.json 加载（path → blob hash）。

## 9.5 录制过滤与资源落盘补充（2026-08-01 迭代）

- 录制过滤：`record --config <file>`（TOML 配置文件，`[record]` 表）。`include_hosts` 数组支持 `host`（任意端口）与 `host:port`（精确）；`include_hosts_regex` 数组为预编译正则，对完整 authority（host:port）做大小写不敏感匹配；两类规则取并集。代理始终转发全部流量，无配置时录制全部；有配置时仅匹配请求落快照/下载资源，其余仅转发。选 TOML 理由：Rust 生态一等支持（toml crate 由 Cargo 团队维护）、原生数组与字面字符串（正则免双重转义）、支持注释；对比 JSON 无注释且正则转义痛苦，serde_yaml 已废弃。
- 资源落盘完善：
  - APP 直接请求的静态资源（Content-Type 判定：image/*、text/css、javascript、font/*、audio/*、video/* 等）除快照外，同时入库 `resources/<origin>/<path>`；
  - 响应文本（JSON/HTML/CSS/JS）中的根相对路径（`/static/xxx`、`/img/xxx` 等）也会被提取并按 origin 拼接下载；
  - `ResourceStore.store` 对同一 origin+path 去重，避免 index.json 重复条目。
- 相对路径提取使用与 URL 提取一致的正则思路（`src/rewrite.rs::extract_relative_asset_paths`），不解析 HTML/JSON，保证响应格式零改动。

## 10. 错误处理与日志

- 库代码错误用 anyhow 传递，CLI 顶层统一打印 `error: ...` 并退出码 1。
- tracing 日志：record 记录每请求一行；replay 记录未命中 404；资源下载失败 warn。
- 快照写入失败视为致命（录制完整性要求）；资源下载失败不致命。

## 11. 测试策略

- 单元测试（模块内 `#[cfg(test)]`）：
  - rewrite：relative/absolute/none 三种模式、端口保留、query 保留、幂等、JSON/HTML/CSS/JS 样例。
  - extract：JSON 字符串、HTML src/href、CSS url() 提取。
  - store：origin 目录名规范化、seq 递增、快照读写 roundtrip（utf8 + base64）。
  - replay 匹配：精确 vs 兜底 vs 404、歧义取最新。
- 集成测试（`tests/`，仅本地端口，无外网）：
  - record：本地 mock origin + 经代理发 absolute-form 请求 → 快照落盘、回传一致。
  - replay：临时快照目录 + 启动重放 → 命中返回改写体、静态资源、404。

## 12. 交付与安装

- `cargo install --path .` 安装到全局 PATH（`~/.cargo/bin/tape`）。
- 文档：README.md 说明两种模式用法、目录结构与软链接切换方法。
