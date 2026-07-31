# box-proxy

专网 HTTP 接口录制与离线重放代理工具（纯 Rust）。

公司内网用 `record` 模式把 APP 的全部 HTTP 请求/响应录制为快照并下载静态资源；居家用 `replay` 模式把 APP 的服务器 IP 指向本机，即可纯离线复现界面与接口数据（零外网、零专网请求）。

仅支持纯 HTTP（无需 HTTPS/证书），基于 tokio + hyper（HTTP/1.1）。

## 安装

```bash
cargo install --path .
# 安装后 box-proxy 位于 ~/.cargo/bin/box-proxy（已加入全局 PATH）
```

## 用法

### 阶段一：公司内网录制

```bash
box-proxy record            # 默认监听 8888，数据写入 ./box-proxy-api
box-proxy record --port 9999 --dir /path/to/dir
box-proxy record --config box-proxy.toml    # 按配置文件限定录制范围
```

在 APP/设备上把 HTTP 代理配置为本机 IP:8888，完整操作一遍 APP 页面即可。

**流量过滤规则（配置文件）**

- 代理会**转发全部**流量（保证 APP 正常访问，包括系统/其他 APP 的后台请求），但**录制范围可用配置文件过滤**：
  - 不传 `--config`：录制全部经过代理的请求（适合专门给目标 APP 配代理的场景）；
  - 传 `--config <file>`：只录制匹配规则的上游，其余请求正常转发但不落快照、不下载资源。
  - 建议：Wi-Fi 系统级代理会把所有 APP 流量带进来，尽量用配置文件限定业务服务器，避免噪音与同路径冲突。

**配置文件（TOML）示例**，`box-proxy record --config box-proxy.toml`：

```toml
# box-proxy 录制过滤配置
[record]
# 精确匹配：host 匹配该主机任意端口；host:port 精确匹配
include_hosts = ["10.1.2.3:8080", "api.company.com"]
# 正则匹配：对完整 authority（host:port）匹配，大小写不敏感；
# 单引号为字面字符串，正则无需双重转义
include_hosts_regex = [
  '^10\.1\.2\.(3|4):\d+$',
  '\.company\.com(:\d+)?$',
]
```

> 注意：TOML 单引号字面字符串不做转义，`\.` 会原样传给正则引擎（表示匹配字面点号）；若用双引号普通字符串则需写 `\\`。两类规则取并集，任一命中即录制。配置文件非法（不存在/TOML 错误/正则错误）会在启动时直接报错。

**资源落盘规则**（`box-proxy-api/resources/`）

- APP **直接请求过**的静态资源（图片/CSS/JS/字体等，按 Content-Type 判定）→ 落盘 `resources/<host_port>/<原始路径>`，同时作为快照（base64）保存；
- 响应体（JSON/HTML/CSS/JS）中**引用**的资源链接 → 自动提取并下载，支持绝对 URL（`http://10.x.x.x/...`）与根相对路径（`/static/xxx`、`/img/xxx`）；
- 内容按 sha256 去重（`resources/blobs/<hash>`），路径处为硬链接副本，`index.json` 记录映射。

**录制保真**

- 录制过程**原样回传**响应（100% 保真），快照落盘 `box-proxy-api/snapshots/`（含原始绝对 URL）。
- 需要录制时也同步改写回传响应：加 `--rewrite-on-record`（默认关闭，避免影响公司网络下的实时会话）。

### 阶段二：居家离线重放

```bash
box-proxy replay            # 默认监听 8080，读取 ./box-proxy-api
box-proxy replay --port 8090 --rewrite absolute --absolute-base http://192.168.1.100:8090/
```

把 APP 的服务器 IP 改为本机 IP:8080（或配置的端口）后访问。

- 按 **method + path** 匹配快照（忽略 host/query），返回原状态码与改写后的响应。
- 改写模式：
  - `relative`（默认）：`http://10.1.2.3:8080/api/x?a=1` → `/api/x?a=1`，与端口/IP/机器解耦，可移植性最好；
  - `absolute`：替换为 `--absolute-base` 指定地址，适合要求绝对 URL 的客户端；
  - `none`：原样返回。
- 静态资源按路径从 `resources/` 提供；未匹配到快照或资源时返回 404。

## 数据目录与软链接切换

默认数据目录为当前工作目录下的 `box-proxy-api`（两个命令都可用 `-d/--dir` 覆盖），支持软链接：把 `box-proxy-api` 指向不同已录制目录即可切换数据源。

```bash
# 示例：录制目录 A、B 之间切换
ln -s /path/to/recorded-A ./box-proxy-api   # 切换后直接 box-proxy replay
```

```text
box-proxy-api/
├── session.json                  # 会话元数据（工具版本、录制时间、origin 列表、快照数）
├── snapshots/
│   └── <host_port>/              # 按原始上游 host:port 分目录
│       └── <序号>-<METHOD>-<hash>.json   # 单接口快照（请求/响应全量，原始数据）
└── resources/
    ├── index.json                # 资源索引（hash → 路径映射）
    ├── blobs/<sha256>            # 去重 blob
    └── <host_port>/<相对路径>     # 硬链接副本，保留原始目录结构
```

快照为 JSON，可直接手工编辑、删除接口数据，重放时即时生效（启动时加载）。

## 命令行参数

```text
box-proxy record [--port 8888] [--dir ./box-proxy-api] [--config box-proxy.toml] [--rewrite-on-record] [-v]
box-proxy replay [--port 8080] [--dir ./box-proxy-api]
                 [--rewrite relative|absolute|none] [--absolute-base http://127.0.0.1:8080/] [-v]
```

## 开发

```bash
cargo test      # 单元 + 集成测试（本地端口，无外网依赖）
cargo build --release
```

## 已知限制

- 仅 HTTP/1.1 明文；HTTPS/CONNECT 会返回 501（PRD 明确不需要）。
- 响应体整体缓冲后再落盘（适合接口/静态资源场景，超大流式响应暂未做流式落盘）。
- 不同上游若存在相同相对路径的静态资源，重放时按索引顺序命中，优先 origin 精确匹配；建议录制时用 `--config` 的过滤规则收敛范围。
- 相对资源路径提取只覆盖根相对路径（`/static/xxx` 等），`../` 形式的相对引用依赖快照兜底（页面渲染时会作为请求被录制）。
