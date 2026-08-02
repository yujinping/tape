# CHANGELOG

本文件由 [git-cliff](https://git-cliff.org) 依据 Conventional Commits 自动生成，请勿手动编辑。
## [unreleased](https://github.com/yujinping/tape/compare/vv0.1.2...HEAD)

### 🐛 修复

- 修复代码审查问题（重放 query 匹配、资源串站、重复录制覆盖等 15 项）

### 📚 文档

- 新增代码审查报告，README 补充 --bind/安全/内存限制说明
- README 定位调整为盒子开发调试工具箱，同步 CLI 帮助文案

### 🚀 新功能

- 新增 app 子命令接收盒子应用网络日志，抽取通用 ingest 组件供 console/app 复用
- 新增 console 子命令，接收盒子 WebView/网页 GET/POST 调试日志并自动落盘
- 新增 logcat 子命令（rcat CLI 移植，自动落盘时间戳日志）

### 🚜 重构

- 抽取 log_file 公共落盘模块（logcat-/console-/app- 前缀命名规范）
- 提取 net 模块承载 HttpClient，消除 record↔download 循环依赖
## [0.1.2](https://github.com/yujinping/tape/releases/tag/v0.1.2) - 2026-08-01

### 🐛 修复

- *(ci)* Publish job 补充 checkout，修复 Release 正文为空（body_path 读不到文件）

### 💼 其他

- Release.sh 允许 RELEASE_NOTES.md 随发布一起提交
- 新增一键发版脚本 release.sh（bump/CHANGELOG/tag/推送）

### 📚 文档

- README 补充一键发版流程说明
- 引入 git-cliff 自动生成 CHANGELOG

### 🔧 其他

- Release v0.1.2
## [0.1.1](https://github.com/yujinping/tape/releases/tag/v0.1.1) - 2026-08-01

### 🔧 其他

- Release v0.1.1
- 移除误提交的发布介绍存档（不纳入版本控制）
- Release 正文自动使用中文发布介绍模板（RELEASE_NOTES.md）

### 🚀 新功能

- 新增 tape list 子命令，列出缓存站点与接口/资源数量
## [0.1.0](https://github.com/yujinping/tape/releases/tag/v0.1.0) - 2026-08-01

### 🎨 代码风格

- Rustfmt 格式化代码（修复 CI fmt 检查失败）

### 🐛 修复

- *(ci)* Publish 改用官方 download-artifact 匹配三平台产物并合并

### 🔧 其他

- 打 tag 时自动构建三平台产物并发布到 GitHub Releases
- *(docs)* 清理过时需求文档并更新忽略规则

### 🚀 新功能

- 专网录制/离线重放代理（客户端兼容、样例配置、中英文README）
- 跨平台支持与一键构建（build.sh/CI/Windows 交叉编译/UPX 压缩）
- 专网HTTP接口录制与离线重放代理工具（record/replay）
