# tape

专网 HTTP 接口录制与离线重放代理工具（纯 Rust）。

tape 在「公司专网 / 有网环境」下将 App 的全部 HTTP 请求与响应录制为快照并下载静态资源，在「居家 / 无网环境」下以同一地址离线重放，使 App 在零外网、零专网请求的情况下完整复现界面与接口行为。

> 发版前可编辑本文件：在「本版本变更」一节补充该版本的更新内容；不编辑则使用以下默认模板。

## 本版本变更

- 新增 `release.sh` 一键发版脚本：bump 版本 → 自动生成 CHANGELOG → 提交 → 打 tag → 推送触发 CI 发布，全程一条命令。
- 引入 git-cliff 自动生成 `CHANGELOG.md`（按 Conventional Commits 分组，含 Release 链接）。
- README 补充发布新版本流程说明。

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
