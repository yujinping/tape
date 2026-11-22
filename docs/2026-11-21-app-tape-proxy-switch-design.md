# App 端「使用 TAPE 代理」开关设计方案

> 面向：电视盒子 App（Android）设置页与网络层
> 目标：用一个开关 + IP + 端口，把业务接口与图片流量导入本机 tape；换人换机零改包，地址填错不致命
> 覆盖：业务 HTTP 接口、图片、WebView（约 90% 以上场景）；不含第三方 SDK、视频流、长连接
> 状态：方案已定，待评审排期

## 1. 背景

### 1.1 采集场景

测试同事在自己笔记本上启动 tape（`tape record`），盒子与笔记本处于同一局域网。盒子 App 的 HTTP 流量经 tape 转发，落盘为可回放、可对比的录制数据。

需要采集的内容分三类：

1. **业务接口**：Retrofit/OkHttp 发出的 API 请求与响应；
2. **图片资源**：列表封面、详情大图等；
3. **WebView 内部流量**：H5 页面本身、页面里的 JS/CSS/XHR/图片。

### 1.2 现状的三个问题

| 问题 | 具体表现 | 后果 |
| --- | --- | --- |
| 地址写死 | 包内写死某台电脑的 IP 或某个域名 | 换人、换办公位就得重新打包发版 |
| 地址错就挂 | baseUrl 指向不存在的 IP，TCP 建连超时叠加 OkHttp 重试 | 业务界面长时间无响应，测试以为 App 崩了 |
| 盒子环境受限 | 部分盒子不能写 sdcard、不能 adb、不能改系统代理 | 推不了配置、抓不了包、系统级方案全部失效 |

还有一条网络约束：部分测试人员的网络与公司服务器不通（运营商专网、跨省不互通）。因此**采集端必须与盒子同局域网**，不能假设有一个集中的公网接收端。

### 1.3 为什么最终选代理，而不是客户端拦截器

最初设想过在 Retrofit/OkHttp 上加拦截器，把请求与响应上报到某个接口。两者在当前约束下的对比如下：

| 维度 | 代理（tape） | 客户端拦截器上报 |
| --- | --- | --- |
| 覆盖面 | 进程内所有 HTTP 流量，含 WebView、图片库 | 只覆盖挂了拦截器的 client |
| 保真度 | 线上字节原样（含 gzip、异常响应） | 需自行序列化，容易丢边界信息 |
| 语义信息 | 无接口名、无页面归属 | 有接口名、类型化参数、耗时、异常栈 |
| 接入成本 | 改一次网络层，之后只填地址 | 每个 client 都要挂，需插桩或改构建 |
| 换环境成本 | 设置页填 IP 即可 | 仍要重新打包（除非把上报地址也做成可配） |
| 失败影响 | 代理不通则业务请求失败，需健康检查兜底 | 上报失败可静默，不影响业务 |

结论：**覆盖面和接入成本由代理胜出**，语义信息是拦截器的优势，但本期目标是「完整录下流量」，不是「理解流量」。拦截器留作后续的语义补强通道（见 13.4），不作为主链路。若将来出现 HTTPS + 证书固定的场景，代理无法解密，届时再单独评估。

## 2. 结论摘要

- 设置页只放**一个开关、一个 IP、一个端口**；协议不暴露，内部固定 `http`。
- IP 前三段由盒子本机 IP 预填，用户通常只输最后一段；端口默认 `8888`，折叠显示。
- 路由用 OkHttp `ProxySelector`，**只代理 `http`**，不改 URL、不改 baseUrl，Host/Cookie/签名全部保持原样。
- 开关内置健康检查（`GET /__tape/health`，1.5s），**连不上就不允许打开**，从根上消灭「地址错就挂」。
- WebView 用**前缀式入口 URL** 接入，页面与页面内请求自然流经 tape，也能覆盖 https 上游。
- 切换开关后清空连接池，保证立即生效。

