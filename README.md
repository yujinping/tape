# tape

HTTP 接口录制与离线重放代理工具（纯 Rust）。

简体中文 | [English](./README.en.md)

tape 在「公司专网 / 有网环境」下，将 App 的全部 HTTP 请求与响应录制为快照，并自动下载页面引用的静态资源；在「居家 / 无网环境」下，以同一地址离线重放这些数据，使 App 在零外网、零专网请求的情况下完整复现界面与接口行为。

- **纯 HTTP 方案**：App 与 tape 之间仅使用 HTTP/1.1 明文，无需安装证书、无需信任根 CA；上游 HTTPS 站点由 tape 作为 TLS 客户端连接并解密后录制。
- **纯 Rust 实现**：基于 tokio + hyper，跨平台（Windows / macOS / Linux），单二进制分发，无运行时依赖。

## 核心特性

- **双模式同址**：`record`（录制）与 `replay`（重放）默认共用 `8888` 端口，两阶段 App 地址无需改动，仅切换 tape 的运行模式。
- **免代理接入**：支持把 tape 当作服务器、URL 加前缀直访（`http://<tape>:8888/http://www.example.com/...`），无需系统代理，适合电视盒子等无法配置代理的设备。
- **客户端兼容**：自动识别三种请求形态——标准正向代理（absolute-form）、URL 前缀式（`/https://host/...`）、单斜杠折叠形式（`/https:/host/...`）；Java（Retrofit / OkHttp）可直接以前缀式地址作为 baseUrl 接入。
- **响应改写**：前缀式请求的跳转与链接自动改写成回到 tape 的地址，离线 / 专网环境不跳公网、不断链；支持 gzip / deflate / br 压缩响应。
- **共用配置**：`record` 与 `replay` 共用同一 TOML 配置文件，含录制过滤、重放端口与改写模式等；命令行参数优先。
- **资源落盘**：App 直接请求过的静态资源与响应中引用的资源自动下载，按 sha256 去重；重放时按路径提供。
- **录制保真**：录制阶段原样回传响应（100% 保真），快照保存原始请求 / 响应，支持手工编辑，重放即时生效。

## 目录

