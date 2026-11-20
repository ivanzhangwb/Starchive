# Starchive

Starchive 将 GitHub 星标、个人整理和选型对话保存在本机。桌面端和独立命令 `starchive` 共用 SQLite。可离线浏览和搜索资料库、编辑笔记、查看已缓存的 README 和恢复对话。

## 安装与构建

需要 Rust 1.90+、Node.js 20.19+ 或 22.12+、npm，以及当前平台的 Tauri 2 系统依赖。macOS 需要 Xcode 命令行工具。Windows 和 Linux 的依赖见 [Tauri 安装说明](https://v2.tauri.app/start/prerequisites/)。

```sh
cargo install --locked --path cli
npm --prefix frontend ci
npm --prefix desktop ci
npm --prefix desktop run dev
```

桌面开发命令会启动 Vite 和 Tauri 窗口。在仓库根目录运行下面的命令生成发布版。macOS 的应用和磁盘映像分别位于 `target/release/bundle/macos/Starchive.app` 和 `target/release/bundle/dmg/`。使用绝对路径设置 `CARGO_TARGET_DIR` 时，产物写入该目录下的 `release/bundle/`。

```sh
npm --prefix desktop run build
```

## 数据和账户

- 默认数据库位于操作系统的本机应用数据目录，文件名为 `Starchive/library.sqlite3`。macOS 上为 `~/Library/Application Support/Starchive/library.sqlite3`。设置 `STARCHIVE_DB_PATH` 可让两个入口使用同一个指定文件。CLI 也可用 `--db FILE` 单独指定路径。
- 桌面端「设置」会显示当前检测到的 GitHub 凭据来源与 pi 路径。离线打开资料库无需登录 GitHub 或安装 pi。
- 同步星标前，可在终端运行 `gh auth login --hostname github.com`，也可在桌面端「设置」粘贴个人访问令牌。保存的令牌存入操作系统的凭据存储，桌面端和 `starchive sync` 共用。凭据优先级依次为设置中保存的令牌、`GH_TOKEN`、`GITHUB_TOKEN`、`STARCHIVE_GITHUB_TOKEN`、GitHub CLI 中 `github.com` 的登录凭据。GitHub 细粒度令牌需要 Starring 读取权限。获取私有仓库 README 还需要该仓库的 Contents 读取权限。公开 README 和公开搜索使用匿名请求。
- 先安装 pi，在 pi 中配置模型并确认可用。桌面端依次查找设置中保存的可执行文件、`STARCHIVE_PI_PATH` 指定的可执行文件、`PATH` 和 Homebrew、npm、nvm、`~/.pi/agent/bin` 等常见安装位置。保存或清除路径后，下一条消息立即使用当前配置。从 macOS Finder 启动时也可使用自动检测或保存的路径。使用仅在终端中设置的环境变量时，请从同一终端启动桌面应用或运行 CLI。

## CLI

```sh
starchive sync
starchive status
starchive list --language Rust
starchive search '离线搜索' --tag desktop --status all
starchive show owner/repo
starchive show owner/repo --refresh-readme
starchive edit owner/repo --note '用于桌面离线索引' --tag desktop --tag search
starchive edit owner/repo --clear-tags
starchive --json status
```

`show` 和 `edit` 也接受 GitHub 仓库的数字 ID。`--note ''` 清空笔记。同步完整拉取星标后再更新资料库，保留笔记与标签。通过 `--status unstarred` 可查看已取消星标的条目。桌面窗口恢复焦点时会刷新资料。若另一窗口或 CLI 已修改草稿中的同一字段，桌面会保留草稿并提示核对当前值。

## 桌面对话

选择本地 pi，描述需求，然后点开候选核对资料。点击「加入对话上下文」可分别授权私有资料和个人笔记。Starchive 在本机为收藏排序，把候选资料和近期对话控制在 32 KiB 的模型请求内。移除授权后，历史回复仍可在本机查看，后续模型请求遵循当前授权。旧版缓存的 README 可离线查看，点「刷新 README」后才能作为新的模型依据。pi 的模型推理可能联网。

点击「扩展到 GitHub 搜索」后输入公开搜索词，每次确认向 GitHub 发出一次查询。已返回的公开候选保存在会话中。可以在 GitHub 打开仓库并加星，随后同步到本地资料库。

## 验证

```sh
cargo test --workspace --locked
cargo clippy --workspace --all-targets --locked -- -D warnings
cargo fmt --all -- --check
npm --prefix frontend run build
npm --prefix desktop run build
```