## 3. 总体方案

```text
┌────────────────────── 电视盒子 ──────────────────────┐
│  App 业务层                                          │
│    ├─ Retrofit / OkHttp ──┐                          │
│    ├─ 图片 Glide / Coil ──┤ 共用同一个 OkHttpClient   │
│    └─ WebView ────────────┼─ 入口 URL 前缀式          │
│                           ▼                          │
│                 TapeProxySelector                    │
│           （开关开 && scheme == http）               │
└───────────────────────────┼──────────────────────────┘
                            │ HTTP 代理（absolute-form）
                            ▼
                  ┌───────────────────┐
                  │   tape : 8888     │   同局域网笔记本
                  │   record / replay │
                  └─────────┬─────────┘
                            │ 明文 http / 前缀式 https
                            ▼
                         上游服务
```

三条路径互相独立，互不嵌套：

1. **接口与图片**：OkHttp 走 `ProxySelector`，tape 收到 `GET http://host/path HTTP/1.1`，转发上游；
2. **WebView**：页面地址被改写为 `http://<tape>/<原始 URL>`，tape 解析前缀后转发；响应体里的绝对/根相对链接由 tape 改写回前缀，页面内请求继续流经 tape（JS 运行时拼的根相对地址除外）；
3. **健康检查**：设置页用不带代理的裸 client 直连 tape 的 `/__tape/health`，与上面两条链路无关。

与 tape 现有能力的对应：

| 能力 | tape 现状 | 本期是否要改 |
| --- | --- | --- |
| 接收 absolute-form 代理请求 | 已支持 | 否 |
| 前缀式入口（`/http://...`、`/https://...`） | 已支持，含单斜杠折叠 | 否 |
| 响应体保存、gzip/deflate/br | 已支持 | 否 |
| 资源下载与 sha256 去重 | `download.rs` 已具备 | 否 |
| 录制范围过滤 | `--config` | 否（部署时配置） |
| 健康检查端点 | 已实现 `GET /__tape/health`（兼容旧 `/health`），返回 `{ok, mode, version}` | 否 |
| 明文 HTTP 监听 | 默认监听全网卡 | 否（注意内网暴露面） |

## 4. 配置与交互

### 4.1 界面

```text
┌──────────────────────────────────┐
│  使用 TAPE 代理          [ 开 ]  │
│                                  │
│  代理 IP    192.168.1. [ 23 ]    │
│  端口       8888          [改]   │
│                                  │
│  本机 IP    192.168.1.10         │
│  状态       ● 已连接 · record    │
└──────────────────────────────────┘
```

可交互元素只有三个：开关、IP 最后一段、端口（默认折叠）。输入控件已具备，本文不再设计键盘交互。

### 4.2 字段规则

| 字段 | 规则 | 默认值 | 非法时 |
| --- | --- | --- | --- |
| 开关 | 开启前必须通过健康检查 | 关 | — |
| 代理 IP | 四段十进制，每段 1–254；前三段可改，最后一段是主输入 | 本机 IP 前三段 + 上次成功值 | 提示「IP 不合法」，不发起检查 |
| 端口 | 1–65535 整数 | 8888 | 提示「端口不合法」，不发起检查 |
| 协议 | 界面不出现 | 固定 `http` | — |

### 4.3 自动预填

- 进入设置页时读取本机 IPv4，取前三段；
- 最后一段优先用「上次成功连接」的值，没有则留空；
- 端口优先用上次成功的值，否则 8888；
- 预填只是默认值，四段都可改。盒子在 `10.1.5.23/16`、电脑在 `10.1.9.5` 这类非 /24 网段，前三段推出来是错的，必须允许改；
- 盒子没有网络、拿不到本机 IP 时不做预填，全部留空。

「上次成功」只在健康检查通过后写入，避免把错地址记成默认值。

### 4.4 开关语义