- [核心特性](#核心特性)
- [典型场景](#典型场景)
- [安装与构建](#安装与构建)
- [快速开始](#快速开始)
- [客户端接入兼容性](#客户端接入兼容性)
- [响应改写](#响应改写)
- [配置说明](#配置说明)
- [HTTPS 上游（record 模式）](#https-上游record-模式)
- [数据目录与软链接切换](#数据目录与软链接切换)
- [命令行参数](#命令行参数)
- [跨平台支持（Windows / macOS / Linux）](#跨平台支持windows--macos--linux)
- [开发](#开发)
- [已知限制](#已知限制)

## 典型场景

### 场景一：专网接口录制，居家离线复现

1. 公司内网运行 `tape record`，监听本机 `8888`；App / 设备把 HTTP 代理指向 `192.168.0.100:8888`，完整操作一遍业务页面；
2. 快照与资源落盘到 `tape-api/` 目录，整个目录拷贝回家；
3. 居家运行 `tape replay`，以同一地址提供数据；App 的服务器地址无需改动，纯离线即可复现全部界面与接口。

### 场景二：无法配置系统代理的设备（电视盒子 / 特定 App）

部分盒子或 App 不支持系统级 HTTP 代理，但允许直接指定服务器地址（如配置「基准地址 / baseUrl」）。此时把 tape 当作服务器，在请求路径前拼接完整目标 URL：

```text
http://192.168.0.100:8888/http://www.example.com/api/v1/login
http://192.168.0.100:8888/https://www.example.com/api/v1/login
```

tape 自动识别并转发、录制、重放，无需任何开关；响应中的跳转与链接会自动改写成回到 tape 的前缀式地址，页面在离线环境下不会断链。

### 场景三：Java / Retrofit 客户端以前缀式为 baseUrl

Android / Java 端使用 Retrofit + OkHttp 时，可把 baseUrl 直接设为前缀式地址（详见「客户端接入兼容性」）。实测兼容 Retrofit 2.x + OkHttp 3.x / 4.x，`@Path`、`@Query`、查询参数均正常。

## 安装与构建

```bash
cargo install --path .
# 安装后 tape 位于 ~/.cargo/bin/tape（已加入全局 PATH）
```

需要交叉编译或一键构建时使用 `./build.sh`，详见「跨平台支持」。

## 快速开始

### 阶段一：公司内网录制

```bash
tape record            # 默认监听 8888，数据写入 ./tape-api
tape record --port 9999 --dir /path/to/dir
tape record --config my-filter.toml    # 显式指定配置文件限定录制范围
```

在 App / 设备上把 HTTP 代理配置为本机 IP:8888，完整操作一遍 App 页面即可。录制时 tape 会**转发全部流量**（保证 App 正常访问），但**录制范围可用配置文件过滤**：

- 不传 `--config`：默认读取数据目录（`./tape-api` 或 `--dir` 指定目录）下的 `tape-config.toml`；该文件不存在则录制全部经过代理的请求；
- 传 `--config <file>`：只录制匹配规则的上游，其余请求正常转发但不落快照、不下载资源；文件缺失或非法会在启动时直接报错。
- 建议：Wi-Fi 系统级代理会把所有 App 流量带进来，尽量用配置文件限定业务服务器，避免噪音与同路径冲突。

### 阶段二：居家离线重放

```bash
tape replay            # 默认监听 8888（与 record 一致），读取 ./tape-api
tape replay --port 8090 --rewrite absolute --absolute-base http://192.168.1.100:8090/
```

把 App 的服务器地址改为本机 IP:8888（或配置的端口）后访问；与录制阶段使用同一个地址。重放按 **method + path** 匹配快照（忽略 host / query），返回原状态码与改写后的响应；静态资源按路径从 `resources/` 提供；未匹配到快照或资源时返回 404。

## 客户端接入兼容性

tape 的请求解析自动识别以下三种形态，**无需任何开关**，且可混用：

| 形态 | 示例请求行 | 适用客户端 |
| --- | --- | --- |
| 标准正向代理（absolute-form） | `GET http://www.example.com/api HTTP/1.1` | 支持系统 / App 级 HTTP 代理的客户端 |
| URL 前缀式（双斜杠） | `GET /http://www.example.com/api HTTP/1.1` | 无法配代理、可指定服务器地址的设备（盒子 / App） |
| URL 前缀式（单斜杠，自动兼容） | `GET /https:/www.example.com/api HTTP/1.1` | 会把 scheme 后 `//` 折叠成单斜杠的库 |

### 方式一：标准正向代理

在 App / 设备上配置 HTTP 代理为 `<tape-ip>:8888`，客户端正常发 absolute-form 请求即可。录制 / 重放均按完整目标 URL 处理，响应不改写，行为与普通正向代理一致。

### 方式二：URL 前缀式（免代理，盒子推荐）

请求路径前直接拼接完整目标 URL（含协议、主机、可选端口）：

```text
http://<tape-ip>:8888/http://www.example.com/api/v1/login
http://<tape-ip>:8888/https://www.example.com/api/v1/login
```

- `record`：从路径解析出目标 host 与真实路径，按正常代理逻辑转发、过滤、录制；快照以真实目标 URL 落盘。
- `replay`：剥离前缀后按 method + path 匹配快照（origin 从前缀中提取做精确匹配），静态资源同样可用。
- 目标支持 `http://` 与 `https://` 前缀；`record` 通过 TLS 客户端连接上游 https 站点并解密录制（无 MITM）。
- 监听地址固定为 `0.0.0.0`（本机所有网卡），局域网设备通过 tape 所在电脑的 IP 直接访问。
- 验证建议用 curl 或 App（浏览器地址栏会对前缀做百分号编码与规范化）：

```bash
curl 'http://127.0.0.1:8888/https://www.example.com/api/v1/login'   # 单引号防止 shell 展开
```

### 方式三：Java / Retrofit / OkHttp 前缀式 baseUrl

Android / Java 端可把 baseUrl 直接设置为前缀式地址。以下结论基于 **Retrofit 2.9.0 + OkHttp 3.14.9 / 4.9.3 实测**：

```java
Retrofit retrofit = new Retrofit.Builder()
        .baseUrl("http://192.168.0.100:8888/https://www.example.com/") // 必须以 / 结尾
        .build();

interface Api {
    // 相对路径（不带前导斜杠）：请求行 GET /https://www.example.com/api/v1/login
    @GET("api/v1/login")
    Call<ResponseBody> login();

    // @Path / @Query / 查询参数均正常
    @GET("users/{id}/posts")
    Call<ResponseBody> userPosts(@Path("id") String id, @Query("page") int page);
}
```

**已验证的行为**

- baseUrl 必须以 `/` 结尾：`.../https://www.example.com/`（结尾斜杠不能省）。缺少时 Retrofit 直接抛出 `IllegalArgumentException: baseUrl must end in /`。
- 接口注解使用相对路径（不带前导斜杠）时，最终请求行为 `GET /https://www.example.com/api/v1/login`，tape 正常识别并转发。
- `@Path`、`@Query`、URL 内查询参数（`@GET("api/v1/login?x=1")`）拼接均正常。
- OkHttp 的 `HttpUrl` 不会折叠路径中的 `//`，`https://` 前缀原样保留；若个别库把 `//` 折叠成单斜杠（`/https:/host/...`），tape 也已自动兼容。
- 实测 OkHttp 3.x 与 4.x 行为一致。

**需要注意的边界**

- **前导斜杠**：`@GET("/api/v1/login")` 会按根路径解析，前缀被丢弃，最终请求为 `GET /api/v1/login`，tape 无法从中恢复目标主机（信息已丢失）。请统一使用不带前导斜杠的相对路径。
- **完整 URL 绕过**：`@Url` 传完整 URL 或 `@GET("https://...")` 会绕过前缀直连原站（不经过 tape），这是 Retrofit 固有语义，接入时需避免。
- **Host 头**：tape 转发时统一以目标上游的 `host[:port]` 覆写 Host 头，客户端原始 Host（通常是 tape 自身地址）会被丢弃，避免上游 WAF / 反 SSRF 拒绝。

### 浏览器直接访问

浏览器地址栏输入前缀式地址可浏览录制 / 重放出的页面，但注意：

- 浏览器会把前缀中的 `:` 百分号编码为 `%3A`（个别浏览器编成 `%20`），tape 已兼容这类编码；
- 浏览器还会做大小写、斜杠等规范化，可能改变请求路径；页面调试建议优先用 curl 或 App，正式使用以 App 为主。

## 响应改写

前缀式请求的响应会被自动改写成回到 tape 的地址，保证后续跳转与资源请求继续走 tape：

- 绝对地址：`https://host/path` → `http://<tape-ip>:8888/https://host/path`（覆盖 `Location` 头与文本 body）；
- 协议相对：`//host/path`（HTML `src` / `href`、CSS `url()` 中常见，浏览器会按页面协议解析成 `http://host` 直连公网）；
- 根相对路径：HTML 标签属性 `href="/assets/x.css"`、CSS `url(/fonts/x.woff2)`（浏览器会解析成 `http://<tape>/...` 丢失前缀）。
- **XML 命名空间 / DTD 标识符**（如 `xmlns="http://www.w3.org/2000/svg"` 等 w3.org URL）不会改写，避免破坏 SVG / XML 导致浏览器拒绝渲染。
- 快照始终保存原始响应，录制保真不受影响；absolute-form（标准代理）请求不启用该改写，保持原有行为。

**压缩响应**：record 转发时会把 `Accept-Encoding` 覆写为 `identity`，让上游返回明文，保证 HTML / JS / CSS 都能被改写；同时内置 gzip / deflate / br 解压 → 改写 → 重压的安全网，兼容个别忽略 `identity` 的上游与历史压缩快照。

## 配置说明

### 共用配置文件

`record` 与 `replay` 共用同一 TOML 配置文件。仓库根目录提供可直接使用的样例 [`tape-config.example.toml`](./tape-config.example.toml)（含 `[record]` / `[replay]` 两表及详细参数说明）。两种使用方式：

1. 复制为数据目录下的 `tape-config.toml`（默认 `./tape-api/`，或 `--dir` 指定目录），record / replay 无需传 `--config` 即可自动加载；
2. 放在任意位置，用 `tape record --config <file>` 或 `tape replay --config <file>` 显式指定（显式指定的文件必须存在）。

取值优先级：**命令行显式参数 > 配置文件 > 内置默认值**。例如 `tape replay --port 9999` 会覆盖配置文件里的 `port`。

### [record] 录制过滤

`include_hosts` 与 `include_hosts_regex` 取并集，任一命中即录制；未匹配的请求仍正常转发，只是不落快照、不下载资源。两项都不设置时录制全部流量。

```toml
[record]
# 精确匹配：host 匹配该主机任意端口；host:port 精确匹配
include_hosts = ["10.1.2.3:8080", "api.company.com"]
# 正则匹配：对完整 authority（host:port）匹配，大小写不敏感
include_hosts_regex = [
  '^10\.1\.2\.(3|4):\d+$',
  '\.company\.com(:\d+)?$',
]
```

> 注意：TOML 单引号字面字符串不做转义，`\.` 会原样传给正则引擎（匹配字面点号）；若用双引号普通字符串则需写 `\\`。配置文件非法（TOML 错误 / 正则错误）会在启动时直接报错；显式 `--config` 指定的文件不存在也会报错。

### [replay] 重放与改写

```toml
[replay]
port = 8888            # 重放服务监听端口，默认 8888（与 record 一致）
rewrite = "relative"   # relative / absolute / none，默认 relative
absolute_base = "http://127.0.0.1:8888/"   # 仅 absolute 模式生效
```

- `rewrite`：
  - `relative`（默认）：把响应中引用本机地址的绝对 URL 改写为相对路径，快照跨机器、换端口、换目录都能直接用；
  - `absolute`：改写为 `absolute_base` 指定基地址，适合依赖固定域名 / IP 的客户端；
  - `none`：不改写，原样返回录制的响应。
- `absolute_base`：仅 `rewrite = "absolute"` 时生效，建议以 `/` 结尾，默认 `http://127.0.0.1:8888/`。
- 监听地址固定为 `0.0.0.0`，与 `record` 一致，局域网设备可直接访问。

## HTTPS 上游（record 模式）

- 前缀式 `/https://www.example.com/...` 与 absolute-form `https://www.example.com/...` 均支持：tape 作为 TLS 客户端连上游，解密后照常转发、过滤、录制；快照 origin 记录为 `https://host:port`。
- 默认使用**系统根证书**校验上游证书（兼容公司内部 CA 已装入系统的场景）。
- 专网自签证书时设置环境变量 `TAPE_INSECURE_TLS=1` 跳过证书校验（仅建议内网使用）。
- 静态资源（`resources/`）中的 https 链接同样支持下载。

## 数据目录与软链接切换

默认数据目录为当前工作目录下的 `tape-api`（两个命令都可用 `-d/--dir` 覆盖），支持软链接切换已录制目录：

```bash
ln -s /path/to/recorded-A ./tape-api   # 切换后直接 tape replay
```

```text
tape-api/
├── session.json                  # 会话元数据（工具版本、录制时间、origin 列表、快照数）
├── snapshots/
│   └── <host_port>/              # 按原始上游 host:port 分目录
│       └── <序号>-<METHOD>-<hash>.json   # 单接口快照（请求/响应全量，原始数据）
└── resources/
    ├── index.json                # 资源索引（hash → 路径映射）
    ├── blobs/<sha256>            # 去重 blob
    └── <host_port>/<相对路径>     # 硬链接副本，保留原始目录结构
```

**资源落盘规则**

- App **直接请求过**的静态资源（图片 / CSS / JS / 字体等，按 Content-Type 判定）→ 落盘 `resources/<host_port>/<原始路径>`，同时作为快照（base64）保存；
- 响应体（JSON / HTML / CSS / JS）中**引用**的资源链接 → 自动提取并下载，支持绝对 URL 与根相对路径；
- 内容按 sha256 去重（`resources/blobs/<hash>`），路径处为硬链接副本，`index.json` 记录映射。

快照为 JSON，可直接手工编辑、删除接口数据，重放时即时生效（启动时加载）。

## 命令行参数

```text
tape record [--port 8888] [--dir ./tape-api] [--config tape-config.toml] [--rewrite-on-record] [-v]
tape replay [--port 8888] [--dir ./tape-api] [--config tape-config.toml]
                 [--rewrite relative|absolute|none] [--absolute-base http://127.0.0.1:8888/] [-v]
tape list [--dir ./tape-api]
```

- `--config`：record / replay 共用配置文件。未指定时默认读取数据目录下的 `tape-config.toml`；该文件不存在时 record 录制全部、replay 使用内置默认值；显式指定则必须存在且合法。
- `--port`：record / replay 默认都是 `8888`，录制与重放两阶段 App 地址保持一致。
- `tape list`：列出数据目录下缓存的站点，以及每个站点的接口快照数与资源文件数（以 `snapshots/` 目录为准）。
- `--rewrite-on-record`：录制时同步改写回传给 App 的响应（默认关闭，避免影响公司网络下的实时会话）。

## 跨平台支持（Windows / macOS / Linux）

- 代码完全跨平台（tokio + hyper，无平台相关依赖），快照 / 资源目录可在任意平台间拷贝迁移复用。
- **一键构建**：[`build.sh`](./build.sh)（当前系统）、`./build.sh mac`（macOS）、`./build.sh win`（Windows x64 交叉编译），产物统一输出到 `dist/`。
  - Windows x64 交叉编译需 mingw：macOS 上 `brew install mingw-w64`。
  - 体积优化：release 已开启 LTO / panic=abort / strip（无运行代价，各平台约减半）；`win` 构建默认用 UPX 再压一道（`UPX=0` 跳过），Windows 版约 10MB → 1.2MB。注意：UPX 打包可能被部分杀软误报，如遇拦截可 `UPX=0 ./build.sh win` 出未压缩版。
- **Windows 本地构建**（MSVC）：安装 Rust 后直接 `cargo build --release`，产物 `target\release\tape.exe`。
- **CI 构建**：[`.github/workflows/ci.yml`](./.github/workflows/ci.yml) 在 Windows / macOS / Linux 三平台跑 fmt / clippy / test；打 tag 时自动构建三个平台的 release 产物并上传 artifact。
- Windows 兼容细节：资源副本文件名做了安全化（非法字符替换、裁剪尾随点 / 空格、Windows 保留设备名 CON/NUL/COM1 等加下划线前缀）；内容哈希去重不受影响；重放仍按原始路径匹配。

## 开发

```bash
cargo test      # 单元 + 集成测试（本地端口，无外网依赖）
cargo build --release
```

## 已知限制

- App ↔ tape 之间仅 HTTP/1.1 明文（tape 不做 HTTPS 服务端，App 无需装证书）；上游 https 站点已支持，但 **CONNECT 隧道 / MITM 抓 https 明文**不支持（需要 CA 证书体系与设备信任）。
- 响应体整体缓冲后再落盘（适合接口 / 静态资源场景，超大流式响应暂未做流式落盘）。
- 不同上游若存在相同相对路径的静态资源，重放时按索引顺序命中，优先 origin 精确匹配；建议录制时用 `--config` 的过滤规则收敛范围。
- 相对资源路径提取只覆盖根相对路径（`/static/xxx` 等），`../` 形式的相对引用依赖快照兜底（页面渲染时会作为请求被录制）。
