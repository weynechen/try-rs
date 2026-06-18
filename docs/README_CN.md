# try-rs

> English version: [../README.md](../README.md)

**try-rs** 是流行工具 [try](https://github.com/tobi/try) 的 Rust 移植版。它是一个命令行工具,帮助你轻松管理和切换用于实验的临时"沙盒"目录,让主项目保持整洁。

它具备快速的 TUI、模糊搜索、Git 集成以及多工作区管理能力。

## 特性

*   **⚡ 快速 TUI**:基于 `crossterm` 构建的交互式界面。
*   **🔍 模糊搜索**:快速找到已有实验,或新建实验。
*   **📅 日期后缀**:目录名会自动追加日期后缀(例如 `my-experiment-2025-12-16`),让实验随时间保持有序、易于区分。
*   **🌐 代理支持**:可通过 `--proxy` 选项或 `TRY_PROXY` 环境变量经由代理工具(如 `proxychains`)克隆仓库——在 GitHub 访问缓慢或受限时非常方便。
*   **📦 Git 集成**:轻松将仓库克隆到独立的、带日期的目录中。
*   **🗂️ 工作区管理**:使用 `try set` 在不同根目录(工作区)之间切换,当前目录会被优先展示。
*   **🪟 跨平台**:支持 Linux、macOS 和 Windows。自动检测当前 Shell,并输出对应的脚本(Bash/Zsh 或 PowerShell)。

## 安装

### 前置条件

需要在系统中安装 Rust 和 Cargo。

### 构建

```bash
git clone <this-repo-url>
cd try-rs
cargo build --release
```

构建产物位于 `./target/release/try`。

## Shell 集成(必需)

由于 `try` 需要改变 Shell 的当前目录(`cd`),它无法作为独立的二进制单独工作。你必须配置 Shell 来包裹它。

### Bash / Zsh(Linux、macOS、Git Bash)

将下面这行加入你的 Shell 配置文件(例如 `~/.bashrc`、`~/.zshrc`):

```bash
# 将 /path/to/try-rs 替换为你编译出的二进制的实际路径
eval "$(/path/to/try-rs/target/release/try init ~/experiments)"
```

*   `~/experiments` 是存放"实验"的默认目录,你可以改成任意喜欢的路径。

添加后,重启终端或运行 `source ~/.zshrc`。

### Windows(PowerShell 5.1+ / PowerShell 7)

在 Windows 上,`try` 会输出 PowerShell 原生脚本。将下面这行加入你的 PowerShell 配置文件(`$PROFILE`):

```powershell
# 将路径替换为你编译出的 try.exe 的实际路径
& 'C:\path\to\try-rs\target\release\try.exe' init --shell powershell 'C:\Users\you\experiments' | Out-String | Invoke-Expression
```

通过 `. $PROFILE` 重新加载(或重启终端)。推荐使用 Windows Terminal 以获得最佳 TUI 体验。

> 必须使用 `Out-String`:init 的输出是多行的,如果没有它,PowerShell 会把一个字符串*数组*传给 `Invoke-Expression`,从而报错 *"Cannot convert 'System.Object[]' to the type 'System.String'"*。固定 `--shell powershell` 还能避免在从导出了 `$SHELL` 的父 Shell(例如 Git Bash)启动 PowerShell 时发生误判。

> **PowerShell 中的命令是 `tr`,而不是 `try`。** `try` 是 PowerShell 的保留关键字(`try { } catch { }`),因此包裹函数不能命名为 `try`——输入 `try` 会被解析成 `try{}` 语句,根本到不了本工具。请改用 `tr`(例如 `tr`、`tr my-idea`、`tr clone <url>`)。若想用别的名字,给 `init` 传 `--name <cmd>`。

> Shell 会被自动检测。若要强制指定,可给 `init` 传 `--shell bash` 或 `--shell powershell`。init 包裹函数会导出 `TRY_SHELL`,因此后续所有调用都会自动输出正确 Shell 的脚本。

## 使用

### 基本导航

直接输入 `try` 打开交互式选择器:

```bash
try
```

*   **输入** 以过滤目录。
*   **上/下** 进行导航。
*   **回车** 切换到选中的目录。
*   **Delete** 标记目录待删除(支持批量删除)。
*   **Esc** 取消。

### 新建实验

输入一个不存在的名称,然后选择 "Create new":

```bash
try my-new-idea
```

这会创建 `~/experiments/my-new-idea-YYYY-MM-DD` 并 `cd` 进去。

### Git 克隆

将仓库克隆到一个全新的、带日期的目录中:

```bash
try clone https://github.com/user/repo.git
```

这会创建 `repo-YYYY-MM-DD` 并把源码克隆进去。

**代理支持**:如果你需要使用代理工具(如 `proxychains` 等)来克隆:

```bash
# 使用 --proxy 选项
try clone https://github.com/user/repo.git --proxy proxychains

# 或设置 TRY_PROXY 环境变量
export TRY_PROXY=proxychains
try clone https://github.com/user/repo.git
```

命令行选项的优先级高于环境变量。

### 工作区管理

`try-rs` 允许你管理多个用于实验的根目录(工作区)。

1.  **添加新工作区**:
    初始化一个新路径。这会把当前会话切换到该路径,并保存到历史记录。
    ```bash
    # 设置当前 TRY_PATH 并加入历史
    try init ~/other/projects
    ```

2.  **切换工作区**:
    使用 `set` 从之前用过的工作区中选择。
    ```bash
    try set
    ```
    这会打开一个 TUI,列出你的工作区历史,并将**当前工作目录置顶**。
    选择某个工作区后会:
    - 更新你的 `TRY_PATH` 环境变量
    - 切换到所选目录(`cd`)
    - 将其保存到工作区历史

## 配置

*   **历史记录**:工作区历史保存在 `~/.config/try/workspaces`(Linux/macOS)或 `%USERPROFILE%\.config\try\workspaces`(Windows)。
*   **环境变量**:本工具依赖 `TRY_PATH` 环境变量,由 Shell 包裹函数管理。

## 许可证

MIT