```text
[关] --用户开启--> [校验中] --health ok----> [开 · 已连接]
                       │
                       └--health fail-----> [关] + 原因提示

[开 · 已连接] --用户关闭--> [关] + evictAll
[开 · 已连接] --运行中请求失败--> [开 · 异常] + 顶部提示（不自动关）
```

规则：

1. 点击开关先做本地字段校验，非法直接提示，不进入「校验中」，避免无谓等待；
2. 健康检查用**裸 client**（`proxy(Proxy.NO_PROXY)`），否则代理 selector 会把检查请求也送进 tape，形成自指；
3. 超时 1.5s。失败文案必须可执行，例如「连不上 192.168.1.23:8888，确认电脑上 tape 已启动、与盒子同网段」；
4. 只有检查通过才落盘并 `evictAll()`；失败不落盘，开关弹回；
5. 运行中失败不自动关闭——代理中途挂掉时自动关会让「关了之后请求是否恢复」变得不可预测。只把状态改成「异常」并给一键关闭入口。

### 4.5 存储与生效

- `SharedPreferences` 存 `enabled`、`host`、`port`、`lastGoodHost`、`lastGoodPort`；
- 内存里维护 `@Volatile` 快照，`ProxySelector.select()` 每次读快照，不加锁；
- 开关状态变化时：更新快照 → `connectionPool.evictAll()`；
- 设置页只在 debug / 内测变体注册，release 变体不编译相关代码（见 5.9 安全约束）。

## 5. 客户端实现

### 5.1 配置模型

```kotlin
data class TapeProxy(
    val host: String,
    val port: Int,
    val hosts: Set<String> = emptySet() // 空 = 所有 http 主机
)

object TapeConfig {
    @Volatile
    private var current: TapeProxy? = null

    fun snapshot(): TapeProxy? = current
    fun update(proxy: TapeProxy?) { current = proxy }
}
```

`hosts` 白名单本期默认空，保留给「只代理业务域名、放过某些内网 http 服务」的场景。

### 5.2 路由：ProxySelector

```kotlin
class TapeProxySelector : ProxySelector() {

    override fun select(uri: URI): List<Proxy> {
        val cfg = TapeConfig.snapshot() ?: return DIRECT
        if (!uri.scheme.equals("http", true)) return DIRECT
        if (cfg.hosts.isNotEmpty() && uri.host !in cfg.hosts) return DIRECT
        return listOf(Proxy(Proxy.Type.HTTP, InetSocketAddress(cfg.host, cfg.port)))
    }

    override fun connectFailed(uri: URI, sa: SocketAddress, e: IOException) {
        TapeStatus.markFailed(e) // 只更新界面状态，不改变开关
    }

    private companion object {
        // 直连：注意这里不是环境变量 NO_PROXY，而是 java.net.Proxy 的 DIRECT 常量
        val DIRECT: List<Proxy> = listOf(Proxy.NO_PROXY)
    }
}
```

挂载点：

```kotlin
val okHttpClient = OkHttpClient.Builder()
    .proxySelector(TapeProxySelector())
    .connectTimeout(3, TimeUnit.SECONDS) // 代理失效时快速失败；可按实测调整
    .build()
```

要点：

- **不要改 baseUrl**。`ProxySelector` 在网络层替换目标，URL 里的 host/path/query 原样保留，服务端的签名、Host 校验、Cookie 域都不受影响；
- **scheme 过滤是安全阀**。第三方 SDK 多为 https，过滤后直连，既不会被代理拖慢，也不会把第三方流量灌进 tape；
- `Retrofit` 的 `@Url` 绝对地址同样会被代理，因为决策在 client 层；
- 若 App 内存在 `HttpURLConnection` 直连路径，`ProxySelector` 覆盖不到，需要在对应位置单独确认（见 12 待确认项）。

### 5.3 切换与连接池

