# tape 代码审查报告（2026-08-02）

## 1. 审查范围与方法

- **项目**：`tape` v0.1.2（目录 `box-proxy`），纯 Rust 实现的电视盒子 / Android 开发调试工具箱（录制 / 重放 / logcat / console / app）。
- **规模**：`src/` 17 个模块约 4400 行，集成测试 `tests/proxy_flow.rs` 1313 行，合计约 5700 行；依赖 19 个 crate。
- **方法**：逐模块阅读源码（cli / config / record / replay / rewrite / store / download / net / http_util / snapshot / ingest / logcat / console / app / list / log_file / main），核对 README 声明的行为与实现的一致性，并用临时诊断测试验证疑点（已清理，工作区无残留改动）。

## 2. 验证结果（全部通过）

| 检查项 | 命令 | 结果 |
| --- | --- | --- |
| 单元测试 | `cargo test --all-targets` | 102 个全部通过（83 单测 + 19 集成），0 失败 |
| 文档测试 | `cargo test --doc` | 0 个（无 doc-test 示例），通过 |
| Clippy | `cargo clippy --all-targets -- -D warnings` | 无任何警告 |
| 格式 | `cargo fmt --check` | 通过 |
| 编译 | `cargo build --all-targets` | 无警告，无错误 |

结论先行：**整体工程质量高**——模块划分清晰、注释详实、错误处理以 `anyhow` 上抛 / 降级告警为主，测试覆盖在同体量工具中属于上乘。初版审查发现重放路径 **1 个必须修复的功能性 bug**（见 3.1）、3.2 的四个中优先级问题与 3.3 的十个低优先级问题，均已处理；当前 111 个测试（89 单测 + 22 集成）全部通过。

## 3. 问题清单

### 3.1 必须修复（高优先级）

#### 3.1.1 重放时带 query 的代理式 / 前缀式请求全部 404（**已修复**）

- **位置**：`src/replay.rs` `handle_request`（137 行、158 行）与 `match_snapshot`（62 行）的键构造不一致。
- **现象**：重放索引 `by_path` / `resources` 的键是**去掉 query** 的路径（索引侧由 `request_method_path` 剥离，39-40 行；资源键 `format!("/{}", e.path)` 在录制侧由 `url_path` 剥离），但查询侧把 `path_and_query`（**含 query**）直接当键用。结果：
  - absolute-form（标准代理）`GET http://host/api/user?id=1` → **404**；
  - 前缀式 `GET /http://host/api/user?id=1` → **404**；
  - 前缀式静态资源 `/img/a.png?v=2` → **404**（缓存破坏参数是最常见形态）。
  - 只有直接访问（`req.uri().path()` 天然不含 query）才正常。
- **与文档矛盾**：README 明确声明「重放按 method + path 匹配快照（**忽略 host / query**）」。
- **影响**：App 通过代理 / 前缀接入时的 URL 几乎必然带 query（时间戳、分页、会话参数），等于核心离线重放功能在带 query 时不可用。
- **验证**：临时集成测试实测 5 种形态，代理 / 前缀带 query 全部 404，直接访问 200；测试后已删除，工作区干净。
- **修复**：查询侧 `path` 统一剥离 query（与索引侧 `request_method_path` / `url_path` 保持一致），同时使 `mime_guess` 的 content-type 推断更准确；新增回归测试 `replay_matches_queries_in_proxy_and_prefix_forms` 覆盖代理式 / 前缀式带 query 的快照与资源命中。全量 103 个测试通过（83 单测 + 20 集成）。

### 3.2 建议修复（中优先级）

#### 3.2.1 资源索引按 path 建键，跨 origin 冲突且跨域资源归属错误（**已修复**）

- **位置**：`src/replay.rs` 35-38 行（`resources` map 键只有 `"/{path}"`）；`src/download.rs` `fetch_and_store` / `ResourceStore::store`。
- **问题**：
  1. 两个不同站点存在同路径资源（如各自 `/img/logo.png`）时，map 键冲突，后写入者覆盖先写入者，重放时可能串站。
  2. 页面 A 引用 CDN 资源时，索引里的 `origin` 存的是**页面** A 的 origin 而非资源自身 host（下载时以 `origin` 参数传入），无法区分资源真实来源。
- **建议**：资源匹配时把请求目标 host 纳入键（`(origin, path)`），下载时记录资源自身的 host。
- **修复**：重放侧资源索引改为 `(authority, path)` 精确匹配 + 按路径回退（直接访问兼容），同路径不同站点不再串站；下载侧新增 `url_origin`，跨域 CDN 资源以自身 origin 入库，页面引用不再错误归属。新增集成测试 `replay_resources_do_not_cross_origins` 与单测 `url_origin_extraction`。

#### 3.2.2 录制落盘每次全量重读快照，O(n²)（**已修复**）

- **位置**：`src/store.rs` `write_snapshot`（98 行）→ `save_session`（102 行）→ `load_snapshots`（109 行、136 行）。
- **问题**：每次写一条快照都会递归读取并解析 `snapshots/` 下**全部** JSON 来统计 `snapshot_count`。长会话录制到数千条后单次落盘耗时随总量线性增长，总体呈平方级。
- **建议**：用 `AtomicUsize` 增量计数（启动时一次性统计），session.json 落盘频率可降低（如每 N 条或结束时写一次）。
- **修复**：`Recorder` 新增 `snapshot_count: AtomicUsize`，启动时统计一次基数，之后 `write_snapshot` 内 `fetch_add` 增量维护，`save_session` 不再全量重读；每次落盘的成本从 O(n) 降为 O(1)。由 `recorder_resumes_seq_and_count_after_existing_snapshots` 测试覆盖。

