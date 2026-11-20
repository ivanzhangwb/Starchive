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

桌面开发命令会启动 Vite 和 Tauri 窗口。生成安装包和发布版二进制可运行下面的命令。Tauri 将产物写入 `target/release/bundle/`。

```sh
npm --prefix desktop run build
```

## 数据和账户

- 默认数据库位于操作系统的本机应用数据目录，文件名为 `Starchive/library.sqlite3`。macOS 上为 `~/Library/Application Support/Starchive/library.sqlite3`。设置 `STARCHIVE_DB_PATH` 可让两个入口使用同一个指定文件。CLI 也可用 `--db FILE` 单独指定路径。
- 设置 `STARCHIVE_GITHUB_TOKEN` 后启动桌面端或运行 `starchive sync`。GitHub 细粒度令牌需要 Starring 读取权限。获取私有仓库 README 还需要该仓库的 Contents 读取权限。公开 README 和公开搜索使用匿名请求。
- 桌面端使用 `PATH` 上的 `pi`，也可以设置 `STARCHIVE_PI_PATH` 指向可执行文件。先在 pi 中配置模型并确认可用。使用终端设置环境变量时，从同一个终端运行 `npm --prefix desktop run dev`，或启动发布包里的 `Starchive.app/Contents/MacOS/starchive-desktop`（macOS），让应用继承配置。

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

`show` 和 `edit` 也接受 GitHub 仓库的数字 ID。`--note ''` 清空笔记。同步完整拉取星标后再更新资料库，保留笔记与标签。通过 `--status unstarred` 可查看已取消星标的条目。

## 桌面对话

选择本地 pi，描述需求，然后点开候选核对资料。点击「加入对话上下文」可分别授权私有资料和个人笔记。移除授权后，历史回复仍可在本机查看，后续模型请求遵循当前授权。pi 的模型推理可能联网。

点击「扩展到 GitHub 搜索」后输入公开搜索词，每次确认向 GitHub 发出一次查询。已返回的公开候选保存在会话中。可以在 GitHub 打开仓库并加星，随后同步到本地资料库。

## 验证

```sh
cargo test --workspace --locked
cargo clippy --workspace --all-targets --locked -- -D warnings
cargo fmt --all -- --check
npm --prefix frontend run build
npm --prefix desktop run build
```