```kotlin
fun apply(proxy: TapeProxy?) {
    TapeConfig.update(proxy)
    okHttpClient.connectionPool.evictAll()
}
```

`OkHttp` 的连接池按 route（host/port/proxy）复用。只改 selector 而不清池，已有连接会继续走旧路线，表现为「开关点了没反应、还是录不到」。清池是必须动作，不是优化。

### 5.4 健康检查

```kotlin
sealed interface HealthResult {
    data class Ok(val mode: String) : HealthResult
    data class Fail(val reason: String) : HealthResult
}

object TapeHealth {
    private val bare = OkHttpClient.Builder()
        .proxy(Proxy.NO_PROXY) // 关键：不套自己
        .connectTimeout(1500, TimeUnit.MILLISECONDS)
        .readTimeout(1500, TimeUnit.MILLISECONDS)
        .build()

    fun check(host: String, port: Int): HealthResult = try {
        val req = Request.Builder().url("http://$host:$port/__tape/health").build()
        bare.newCall(req).execute().use { resp ->
            if (!resp.isSuccessful) {
                HealthResult.Fail("tape 返回 HTTP ${resp.code}")
            } else {
                val body = resp.body?.string().orEmpty()
                val json = runCatching { JSONObject(body) }.getOrNull()
                if (json == null || !json.optBoolean("ok")) {
                    HealthResult.Fail("响应不是 tape：$body")
                } else {
                    HealthResult.Ok(json.optString("mode", "unknown"))
                }
            }
        }
    } catch (e: IOException) {
        HealthResult.Fail(e.message ?: "连不上")
    }
}
```

tape 0.2.0 起已内置 `/__tape/health`，返回 `ok` / `mode` / `version`，因此不需要再用根路径探测做过渡。仅在对接旧版 tape 时，才退化为「请求 `http://host:port/`，收到任意 HTTP 响应即视为连通」。

### 5.5 读取本机 IPv4

```kotlin
fun localIpv4(ctx: Context): String? {
    val cm = ctx.getSystemService(ConnectivityManager::class.java)
    val network = cm.activeNetwork ?: return null
    val lp = cm.getLinkProperties(network) ?: return null
    return lp.linkAddresses.asSequence()
        .map { it.address }
        .filterIsInstance<Inet4Address>()
        .firstOrNull { !it.isLoopbackAddress }
        ?.hostAddress
}
```

- 需要 `ACCESS_NETWORK_STATE`（普通权限，无需运行时申请）；
- 取 `activeNetwork`（有默认路由的那张网卡），不要按网卡顺序取。盒子常常 WiFi 与有线同时在线，取错会预填到另一个网段；
- 取不到就留空，不要瞎猜；
- 上面的写法要求 API 23+。盒子若低到 API 19/21，改用 `getActiveNetworkInfo()` 取当前网络，再调 `getLinkProperties(networkInfo)`（已废弃但在旧系统可用），取不到时留空即可——预填只是省打字，不能因为版本差异影响功能。

### 5.6 WebView 前缀式接入

```kotlin
fun tapeUrl(origin: String): String {
    val cfg = TapeConfig.snapshot() ?: return origin
    require(origin.startsWith("http://") || origin.startsWith("https://")) {
        "origin 必须是绝对地址"
    }
    return "http://${cfg.host}:${cfg.port}/$origin"
}
```

调用点：

```kotlin
webView.loadUrl(tapeUrl("https://h5.example.com/home"))
```

- tape 侧已支持 `/https:/host/...`（单斜杠折叠）与 `/https://host/...` 两种写法；
- 不带前导斜杠的相对地址（`api/x`、`./img/a.png`）会拼在当前前缀路径下，自然落在 tape 内；
- 带前导斜杠的（`/api/x`）会被浏览器解析到 tape 根，靠 tape 的响应改写兜住（HTML 属性、CSS `url()`、文本中的绝对 URL 都会改写）；但 JS 运行时拼出来的 `/api/x` 改写不了，是盲区；
- 开关关闭时 `tapeUrl` 返回原地址，WebView 行为不变。

