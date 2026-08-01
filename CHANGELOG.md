# CHANGELOG

本文件由 [git-cliff](https://git-cliff.org) 依据 Conventional Commits 自动生成，请勿手动编辑。
## [unreleased](https://github.com/yujinping/tape/compare/vv0.1.1...HEAD)

### 💼 其他

- Release.sh 允许 RELEASE_NOTES.md 随发布一起提交
- 新增一键发版脚本 release.sh（bump/CHANGELOG/tag/推送）

### 📚 文档

- README 补充一键发版流程说明
- 引入 git-cliff 自动生成 CHANGELOG
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