#### 3.2.3 重复录制同一目录会静默覆盖旧快照（**已修复**）

- **位置**：`src/store.rs` `next_id`（78 行）+ `write_snapshot`（82-100 行）。
- **问题**：每次启动序号都从 `000001` 重新开始，文件名 `{id}-{method}-{urlhash}.json` 不含会话标识。同一目录第二次录制时，相同 method+URL 的旧快照被**静默覆盖**，上一会话数据丢失（无提示、无备份）。
- **建议**：文件名加入会话时间戳，或启动时检测已存在文件并提示换目录 / 归档。
- **修复**：`Recorder::new` 启动时扫描已有快照的最大 id，序号从 `max_id + 1` 续起（文件名 `{id}-...` 天然不冲突，磁盘布局不变），旧会话与新会话快照共存，replay 按既有「取最新 id」语义命中。由 `recorder_resumes_seq_and_count_after_existing_snapshots` 测试覆盖。

#### 3.2.4 `TAPE_INSECURE_TLS=false` 也会关闭证书校验（**已修复**）

- **位置**：`src/net.rs` `insecure_tls_enabled`（42-47 行）。
- **问题**：判断条件是「非 `0` 且非空」，因此 `false` / `no` / `off` 都会**启用**跳过校验，与常见布尔语义和文档（写的是 `=1`）不符，用户本想开启校验却关掉了。
- **建议**：显式白名单 `1`/`true`/`yes`/`on` 才算启用。
- **修复**：改为显式白名单（`1` / `true` / `yes` / `on`，大小写不敏感、容忍首尾空格），其余取值一律视为关闭。新增单测 `insecure_tls_whitelist_semantics` 覆盖 `false`/`0`/`off`/空值/未设置。

### 3.3 低优先级 / 仅供参考

1. **开放转发代理无访问控制**（**已处理**）：record / replay 新增 `--bind`（默认 `0.0.0.0` 不变），可 `--bind 127.0.0.1` 限制仅本机；replay 还支持配置文件 `[replay] bind`。README 补充无鉴权服务的网络隔离说明。
2. **请求 / 响应体全量进内存**（**已处理**）：README「已知限制」明确注明请求与响应体整体缓冲、无大小上限及大文件内存开销。
3. **replay 资源响应不校验 method**（**已处理**）：资源路径仅允许 GET / HEAD，其余返回 405 + `Allow: GET, HEAD`；新增集成测试。
4. **非 ASCII 请求头值被静默丢弃**（**已处理**）：`src/record.rs` 两处改用 `String::from_utf8_lossy(v.as_bytes())`，快照保留可见内容，不再置空。
5. **logcat 在 async 上下文同步执行 adb**（**已处理**）：`adb devices` 与 `logcat -c` 改走 `tokio::task::spawn_blocking`，不再阻塞 tokio 工作线程。
6. **落盘文件名精确到秒**（**已处理**）：`stamp()` 增加毫秒精度（`YYYYMMDD-HHMMSSmmm`），同秒多次启动不再互相截断；README / 注释 / 测试同步更新。
7. **`origin_dir_name` 不区分 scheme**（**已处理**）：目录名改为 `http_` / `https_` 前缀区分；`load_snapshots` 改为递归收集 JSON，兼容历史无前缀旧目录（有专门兼容测试）。
8. **`save_session` 失败与损坏快照静默**（**已处理**）：`session.json` 更新失败与快照 JSON 解析失败均改为 `warn` 告警。
9. **Relative 改写对无 path 有 query 的 URL 丢 query**（**已处理**）：URL / 协议相对正则的路径组支持 `?query` 形态，改写后保留 query（`http://host?x=1` → `/?x=1`），提取 URL 也更完整；新增单测。
10. **注释过期**（**已处理**）：`log_file.rs` 等处的 console / app「规划中」更新为已实现，时间戳格式说明同步。

## 4. 测试覆盖评估

**做得好的**：
- 改写逻辑覆盖全：relative / absolute / prefix 三种模式、协议相对链接、根相对 HTML 属性、CSS `url()`、gzip/deflate/br 压缩体往返、SVG 命名空间与 DOCTYPE 豁免、幂等性。
- 端到端覆盖了 record / replay 两条主链路：absolute-form、前缀式（含浏览器百分号编码、单斜杠折叠）、HTTPS 自签上游、Host 头覆写、Referer 注入、过滤规则、302 Location、资源落盘与重放、歧义快照取最新。
- 配置模块覆盖了 CLI 优先于配置文件、非法 TOML / 正则、默认路径解析。
- ingest / logcat / list / log_file 均有解析与格式单测。

**缺口**（本次 3.1 的 bug 正是缺口的直接后果）：
- **重放请求带 query**：现有集成测试的请求行全部无 query，未覆盖索引去 query 与查询侧不一致的路径。
- **`--rewrite-on-record` 端到端**：集成测试全部用 `RecordState::new(dir, false, ...)`，该开关只有纯函数单测，没有走 HTTP 链路。
- **多 origin 同路径资源**：资源索引冲突（3.2.1）无测试。
- **大 body / 慢上游 / 并发**：120s 超时、8 并发下载等没有压力型测试。
- **replay 的 HEAD / 非 GET 请求**、资源响应的 method 校验无测试。

## 5. 结论

代码结构清晰、注释到位、错误处理务实，CI（fmt + clippy -D warnings + 三平台测试）闭环完整。3.1 / 3.2 / 3.3 共 15 个问题全部处理完毕，当前 111 个测试（89 单测 + 22 集成）全部通过、clippy 与 fmt 干净。