### 5.7 图片库共用 client

图片是独立于业务 API 的第二条链路，必须显式注入，否则漏图。

Glide：

```kotlin
@GlideModule
class TapeGlideModule : AppGlideModule() {
    override fun registerComponents(ctx: Context, glide: Glide, registry: Registry) {
        registry.replace(
            GlideUrl::class.java,
            InputStream::class.java,
            OkHttpUrlLoader.Factory(okHttpClient) // 与业务共用
        )
    }
}
```

Coil：

```kotlin
ImageLoader.Builder(context)
    .okHttpClient { okHttpClient }
    .build()
```

### 5.8 明文流量

Android 9（targetSdk 28）起默认禁止明文 HTTP。代理地址是 `http://`，因此：

```xml
<!-- debug / 内测变体的 AndroidManifest.xml -->
<application android:usesCleartextTraffic="true" ... >
```

若已配置 `networkSecurityConfig`，则需要在该文件中放开（`cleartextTrafficPermitted="true"`），且 `usesCleartextTraffic` 会被忽略。**只在测试变体放开**，release 不要带。

### 5.9 接入位置清单

| 位置 | 改动 | 说明 |
| --- | --- | --- |
| OkHttpClient 构建处 | 挂 `TapeProxySelector` | 业务与图片共用同一个实例 |
| 图片加载初始化 | 注入同一个 client | Glide/Coil，见 5.7 |
| WebView 加载入口 | URL 过 `tapeUrl()` | 只改加载点，不改 H5 |
| 设置页 | 开关 + IP + 端口 + 状态行 | 输入控件已有 |
| Application 启动 | 读取 SP，填充内存快照 | 冷启动即生效 |
| debug manifest | 允许明文 | 5.8 |
| 构建配置 | 设置页与相关类仅测试变体编译 | release 不可达，避免后门 |

安全约束：一个能指向任意 IP:PORT 的流量出口，在正式包里等同于后门（可被用来做中间人）。因此设置页、`TapeProxySelector` 的启用逻辑、相关字符串资源都应在 release 变体中被裁掉或不可达，而不是仅靠 `BuildConfig.DEBUG` 在运行时判断。

## 6. WebView 方案取舍

| 方案 | 能拿到什么 | 拿不到什么 | 结论 |
| --- | --- | --- | --- |
| `shouldInterceptRequest` | URL、method、部分 header | **POST body**、流式响应、WebSocket | 不用，只可作兜底 |
| 入口 URL 前缀式 | 页面、JS/CSS、XHR（GET/POST 全量）、图片；响应体里的绝对/根相对链接会被 tape 改写回前缀 | JS 运行时拼出的根相对地址、WebSocket | **采用** |
| 系统代理 | 全部 http | 盒子改不了系统代理 | 不可行 |

前缀式不是「原样透传」，tape 会对文本响应做改写：`http(s)://host/...`、`//host/...`、HTML 的 `href/src/action/poster`、CSS 的 `url(/...)` 都会被改写成 `http://<tape>/<scheme>://<host>/...`（`src/rewrite.rs`，`record.rs` 中 `prefix_style` 分支）。因此**页面里写死的绝对地址并不是盲区**，仍会被带回 tape。

真正的两个代价：

1. **origin 变了**。页面运行在 `http://<tape>:8888/https://...` 下，若 H5 校验 `location.origin` 或依赖它做鉴权，会异常。需要 H5 侧确认（见 12）；
2. **JS 运行时拼出的根相对地址是盲区**。`fetch('/api/x')`、`axios.get('/api/x')` 在浏览器里解析为 `http://<tape>:8888/api/x`，丢了上游信息，tape 无法判断该转发到哪。响应改写只能处理静态文本里的 URL，处理不了运行时拼接。H5 若统一用 `baseURL` 拼绝对地址则不受影响（绝对地址形态会被改写）。

