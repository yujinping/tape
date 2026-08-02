# tape

A development & debugging toolbox for TV boxes / Android, written in pure Rust.

English | [简体中文](./README.md)

tape targets the **development and debugging** of TV boxes, large-screen and Android devices: one binary combines API recording / offline replay, live Android logcat viewing, and WebView console log ingestion with automatic log dumps.

- **Development (online)**: on a corporate intranet, tape records every HTTP request and response made by an app into snapshots and automatically downloads the static assets referenced by pages.
- **Debugging**: view Android device logs in real time with level / keyword filtering; every session is automatically dumped to a timestamped `.log` file for later review.
- **Offline replay**: serve the recorded snapshots from the same address, letting the app reproduce its full UI and API behavior with zero external and zero intranet traffic.

- **Pure HTTP**: traffic between the app and tape is plain HTTP/1.1 — no certificates to install, no root CA to trust. HTTPS upstreams are connected by tape as a TLS client, decrypted, and recorded.
- **Pure Rust**: built on tokio + hyper; cross-platform (Windows / macOS / Linux); ships as a single binary with no runtime dependencies.

## Table of Contents

- [Features](#features)
- [Use Cases](#use-cases)
- [Installation](#installation)
- [Quick Start](#quick-start)
- [Client Compatibility](#client-compatibility)
- [Response Rewriting](#response-rewriting)
- [Configuration](#configuration)
- [HTTPS Upstreams](#https-upstreams-record-mode)
- [Data Directory & Symlink Switching](#data-directory--symlink-switching)
- [Command-line Options](#command-line-options)
- [logcat: Live Android Logs with Auto-dump](#logcat-live-android-logs-with-auto-dump)
- [console: Box WebView Debug Log Ingestion](#console-box-webview-debug-log-ingestion)
- [app: Box App Network Log Ingestion](#app-box-app-network-log-ingestion)
- [Cross-platform Support](#cross-platform-support-windows--macos--linux)
- [Roadmap](#roadmap)
- [Development](#development)
- [Known Limitations](#known-limitations)

## Features

- **Same address for both modes**: `record` and `replay` share the default port `8888`, so the app's server address never changes between the two phases — just switch the tape mode.
- **Proxy-less access**: tape can be used as a plain server with the target URL prefixed to the request path (`http://<tape>:8888/http://www.example.com/...`); no system proxy is required, which fits TV boxes and other devices that cannot be configured with a proxy.
- **Client compatibility**: three request forms are recognized automatically — standard forward proxy (absolute-form), URL-prefixed (`/https://host/...`), and single-slash folded (`/https:/host/...`). Java (Retrofit / OkHttp) can use a prefixed address directly as `baseUrl`.
- **Response rewriting**: redirects and links in responses to prefixed requests are rewritten back to tape, so pages never jump to the public internet and never break in offline / intranet environments. gzip / deflate / br compressed responses are supported.
- **Shared config**: `record` and `replay` share a single TOML config file (recording filters, replay port, rewrite mode); CLI arguments take precedence.
- **Asset persistence**: static assets requested directly by the app or referenced in responses are downloaded and deduplicated by sha256; replay serves them by path.
- **Fidelity**: responses are passed through unchanged during recording (100% fidelity); snapshots store the original requests and responses, are human-editable, and take effect immediately on replay.
- **Device debug logs**: `tape logcat` is a pure CLI viewer for Android device logs (level/keyword filtering, colored terminal output) that automatically writes each session to a timestamped `.log` file — no extra log-collection tool needed.
- **Console log ingestion**: `tape console` runs an HTTP endpoint that accepts `console.log` etc. from box WebViews / web pages via GET / POST, with CORS support and instant dumps to `console-YYYYMMDD-HHMMSS.log`.
- **App network log ingestion**: `tape app` accepts in-app logs / network events pushed by boxes that cannot use logcat (GET / POST), dumping instantly to `app-YYYYMMDD-HHMMSS.log`; it shares the same ingestion component as `console`.

## Use Cases

tape mainly serves the development and debugging of TV-box / large-screen apps: record APIs, capture logs, and replay offline — switching modes is one command and the app's address never changes.

### 1. Box app API recording, offline replay at home

1. Run `tape record` on the corporate intranet, listening on port `8888`. Point the app's HTTP proxy at `192.168.0.100:8888` and exercise the business flows.
2. Snapshots and assets are written under `tape-api/`; copy the directory home.
3. Run `tape replay` at home, serving from the same address. The app's server address stays unchanged; the full UI and API behavior can be reproduced with zero network access.

### 2. Devices that cannot configure a system proxy (TV boxes / specific apps)

Some boxes or apps do not support system-level HTTP proxies but allow specifying a server address (e.g. a "base URL"). Point them at tape and prefix the request path with the full target URL:

```text
http://192.168.0.100:8888/http://www.example.com/api/v1/login
http://192.168.0.100:8888/https://www.example.com/api/v1/login
```

tape recognizes, forwards, records, and replays these requests automatically — no flags needed. Redirects and links in responses are rewritten back to tape, so pages never break in offline environments.

### 3. Java / Retrofit clients with a prefixed baseUrl

Android / Java apps using Retrofit + OkHttp can set the prefixed address as `baseUrl` directly (see [Client Compatibility](#client-compatibility)). Verified with Retrofit 2.x + OkHttp 3.x / 4.x; `@Path`, `@Query`, and query strings work normally.

## Installation

```bash
cargo install --path .
# tape is installed to ~/.cargo/bin/tape (already on PATH)
```

For cross-compilation or one-command builds, use [`build.sh`](./build.sh) — see [Cross-platform Support](#cross-platform-support-windows--macos--linux).

## Quick Start

### Phase 1: Record on the intranet

```bash
tape record            # listens on 8888 by default, writes data to ./tape-api
tape record --port 9999 --dir /path/to/dir
tape record --config my-filter.toml    # explicitly limit what gets recorded
```

Configure the HTTP proxy on the app / device to `<your-ip>:8888` and exercise the app's pages. tape **forwards all traffic** (so the app keeps working), but the **recording scope can be limited via the config file**:

- Without `--config`: tape reads `tape-config.toml` from the data directory (`./tape-api` or the `--dir` path); if the file does not exist, every proxied request is recorded.
- With `--config <file>`: only requests matching the rules are recorded; everything else is forwarded but not snapshotted or downloaded. A missing or invalid file aborts startup.
- Tip: a Wi-Fi system-level proxy pulls in traffic from every app; use the config file to scope recording to the business servers and avoid noise and path collisions.

### Phase 2: Replay offline

```bash
tape replay            # listens on 8888 by default (same as record), reads ./tape-api
tape replay --port 8090 --rewrite absolute --absolute-base http://192.168.1.100:8090/
```

Change the app's server address to `<your-ip>:8888` (or the configured port) — the same address used during recording. Replay matches snapshots by **method + path** (ignoring host / query), returns the original status code and the rewritten response, and serves static assets by path from `resources/`. Unmatched snapshots or assets return 404.

## Client Compatibility

The request parser recognizes the following three forms **automatically, with no flags**, and they can be mixed:

| Form | Example request line | Typical client |
| --- | --- | --- |
| Standard forward proxy (absolute-form) | `GET http://www.example.com/api HTTP/1.1` | Clients with system / app-level HTTP proxy support |
| URL-prefixed (double slash) | `GET /http://www.example.com/api HTTP/1.1` | Devices that cannot configure a proxy but can set a server address (boxes / apps) |
| URL-prefixed (single slash, auto-compatible) | `GET /https:/www.example.com/api HTTP/1.1` | Libraries that fold `//` after the scheme into a single slash |

### Standard forward proxy (absolute-form)

Configure the HTTP proxy on the app / device as `<tape-ip>:8888` and let the client send normal absolute-form requests. Both record and replay process the full target URL; responses are not rewritten, matching standard forward-proxy behavior.

### URL-prefixed access (proxy-less, recommended for TV boxes)

Prefix the request path with the full target URL (scheme, host, optional port):

```text
http://<tape-ip>:8888/http://www.example.com/api/v1/login
http://<tape-ip>:8888/https://www.example.com/api/v1/login
```

- `record`: extracts the target host and the real path from the URL, then forwards, filters, and records as a normal proxy; snapshots are stored under the real target URL.
- `replay`: strips the prefix and matches snapshots by method + path (origin is extracted from the prefix for exact matching); static assets work the same way.
- Both `http://` and `https://` targets are supported; `record` connects to HTTPS upstreams as a TLS client and decrypts them (no MITM).
- tape listens on `0.0.0.0` (all interfaces), so LAN devices can reach it via the IP of the machine running tape.
- For verification, prefer curl or the app (browsers percent-encode and normalize the prefix):

```bash
curl 'http://127.0.0.1:8888/https://www.example.com/api/v1/login'   # single quotes prevent shell expansion
```

### Java / Retrofit / OkHttp with a prefixed baseUrl

Android / Java clients can set the prefixed address as `baseUrl`. The following was verified with **Retrofit 2.9.0 + OkHttp 3.14.9 / 4.9.3**:

```java
Retrofit retrofit = new Retrofit.Builder()
        .baseUrl("http://192.168.0.100:8888/https://www.example.com/") // must end with /
        .build();

interface Api {
    // Relative path (no leading slash): request line GET /https://www.example.com/api/v1/login
    @GET("api/v1/login")
    Call<ResponseBody> login();

    // @Path / @Query / query strings work normally
    @GET("users/{id}/posts")
    Call<ResponseBody> userPosts(@Path("id") String id, @Query("page") int page);
}
```

**Verified behavior**

- The `baseUrl` must end with `/`: `.../https://www.example.com/` (the trailing slash is required). Otherwise Retrofit throws `IllegalArgumentException: baseUrl must end in /`.
- With relative interface paths (no leading slash), the final request line is `GET /https://www.example.com/api/v1/login`, which tape recognizes and forwards.
- `@Path`, `@Query`, and in-URL query strings (e.g. `@GET("api/v1/login?x=1")`) all compose correctly.
- OkHttp's `HttpUrl` does not fold the `//` in a path, so the `https://` prefix is preserved; if some other library folds it to a single slash (`/https:/host/...`), tape already handles that too.
- OkHttp 3.x and 4.x behave identically.

**Edge cases to avoid**

- **Leading slash**: `@GET("/api/v1/login")` resolves against the server root, dropping the prefix; the request becomes `GET /api/v1/login` and tape cannot recover the target host (the information is lost). Always use relative paths without a leading slash.
- **Full-URL bypass**: `@Url` with a full URL or `@GET("https://...")` bypasses the prefix and connects to the origin directly (not through tape). This is inherent Retrofit behavior — avoid it.
- **Host header**: tape rewrites the Host header to the upstream `host[:port]` when forwarding; the client's original Host (usually the tape address itself) is discarded to avoid upstream WAF / anti-SSRF rejections.

### Browser access

Browsers can open prefixed URLs to view recorded / replayed pages, but note:

- Browsers percent-encode the `:` in the prefix as `%3A` (some as `%20`); tape tolerates these encodings.
- Browsers also normalize case and slashes, which may change the request path; prefer curl or the app for debugging, and rely on the app for real usage.

## Response Rewriting

Responses to prefixed requests are rewritten so that subsequent redirects and asset requests keep going through tape:

- Absolute URLs: `https://host/path` → `http://<tape-ip>:8888/https://host/path` (applies to `Location` headers and text bodies).
- Protocol-relative: `//host/path` (common in HTML `src` / `href` and CSS `url()`; browsers resolve it against the page's scheme and would otherwise hit the public internet).
- Root-relative paths: HTML attribute `href="/assets/x.css"`, CSS `url(/fonts/x.woff2)` (browsers would resolve them as `http://<tape>/...`, losing the prefix).
- **XML namespaces / DTD identifiers** (e.g. `xmlns="http://www.w3.org/2000/svg"` and other w3.org URLs) are never rewritten, to avoid breaking SVG / XML rendering.
- Snapshots always store the original response, so recording fidelity is unaffected; absolute-form (standard proxy) requests do not get this rewriting.

**Compressed responses**: during recording, tape rewrites `Accept-Encoding` to `identity` so upstreams return plain text and HTML / JS / CSS can be rewritten. A gzip / deflate / br decompress → rewrite → recompress safety net also handles upstreams that ignore `identity` and historical compressed snapshots.

## Configuration

### Shared config file

`record` and `replay` share a single TOML config file. A ready-to-use sample with detailed comments is provided at [`tape-config.example.toml`](./tape-config.example.toml). Two ways to use it:

1. Copy it to the data directory as `tape-config.toml` (default `./tape-api/`, or the `--dir` path); both `record` and `replay` load it automatically without `--config`.
2. Place it anywhere and pass `tape record --config <file>` or `tape replay --config <file>` (an explicitly specified file must exist).

Precedence: **explicit CLI argument > config file > built-in default**. For example, `tape replay --port 9999` overrides the `port` in the config file.

### [record] recording filters

`include_hosts` and `include_hosts_regex` are combined with OR semantics; a hit means "record". Unmatched requests are still forwarded but neither snapshotted nor downloaded. With neither set, all traffic is recorded.

```toml
[record]
# Exact match: "host" matches any port; "host:port" matches exactly
include_hosts = ["10.1.2.3:8080", "api.company.com"]
# Regex match: case-insensitive against the full authority (host:port)
include_hosts_regex = [
  '^10\.1\.2\.(3|4):\d+$',
  '\.company\.com(:\d+)?$',
]
```

> Note: TOML literal strings (single quotes) do not process escapes, so `\.` is passed to the regex engine as-is (matching a literal dot); with double-quoted strings you must write `\\`. An invalid config file (TOML or regex error) aborts startup; an explicitly specified missing `--config` file also aborts startup.

### [replay] replay & rewriting

```toml
[replay]
port = 8888            # replay port, default 8888 (same as record)
rewrite = "relative"   # relative / absolute / none, default relative
absolute_base = "http://127.0.0.1:8888/"   # only used in absolute mode
```

- `rewrite`:
  - `relative` (default): rewrites absolute URLs pointing at the local address to relative paths; snapshots stay portable across machines, ports, and directories.
  - `absolute`: rewrites to the base URL given by `absolute_base`; suitable for clients that require absolute URLs.
  - `none`: returns the recorded response unchanged.
- `absolute_base`: only used when `rewrite = "absolute"`; should end with `/`; default `http://127.0.0.1:8888/`.
- tape listens on `0.0.0.0` in both modes, so LAN devices can reach it directly.

## HTTPS Upstreams (record mode)

- Both prefixed `/https://www.example.com/...` and absolute-form `https://www.example.com/...` are supported: tape connects to the upstream as a TLS client, decrypts, then forwards, filters, and records; the snapshot origin is stored as `https://host:port`.
- By default, upstream certificates are validated against the **system root certificates** (works when a corporate internal CA is installed).
- For self-signed certificates on private networks, set `TAPE_INSECURE_TLS=1` to skip certificate validation (intranet use only).
- HTTPS links in static assets (`resources/`) are also downloadable.

## Data Directory & Symlink Switching

The default data directory is `tape-api` under the current working directory (both commands accept `-d/--dir`). Symlinks are supported for switching between recorded datasets:

```bash
ln -s /path/to/recorded-A ./tape-api   # then just run tape replay
```

```text
tape-api/
├── session.json                  # session metadata (tool version, record time, origins, snapshot count)
├── snapshots/
│   └── <host_port>/              # one directory per upstream host:port
│       └── <seq>-<METHOD>-<hash>.json   # per-endpoint snapshot (full request/response, original data)
└── resources/
    ├── index.json                # asset index (hash → path mapping)
    ├── blobs/<sha256>            # deduplicated blobs
    └── <host_port>/<relative-path>     # hard-link copies preserving the original layout
```

**Asset persistence rules**

- Static assets (images / CSS / JS / fonts, judged by Content-Type) **directly requested by the app** → stored under `resources/<host_port>/<original-path>` and also snapshotted (base64).
- Asset links **referenced** in response bodies (JSON / HTML / CSS / JS) are extracted and downloaded automatically; both absolute URLs and root-relative paths are supported.
- Content is deduplicated by sha256 (`resources/blobs/<hash>`); the path entries are hard-link copies, and `index.json` records the mapping.

Snapshots are JSON: they can be edited or deleted by hand, and replay picks them up immediately (they are loaded at startup).

## Command-line Options

```text
tape record [--port 8888] [--dir ./tape-api] [--config tape-config.toml] [--rewrite-on-record] [-v]
tape replay [--port 8888] [--dir ./tape-api] [--config tape-config.toml]
                 [--rewrite relative|absolute|none] [--absolute-base http://127.0.0.1:8888/] [-v]
tape list [--dir ./tape-api]
tape logcat [-s SERIAL] [-l LEVEL] [--search KEYWORD] [--log-dir ./logs] [--no-color] [-v]
tape console [--port 8899] [--log-dir ./logs] [--no-color] [-v]
tape app [--port 8900] [--log-dir ./logs] [--no-color] [-v]
```

- `--config`: shared config file for record / replay. When omitted, tape reads `tape-config.toml` from the data directory; if missing, record captures everything and replay uses built-in defaults. When explicitly provided, the file must exist and be valid.
- `--port`: `8888` by default for both record and replay, keeping the app's address identical across the two phases.
- `tape list`: lists the cached sites in the data directory with per-site snapshot and asset counts (based on the `snapshots/` directory).
- `--rewrite-on-record`: also rewrite responses while recording (default off, to keep live sessions on the corporate network untouched).
- `tape logcat`: live Android logcat viewer with level/keyword filtering; each session is automatically saved to `{log-dir}/logcat-YYYYMMDD-HHMMSS.log` (plain text, no color). See [logcat](#logcat-live-android-logs-with-auto-dump) below.
- `tape console`: starts an HTTP endpoint that accepts debug logs pushed from box WebViews / web pages (GET / POST), automatically saved to `{log-dir}/console-YYYYMMDD-HHMMSS.log`. See [console](#console-box-webview-debug-log-ingestion) below.
- `tape app`: starts an HTTP endpoint that accepts in-app logs / network events pushed by boxes that cannot use logcat (GET / POST), automatically saved to `{log-dir}/app-YYYYMMDD-HHMMSS.log`. See [app](#app-box-app-network-log-ingestion) below.

## logcat: Live Android Logs with Auto-dump

`tape logcat` is a pure CLI subcommand ported from [rcat](https://github.com/soenkehahn/rcat) (MIT License): no GUI required, it streams Android device logs to the terminal with filtering and **automatically dumps each session to disk** for later review and archiving.

```bash
tape logcat                          # uses the first online device; writes ./logs/logcat-YYYYMMDD-HHMMSS.log
tape logcat -s emulator-5554 -l E --search crash   # only Error+ entries containing "crash"
tape logcat --log-dir /tmp/logs --no-color > logcat.txt   # redirect to a file (color auto-disabled when piped)
```

- **Device selection**: defaults to the first online device from `adb devices`; use `-s/--serial` with multiple devices.
- **Level filter**: `-l/--level` is the minimum level (V/D/I/W/E/F, default V = all); `--search` matches tag / message / pid / tid, case-insensitive.
- **Auto-dump**: every run creates a timestamped `logcat-YYYYMMDD-HHMMSS.log` (local time) with plain text (no ANSI colors); if writing fails, tape degrades to terminal-only output and warns.
- **Dump naming convention**: log sources are distinguished by prefix and share one `--log-dir` — `logcat-` (device logs), `console-` (WebView console pushes), `app-` (box app network logs), all as `{prefix}-YYYYMMDD-HHMMSS.log`.
- **Stop**: Ctrl-C stops gracefully and prints the saved log path.
- **Prerequisite**: adb must be installed and on PATH (`adb devices` must list the device); tape reads via `adb -s <serial> logcat -v threadtime`.
- **Privacy**: logs are read locally through adb and never uploaded; files stay under `--log-dir`.

## console: Box WebView Debug Log Ingestion

`tape console` runs an HTTP endpoint that accepts debug logs pushed from WebViews / web pages inside the box, with the same **instant dump** experience as `logcat`: `{log-dir}/console-YYYYMMDD-HHMMSS.log`. Responses carry CORS headers, so cross-origin fetch / XHR from web pages works without certificates or a proxy.

```bash
tape console                          # listens on 0.0.0.0:8899; writes ./logs/console-YYYYMMDD-HHMMSS.log
tape console --port 9000 --log-dir /tmp/logs
```

**Push protocol** (plain `fetch` / `XMLHttpRequest` from the web page, no SDK needed):

```js
// GET style (simple; suitable for <script> beacon reporting)
fetch('http://<tape-ip>:8899/?msg=' + encodeURIComponent('user clicked login') + '&level=info&tag=login')

// POST JSON (recommended; carries url / line for locating the issue)
fetch('http://<tape-ip>:8899/', {
  method: 'POST',
  headers: { 'Content-Type': 'application/json' },
  body: JSON.stringify({
    level: 'warn',          // log / debug / info / warn / error
    message: 'API timeout', // or the short alias msg
    tag: 'page',            // optional, source label
    url: location.href,     // optional, combined with line into the tag
    line: 12                // optional, code line number
  })
});
```

- **GET**: `/?msg=...&level=...&tag=...&url=...&line=...`; `msg` is required (URL-encoded); multi-line messages (`%0A`) are dumped line by line.
- **POST `text/plain`**: the whole body is one log entry; multiple lines are recorded separately (default level `log`).
- **POST `application/x-www-form-urlencoded`**: same parameters as GET.
- **POST `application/json`**: a single object or an array of objects with `level` / `message` (or `msg`) / `tag` / `url` / `line`; invalid JSON is recorded as an `[error]` entry.
- **CORS**: every response includes `Access-Control-Allow-Origin: *`; OPTIONS preflight returns 204.
- **Terminal output**: printed live with level colors; color is auto-disabled when piped (`--no-color` to force).
- **Stop**: Ctrl-C stops gracefully and prints the saved log path.

## app: Box App Network Log Ingestion

`tape app` accepts in-app logs / network events pushed by boxes that **cannot use logcat** (e.g. no adb on the firmware, or the app wants to report directly), dumping instantly to `{log-dir}/app-YYYYMMDD-HHMMSS.log`. It shares the same HTTP ingestion component as `console` — **the push protocol is identical** (GET / POST / JSON / form / plain text, see [console](#console-box-webview-debug-log-ingestion) above); only the listening port and dump prefix differ:

```bash
tape app                              # listens on 0.0.0.0:8900; writes ./logs/app-YYYYMMDD-HHMMSS.log
tape app --port 9100 --log-dir /tmp/logs
```

In-app reporting example (OkHttp / HttpURLConnection both work; CORS headers included for web pages):

```text
POST http://<tape-ip>:8900/   Content-Type: application/json
{"level":"error","message":"token expired","tag":"auth","url":"","line":0}
```

- **Port plan**: `record` / `replay` = `8888`, `console` = `8899`, `app` = `8900` — no conflicts, all can run at once.
- GET query, POST plain text / form / JSON, level-colored terminal output, and graceful Ctrl-C stop with the saved path all work the same way.

## Cross-platform Support (Windows / macOS / Linux)

- The code is fully cross-platform (tokio + hyper, no platform-specific dependencies); snapshot / asset directories can be copied and reused across platforms.
- **One-command builds**: `./build.sh` (current OS), `./build.sh mac` (macOS), `./build.sh win` (Windows x64 cross-compilation); artifacts go to `dist/`.
  - Windows x64 cross-compilation requires mingw: on macOS, `brew install mingw-w64`.
  - Size optimization: release builds enable LTO / panic=abort / strip (no runtime cost, roughly halves each binary); the `win` build additionally runs UPX (`UPX=0` to skip), shrinking the Windows binary from ~10 MB to ~1.2 MB. Note: UPX-packed executables may be flagged by some antivirus software; use `UPX=0 ./build.sh win` if that happens.
- **Windows local build** (MSVC): install Rust and run `cargo build --release`; the artifact is `target\release\tape.exe`.
- **CI builds**: [`.github/workflows/ci.yml`](./.github/workflows/ci.yml) runs fmt / clippy / test on Windows, macOS, and Linux; pushing a tag builds release artifacts for all three platforms and uploads them as artifacts.
- Windows compatibility details: asset copy filenames are sanitized (invalid characters replaced, trailing dots / spaces trimmed, reserved device names such as CON / NUL / COM1 prefixed with `_`); content-hash deduplication is unaffected, and replay still matches by the original path.

## Releasing a New Version

Edit the "本版本变更" (Changes in this release) section of `RELEASE_NOTES.md` first, then run the one-command script:

```bash
./release.sh 0.1.2            # or ./release.sh v0.1.2
./release.sh --dry-run 0.1.2  # preview what would be done
```

The script bumps the version (`Cargo.toml` / `Cargo.lock`), regenerates `CHANGELOG.md` with git-cliff, commits, tags, and pushes `main` plus the tag. Pushing the tag triggers CI to build the three platform binaries, create the Release (body from `RELEASE_NOTES.md`), and attach the assets — no manual web steps.

## Roadmap

tape keeps expanding around TV-box development / debugging. Planned features:

- **WebView console log ingestion** ✅ done (`tape console`): box WebViews / web pages push `console.log` etc. via GET / POST for unified viewing and dumping.
- **Instant console log dumps** ✅ done: pushes are written immediately to `console-YYYYMMDD-HHMMSS.log`.
- **Box app network logs** ✅ done (`tape app`): boxes that cannot use logcat push their in-app logs / network events to tape via GET / POST, dumped as `app-YYYYMMDD-HHMMSS.log`.
- More box-debugging helpers will keep being added (feature requests welcome via issues).

## Development

```bash
cargo test      # unit + integration tests (local ports, no external network)
cargo build --release
```

## Known Limitations

- The link between the app and tape is plain HTTP/1.1 only (tape does not serve HTTPS, so the app needs no certificates); HTTPS upstreams are supported, but **CONNECT tunneling / MITM of plain HTTPS** is not (it would require a CA hierarchy and device trust).
- Response bodies are buffered before being written out (fine for APIs and static assets; streaming for very large responses is not implemented yet).
- If different upstreams share the same relative asset path, replay resolves by index order with origin-exact matching preferred; use `--config` filters during recording to narrow the scope.
- Asset extraction covers root-relative paths (`/static/xxx` etc.); `../`-style relative references rely on snapshot fallback (they are recorded as requests when the page renders).
