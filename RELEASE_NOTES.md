# tape

专网 HTTP 接口录制与离线重放代理工具（纯 Rust）。

tape 在「公司专网 / 有网环境」下将 App 的全部 HTTP 请求与响应录制为快照并下载静态资源，在「居家 / 无网环境」下以同一地址离线重放，使 App 在零外网、零专网请求的情况下完整复现界面与接口行为。

> 发版前可编辑本文件：在「本版本变更」一节补充该版本的更新内容；不编辑则使用以下默认模板。

## 本版本变更

- **修复**：重放时带 query 参数的代理式 / 前缀式请求全部 404 的问题——快照与资源索引按「去 query 的路径」匹配，与录制侧一致（此前带 query 的接口在离线重放时会 MISS）。
- **修复**：静态资源索引改为按「origin + path」精确匹配，多站点同路径资源不再互相串站；页面引用的跨域 CDN 资源按自身域名归属。
- **修复**：重复录制同一数据目录时，快照序号从已有最大 id 续起，不再回到 `000001` 静默覆盖旧快照。
- **性能**：录制落盘改为增量维护 `session.json` 快照数，消除每次全量重读的 O(n²) 开销。
- **安全**：`TAPE_INSECURE_TLS` 改为显式白名单（`1` / `true` / `yes` / `on`），`false` 不再误开启跳过证书校验；record / replay 新增 `--bind` 监听地址选项（默认 `0.0.0.0` 不变），可用 `127.0.0.1` 限制仅本机。
- **完善**：replay 资源路径仅允许 GET / HEAD（其余 405）；非 ASCII 头值按 lossy 保留不再置空；logcat 的 adb 调用不再阻塞异步运行时；日志落盘文件名增加毫秒精度防同秒截断；快照目录区分 http / https；损坏快照与 session.json 写入失败改为告警；无 path 带 query 的 URL 改写不再丢 query。
- 新增完整代码审查报告 `docs/code-review-2026-08-02.md`（3.1 / 3.2 / 3.3 共 15 项问题全部处理）。

## 核心亮点

- **双模式同址**：`record` 与 `replay` 默认共用 `8888` 端口，录制 / 重放两阶段 App 地址无需改动，只需切换运行模式。
- **免代理接入**：支持 URL 加前缀直访（`http://<tape>:8888/http://www.example.com/...`），无需系统代理，适合电视盒子等无法配置代理的设备。
- **客户端兼容**：自动识别标准正向代理（absolute-form）、URL 前缀式、单斜杠折叠三种请求形态；Java（Retrofit 2.x + OkHttp 3.x / 4.x）可直接以前缀式地址作为 `baseUrl` 接入（实测验证）。
- **响应改写**：前缀式请求的跳转与链接自动改写成回到 tape 的地址，离线环境不跳公网、不断链；支持 gzip / deflate / br 压缩响应。
- **共用配置**：`record` 与 `replay` 共用同一 TOML 配置文件，含录制过滤、重放端口、改写模式，命令行参数优先。
- **录制保真**：录制阶段原样回传响应（100% 保真）；快照为可编辑 JSON，重放即时生效。
- **资源落盘**：静态资源自动下载并按 sha256 去重，重放按路径提供。
- **HTTPS 上游**：作为 TLS 客户端连接并解密录制，默认校验系统根证书，内网自签证书可用 `TAPE_INSECURE_TLS=1`。
- **跨平台**：Windows / macOS / Linux 单二进制；`build.sh` 一键构建，CI 三平台自动测试、构建与发布。

## 快速上手

```bash
# 阶段一：公司内网录制（默认监听 8888，数据写入 ./tape-api）
tape record

# 阶段二：居家离线重放（同一地址，无需改动 App 配置）
tape replay
```

盒子 / 无法配置代理的设备，把 tape 当服务器、路径前拼完整目标 URL 即可：

```text
http://192.168.0.100:8888/https://www.example.com/api/v1/login
```

Java / Retrofit 客户端可直接设置前缀式 baseUrl（必须以 `/` 结尾，接口用相对路径）：

```java
Retrofit retrofit = new Retrofit.Builder()
        .baseUrl("http://192.168.0.100:8888/https://www.example.com/")
        .build();
```

## 获取二进制

- 源码安装：`cargo install --path .`
- 打 tag 后 CI 自动构建三平台二进制并挂载到本 Release。

## 已知限制

- App ↔ tape 之间仅 HTTP/1.1 明文（无 HTTPS 服务端 / 无需装证书）；不支持 CONNECT 隧道 / MITM 抓 https 明文。
- 响应体整体缓冲后落盘，超大流式响应暂不支持流式落盘。
- 相对资源路径提取覆盖根相对路径（`/static/xxx` 等），`../` 形式依赖快照兜底。

## 文档

- [README（中文）](https://github.com/yujinping/tape/blob/main/README.md)
- [README（English）](https://github.com/yujinping/tape/blob/main/README.en.md)
- [样例配置文件](https://github.com/yujinping/tape/blob/main/tape-config.example.toml)