WebSocket（`ws://`、`wss://`）不走前缀式，不在本期覆盖范围。

## 7. 覆盖矩阵

| 流量类型 | 覆盖 | 路径 | 备注 |
| --- | --- | --- | --- |
| 业务 API（GET/POST/PUT/DELETE） | 是 | ProxySelector | 含请求体与响应体 |
| 业务图片 | 是 | ProxySelector | 需共用 client，见 5.7 |
| WebView 首屏 HTML | 是 | 前缀式 | |
| WebView 内 JS/CSS | 是 | 前缀式 | |
| WebView 内 XHR/fetch（GET/POST） | 是 | 前缀式 | 地址为绝对 URL 或相对路径；含 POST body |
| WebView 内图片 | 是 | 前缀式 | |
| 页面/JS 文本中的绝对 URL | 是 | 前缀式 + 响应改写 | 仅文本类响应（HTML/CSS/JS） |
| JS 运行时拼的根相对地址 | 否 | — | 如 `fetch('/api/x')`，解析到 tape 根，无上游信息 |
| 视频 / ExoPlayer | 否 | — | 大文件、分片，不适合全量缓冲 |
| WebSocket / 推送长连接 | 否 | — | 协议不同 |
| gRPC / native 直连 | 否 | — | 非 HTTP/1.1 |
| 第三方 SDK（多为 https） | 否 | — | scheme 过滤后直连，不影响其功能 |
| DNS / UDP | 否 | — | 代理不接管 |

矩阵之外还有一类：**系统级或其它进程的流量**。代理只作用于本 App 进程，这是与「盒子全局抓包」的本质区别，也是无法 adb 环境下的唯一可行解。

## 8. tape 侧配套

### 8.1 `GET /__tape/health`（已实现）

- 在代理路由**之前**拦截，不写入录制数据；
- 返回：

```json
{ "ok": true, "mode": "record", "version": "0.2.0" }
```

- `mode` 取当前运行模式（`record` / `replay`），供设置页状态行显示；`version` 用于确认「连的是不是我以为的那个 tape」；
- 响应头带 `Cache-Control: no-store`，避免盒子或中间层缓存健康检查结果；
- 路径带 `__tape` 前缀，避免与上游业务接口（尤其透明模式下常见的 `/health`）冲突；
- 识别条件很严格，只认**直连 tape** 的 `GET /__tape/health`（origin-form，无 authority）。这样不会误伤业务流量：
  - 正向代理请求 `GET http://upstream/health` 带 authority，继续转发上游并正常录制；
  - 前缀式请求 `/http://upstream/health` 路径不等于 `/__tape/health`，同样不拦截。
- 兼容旧路径 `GET /health`：仅当请求没有 `Host`、或 `Host` 端口等于 tape 监听端口时才响应；透明模式下上游的 `/health` 因此能正常转发。
- 实现位置：`src/health.rs`（`is_request` / `response`），`src/record.rs` 与 `src/replay.rs` 的 `handle_request` 顶部各有一个早退判断；
- 集成测试覆盖三种情况：record 模式返回且不落快照、replay 模式返回、上游 `/health` 被正常转发并录制。
- 不需要鉴权（与 tape 现状一致），但要在部署文档中提示「仅监听内网、勿暴露公网」。

### 8.2 absolute-form 下的 `Proxy-Connection`

OkHttp 走 HTTP 代理时可能带上 `Proxy-Connection: Keep-Alive`。需确认 tape 的请求解析对未知/逐跳头不报错；若出现 400 或转发异常，在入口处剥离该头即可。这一条属于联调验证项，不是设计假设。

### 8.3 录制范围收敛

部署时用 `--config` 限定业务域名，避免把系统升级检查、广告上报等噪音混进录制集。属于使用约定，不改代码。

### 8.4 可选：来源标记

若后续要区分「接口」「图片」「WebView」三类流量，可在录制记录里加来源字段。本期不做，先保证录全。

## 9. 失败模式与边界

| 场景 | 表现 | 处理 |
| --- | --- | --- |
| IP / 端口填错 | 连接被拒或 1.5s 超时 | 开关弹回 + 可执行原因，业务不受影响 |
| 电脑上 tape 没启动 | connection refused | 同上，文案提示启动 tape |
| 盒子与电脑不同网段 | 超时 | 状态行显示本机 IP，提示核对网段 |
| 多网卡导致预填错 | 预填到另一个网段 | 四段都可改；状态行显示本机 IP 供比对 |
| DHCP 换了地址 | 上次成功的地址失效 | 下次开启时健康检查失败，改最后一段即可 |
| 开关切换后仍走旧路由 | 录不到新流量 | `evictAll()`，见 5.3 |
| 运行中 tape 挂了 | 请求超时/失败 | 状态转「异常」+ 顶部提示，提供一键关闭；不自动关 |
| HTTPS 请求 | 直连，不经过 tape | 设计如此；若将来要录，需另做方案 |
| 第三方 SDK 走 https | 直连 | 设计如此，不影响其功能 |
| WebView 内用 Cookie 鉴权 | 前缀式把页面 origin 变成 tape 地址，H5 的 Cookie 作用域随之变化 | 属前缀式代价；原生接口的 Cookie 不受影响，见 12 |
| 大文件上传/下载 | tape 全量缓冲，内存上涨 | 避免经代理传大文件；必要时调 tape 缓冲策略 |
| targetSdk ≥ 28 且未放开明文 | `CLEARTEXT communication not permitted` | 测试变体 manifest 放开，见 5.8 |
| H5 在 JS 里 `fetch('/api/x')` | 请求打到 tape 根路径，tape 无法确定上游，返回 404/502 | H5 改用绝对 baseURL；属前缀式盲区 |
| 页面校验 origin | H5 功能异常 | H5 侧确认；必要时让 H5 放行 tape origin |

## 10. 备选方案与决策记录

| 方案 | 做法 | 放弃原因 | 何时重新考虑 |
| --- | --- | --- | --- |
| URL 改写 interceptor | 把请求 URL 的 scheme/host 换成 tape，路径拼前缀 | 改 Host/Path 会影响 Cookie 域、Referer、接口签名；路径拼接边界多 | 出现必须走 https 且无法用前缀式的场景 |
| 局域网 UDP 自动发现 | tape 广播，App 监听后自动填地址 | 需组播权限、多网卡下易选错；AP 隔离常失败；多一个攻击面 | 手填体验被证明不可接受时，作为隐藏入口 |
| 自定义 `Dns` | 把业务域名解析到 tape IP | 只能换 IP 不能换端口，tape 得占 80；还需 tape 按 Host 路由 | tape 能固定占 80 且支持 Host 路由时 |
| 固定中心机 | 全组共用一个 tape 地址 | 换人换网即失效；专网环境不可达 | 团队集中办公、网络互通时 |
| 系统代理 | 改盒子全局代理 | 盒子系统设置不可改、无 adb | 有系统权限的定制盒子 |
| DNS 劫持 | 把域名指到 tape | 需可控 DNS；https 与 pinning 不成立 | 有内网 DNS 控制权时 |
| `VpnService` | App 内起 VPN 接管全部流量 | 需自建 TCP 栈、包体大、盒子授权弹窗不稳定 | 需要连非 HTTP 流量时 |
| Gradle + ASM 插桩 | 编译期给所有 OkHttpClient 挂拦截器 | 构建复杂、对 OkHttp 版本敏感；第三方 SDK 仍不保证 | 需要语义级数据（接口名、耗时）时 |
| 客户端拦截器上报 | 拦截请求响应，POST 到接收端 | 漏 WebView/图片库/SDK；需改包；接收端同样受专网限制 | 作为语义补强通道，见 13.4 |

## 11. 验收清单

- [ ] 开关打开后，`tape record` 目录出现业务接口记录，method、URL、请求体、响应体、状态码齐全；
- [ ] 开关关闭后，请求直连，tape 无新增记录；
- [ ] IP 填错时，开关 2s 内弹回并给出可执行原因，业务界面不卡死；
- [ ] tape 未启动时，同上；
- [ ] 切换开关后立即生效，无需杀进程；
- [ ] 列表与详情页图片出现在录制数据中（Glide/Coil 已共用 client）；
- [ ] WebView 首屏、JS/CSS、XHR（含 POST）出现在录制数据中；
- [ ] 冷启动后配置仍在，状态行能自动反映连通性；
- [ ] 全程可用遥控器完成（上下左右 + 确定），无系统输入法依赖；
- [ ] 非 /24 网段下可手动改前三段并连通；
- [ ] 盒子重启、断网重连后仍可正常开启；
- [ ] release 包中设置页不可达，相关开关逻辑未编入。

## 12. 待确认问题

| 问题 | 影响 | 确认方式 |
| --- | --- | --- |
| H5 是否在 JS 里用根相对地址调接口 | 决定 WebView 覆盖率 | 看 H5 代码里 fetch/axios 的 baseURL |
| H5 是否校验 `location.origin` | 前缀式可能导致页面异常 | 与 H5 负责人确认 |
| App 内是否只有一个 OkHttpClient | 多个 client 会漏采 | 全局搜索 `OkHttpClient.Builder` |
| 是否有 `HttpURLConnection` 直连 | 该路径不受 ProxySelector 影响 | 全局搜索 |
| H5 是否依赖 Cookie 鉴权 | 前缀式下 H5 登录态可能异常 | 看 H5 请求头与 Cookie 写入域 |
| 图片库能否注入 client | 决定图片是否漏采 | 看 Glide/Coil 初始化代码 |
| targetSdk 与明文配置 | 决定是否需要改 manifest | 看 `build.gradle` |
| tape 端口是否可固定 | 决定是否需要端口输入框 | 部署约定；默认 8888 已够用 |

## 13. 后续可选项

### 13.1 健康检查展示更多信息

`/__tape/health` 返回 `mode` 与 `version` 后，状态行可显示 `● 已连接 · record · v0.2.0`，便于确认「连的是不是我以为的那个 tape」。

### 13.2 局域网自动发现

作为隐藏入口（连按五次版本号之类），用 UDP 广播发现同网段 tape，自动填 IP。不放进主流程：失败模式多，且手填成本已经很低。

### 13.3 `shouldInterceptRequest` 兜底

只对「JS 运行时拼根相对地址」这类漏采请求做兜底，能拿到 URL 与 GET 响应，但拿不到 POST body。作为 6 的补充，不作为主路径。

### 13.4 语义事件通道

原始提案（拦截器上报）的合理归宿：在业务 client 上加一个轻量拦截器，只上报「接口名、耗时、页面归属、异常」，不传 body。与 tape 的字节级录制互补，用于生成「哪个页面调了哪些接口」的索引。本期不做。

## 附录 A 术语

| 术语 | 含义 |
| --- | --- |
| absolute-form | 代理请求行里带完整 URL，如 `GET http://host/path HTTP/1.1` |
| 前缀式 | 把原始 URL 整体拼在 tape 地址后面，如 `http://tape:8888/https://host/path` |
| CONNECT | HTTPS 隧道方法，tape 不支持，故 https 不走代理 |
| ProxySelector | OkHttp 的每请求代理决策接口，可按 scheme/host 动态选择 |
| 逐跳头 | 只对单段连接有意义的头，如 `Proxy-Connection`、`Connection` |