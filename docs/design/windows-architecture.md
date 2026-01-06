# Windows 架构重构设计文档

## 1. 设计原则

### 1.1 核心原则

- **SOLID 原则**: 单一职责、开闭原则、里氏替换、接口隔离、依赖反转
- **Clean Architecture**: 分层架构，核心逻辑与平台实现解耦
- **最小依赖**: 仅支持 Windows 10+ + PowerShell 5.1+ + Windows Terminal

### 1.2 设计目标

- 核心业务逻辑与 Shell 集成层完全解耦
- 使用接口抽象实现依赖反转
- 便于未来扩展支持其他平台/Shell
- 保持代码的可测试性

---

## 2. 目录结构设计

```
src/
├── lib.rs              # 库入口，导出公共接口
├── core/               # 核心业务逻辑 (平台无关)
│   ├── mod.rs
│   ├── selector.rs     # TUI 选择器
│   ├── workspace.rs    # 工作区管理
│   ├── scorer.rs       # 模糊搜索算法
│   └── models.rs       # 数据模型
├── shell/              # Shell 集成抽象层
│   ├── mod.rs
│   ├── trait.rs        # ShellIntegration trait
│   └── script.rs       # ScriptGenerator trait
├── platform/           # 平台具体实现
│   ├── mod.rs
│   └── powershell.rs   # PowerShell 实现
├── cli.rs              # CLI 参数解析
└── main.rs             # 程序入口
```

### 2.1 模块职责说明

| 模块 | 职责 | 平台相关 |
|------|------|----------|
| `core/` | 核心业务逻辑 | ❌ 无关 |
| `shell/` | Shell 集成抽象 | ❌ 抽象 |
| `platform/` | 平台具体实现 | ✅ 相关 |
| `cli.rs` | CLI 解析 | ❌ 无关 |
| `main.rs` | 依赖注入与组装 | ⚠️ 组装层 |

---

## 3. 核心接口设计

### 3.1 Shell 集成接口 (`shell/trait.rs`)

```rust
use std::path::PathBuf;
use anyhow::Result;

/// Shell 集成抽象 - 定义如何与 Shell 交互
pub trait ShellIntegration: Send + Sync {
    /// 生成初始化脚本，输出到 stdout 供用户 eval
    fn generate_init_script(&self, default_path: &PathBuf) -> String;

    /// 获取当前工作目录
    fn current_dir(&self) -> Result<PathBuf>;

    /// 获取配置文件路径
    fn config_path(&self) -> Result<PathBuf>;

    /// 检测 Shell 类型
    fn shell_type(&self) -> ShellType;
}
```

### 3.2 脚本生成器接口 (`shell/script.rs`)

```rust
use std::path::PathBuf;
use anyhow::Result;

/// 脚本生成器抽象 - 定义如何生成 Shell 命令
pub trait ScriptGenerator: Send + Sync {
    /// 生成 cd 命令
    fn generate_cd(&self, path: &PathBuf) -> String;

    /// 生成 mkdir + cd 命令
    fn generate_mkdir_cd(&self, path: &PathBuf) -> Result<String>;

    /// 生成设置环境变量命令
    fn generate_set_env(&self, key: &str, value: &str) -> String;

    /// 生成组合命令序列
    fn generate_sequence(&self, commands: Vec<String>) -> String;

    /// 生成 git clone 命令
    fn generate_git_clone(&self, url: &str, dest: &PathBuf, proxy: Option<&str>) -> String;

    /// 转义路径
    fn escape_path(&self, path: &PathBuf) -> String;
}

/// Shell 类型枚举
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ShellType {
    Bash,
    Zsh,
    PowerShell,
}
```

### 3.3 用户操作接口 (`core/models.rs`)

```rust
use std::path::PathBuf;

/// 用户选择后返回的操作 (平台无关)
#[derive(Debug, Clone)]
pub enum UserAction {
    /// 切换到已有目录
    Cd(PathBuf),

    /// 创建并切换到新目录
    MkdirCd(PathBuf),

    /// 设置工作空间并切换
    SetWorkspace(PathBuf),

    /// Git 克隆
    Clone {
        url: String,
        dest: PathBuf,
        proxy: Option<String>,
    },
}

/// 选择器模式
#[derive(Debug, Clone)]
pub enum SelectorMode {
    /// 扫描工作区目录
    Scan(PathBuf),

    /// 从历史记录中选择
    History(Vec<PathBuf>),
}

/// 目录条目
#[derive(Debug, Clone)]
pub struct Entry {
    pub basename: String,
    pub basename_down: String,
    pub path: PathBuf,
    pub mtime: std::time::SystemTime,
}
```

---

## 4. 核心模块设计

### 4.1 `core/selector.rs` - TUI 选择器

```rust
use crossterm::{
    cursor,
    event::{Event, KeyCode, KeyModifiers},
    style::{Attribute, Color, Print, SetAttribute, SetForegroundColor},
    terminal::{Clear, ClearType},
    QueueableCommand, ExecutableCommand,
};
use std::io::Stderr;

/// 交互式目录选择器 (平台无关)
pub struct Selector {
    mode: SelectorMode,
    workspace_path: PathBuf,
    input_buffer: String,
    cursor_pos: usize,
    scroll_offset: usize,
    entries: Vec<Entry>,
    marked_for_deletion: Vec<PathBuf>,
    delete_mode: bool,
    width: u16,
    height: u16,
}

impl Selector {
    pub fn new(mode: SelectorMode, query: String, workspace_path: PathBuf) -> Self;

    /// 运行选择器，返回用户操作
    pub fn run(&mut self) -> Result<Option<UserAction>>;

    /// 渲染 UI
    fn render(&self, stderr: &mut Stderr) -> Result<()>;

    /// 处理用户输入
    fn handle_input(&mut self) -> Result<Option<UserAction>>;

    /// 加载条目
    fn load_entries(&mut self) -> Result<()>;

    /// 刷新分数
    fn refresh_scores(&mut self);

    /// 处理选择
    fn handle_selection(&self) -> Option<UserAction>;
}
```

**职责**:
- 处理 TUI 渲染和交互
- 管理目录列表和过滤
- 返回平台无关的 `UserAction`
- 不依赖任何 Shell 特定逻辑

### 4.2 `core/workspace.rs` - 工作区管理

```rust
/// 工作区历史管理器 (平台无关)
pub struct WorkspaceManager {
    config_path: PathBuf,
}

impl WorkspaceManager {
    pub fn new(config_path: PathBuf) -> Self;

    /// 添加工作区到历史
    pub fn add_workspace(&self, path: &PathBuf) -> Result<()>;

    /// 获取工作区历史列表
    pub fn get_workspaces(&self) -> Result<Vec<PathBuf>>;
}
```

**职责**:
- 管理工作区配置文件读写
- 维护工作区历史记录
- 配置路径由外部传入，平台无关

### 4.3 `core/scorer.rs` - 模糊搜索算法

```rust
/// 模糊搜索评分器 (纯函数)
pub struct Scorer;

impl Scorer {
    /// 计算目录名与查询的匹配分数
    pub fn calculate_score(entry: &Entry, query: &str, now: std::time::SystemTime) -> f64;
}
```

**职责**:
- 实现模糊搜索算法
- 计算匹配分数和排序
- 无状态，纯函数

---

## 5. PowerShell 平台实现

### 5.1 `platform/powershell.rs` - PowerShell 集成

```rust
use crate::shell::{ShellIntegration, ScriptGenerator, ShellType};
use crate::core::models::SelectorMode;
use anyhow::{Context, Result};
use directories::ProjectDirs;
use std::path::PathBuf;

/// PowerShell Shell 集成实现
pub struct PowerShellIntegration;

impl ShellIntegration for PowerShellIntegration {
    fn generate_init_script(&self, default_path: &PathBuf) -> String {
        format!(
            r#"
function try {{
    param([Parameter(ValueFromRemainingArguments)]$Args)

    $exe = "{}"
    $output = & $exe $Args 2>&1

    if ($LASTEXITCODE -eq 0) {{
        Invoke-Expression $output
    }}
}}

$env:TRY_PATH = "{}"
"#,
            env::current_exe()
                .unwrap_or_else(|_| PathBuf::from("try"))
                .display(),
            default_path.display()
        )
    }

    fn current_dir(&self) -> Result<PathBuf> {
        std::env::current_dir().map_err(Into::into)
    }

    fn config_path(&self) -> Result<PathBuf> {
        ProjectDirs::from("com", "try-rs", "try")
            .map(|p| p.config_dir().join("workspaces"))
            .context("Failed to determine config path")
    }

    fn shell_type(&self) -> ShellType {
        ShellType::PowerShell
    }
}

/// PowerShell 脚本生成器
pub struct PowerShellGenerator;

impl ScriptGenerator for PowerShellGenerator {
    fn generate_cd(&self, path: &PathBuf) -> String {
        format!("Set-Location '{}'", self.escape_path(path))
    }

    fn generate_mkdir_cd(&self, path: &PathBuf) -> Result<String> {
        Ok(format!(
            "New-Item -ItemType Directory -Path '{}' -Force | Out-Null; {}",
            self.escape_path(path),
            self.generate_cd(path)
        ))
    }

    fn generate_set_env(&self, key: &str, value: &str) -> String {
        format!("$env:{} = '{}'", key, value)
    }

    fn generate_sequence(&self, commands: Vec<String>) -> String {
        commands.join("; ")
    }

    fn generate_git_clone(&self, url: &str, dest: &PathBuf, proxy: Option<&str>) -> String {
        let clone_cmd = if let Some(proxy_tool) = proxy {
            format!(
                "{} git clone '{}' '{}'",
                proxy_tool,
                url,
                self.escape_path(dest)
            )
        } else {
            format!("git clone '{}' '{}'", url, self.escape_path(dest))
        };

        self.generate_sequence(vec![
            format!(
                "New-Item -ItemType Directory -Path '{}' -Force | Out-Null",
                self.escape_path(dest)
            ),
            format!("Write-Host 'Cloning {}...'", url),
            clone_cmd,
            self.generate_cd(dest),
        ])
    }

    fn escape_path(&self, path: &PathBuf) -> String {
        // Windows 路径转义：反斜杠替换为正斜杠，单引号转义为 ''
        let path_str = path.display().to_string();
        path_str.replace('\\', "/").replace("'", "''")
    }
}
```

---

## 6. 依赖关系图

```
┌─────────────────────────────────────────────────────┐
│                     main.rs                          │
│              (依赖注入 / 组装层)                      │
└────────────┬────────────────────────────────────────┘
             │
    ┌────────▼─────────┐
    │     cli.rs       │
    │  (CLI 解析)      │
    └────────┬─────────┘
             │
     ┌───────┴──────────────────┐
     │                          │
┌────▼────────┐         ┌──────▼───────┐
│ core/       │         │ shell/       │
│ selector.rs │───────▶│ trait.rs     │
│ workspace.rs│         │ script.rs    │
│ scorer.rs   │         │              │
└─────────────┘         └──────┬───────┘
                              │
                    ┌─────────▼──────────┐
                    │  platform/         │
                    │  powershell.rs     │
                    │  (实现 Shell 集成)  │
                    └────────────────────┘
```

### 6.1 依赖反转体现

- `core/` 层依赖 `shell/trait.rs` (抽象接口)
- `platform/powershell.rs` 依赖 `shell/trait.rs` (抽象接口)
- `main.rs` 负责组装具体实现，注入到 `core/` 层
- **高层模块不依赖低层模块，两者都依赖抽象**

### 6.2 模块间依赖表

| 模块 | 依赖 | 输出 |
|------|------|------|
| `core/models.rs` | 无 | `UserAction`, `Entry`, `SelectorMode` |
| `core/scorer.rs` | `models` | `Scorer` |
| `core/workspace.rs` | `models` | `WorkspaceManager` |
| `core/selector.rs` | `models`, `scorer` | `Selector` |
| `shell/trait.rs` | `models` | `ShellIntegration`, `ScriptGenerator` |
| `platform/powershell.rs` | `shell/trait` | `PowerShellIntegration`, `PowerShellGenerator` |
| `cli.rs` | `core/models`, `shell/trait` | `Cli`, `Commands` |
| `main.rs` | 以上全部 | 入口点 |

---

## 7. 核心流程设计

### 7.1 主程序流程

```rust
use clap::{Parser, Subcommand};
use crate::cli::{Cli, Commands};
use crate::core::{Selector, SelectorMode, WorkspaceManager};
use crate::platform::powershell::{PowerShellIntegration, PowerShellGenerator};

fn main() -> Result<()> {
    // 1. 依赖注入
    let shell = PowerShellIntegration;
    let generator = PowerShellGenerator;
    let workspace_mgr = WorkspaceManager::new(shell.config_path()?);

    // 2. CLI 解析
    let cli = Cli::parse();

    // 3. 路径解析
    let base_path = resolve_base_path(&shell);

    // 4. 命令分发
    match cli.command {
        Some(Commands::Init { path }) => {
            // 输出初始化脚本
            println!("{}", shell.generate_init_script(&path));
        }
        Some(Commands::Set) => {
            // 运行工作区选择器
            let workspaces = workspace_mgr.get_workspaces()?;
            run_workspace_selector(shell, generator, workspaces, base_path)?;
        }
        None => {
            // 运行交互式选择器
            run_interactive(shell, generator, base_path, cli.query)?;
        }
        // ... 其他命令
    }

    Ok(())
}

fn resolve_base_path(shell: &impl ShellIntegration) -> PathBuf {
    std::env::var("TRY_PATH")
        .map(PathBuf::from)
        .unwrap_or_else(|_| dirs::home_dir().unwrap().join("experiments"))
}
```

### 7.2 交互式选择器流程

```rust
use crate::core::models::{UserAction, SelectorMode};
use crate::shell::{ShellIntegration, ScriptGenerator};

fn run_interactive(
    shell: impl ShellIntegration,
    generator: impl ScriptGenerator,
    base_path: PathBuf,
    query: Option<String>,
) -> Result<()> {
    // 1. 创建选择器 (平台无关)
    let mut selector = Selector::new(
        SelectorMode::Scan(base_path.clone()),
        query.unwrap_or_default(),
        base_path,
    );

    // 2. 运行选择器
    if let Some(action) = selector.run()? {
        // 3. 将平台无关的 UserAction 转换为具体脚本
        let script = match action {
            UserAction::Cd(path) => generator.generate_sequence(vec![
                generator.generate_cd(&path),
            ]),
            UserAction::MkdirCd(path) => generator.generate_mkdir_cd(&path)?,
            UserAction::SetWorkspace(path) => {
                generator.generate_sequence(vec![
                    generator.generate_set_env("TRY_PATH", &path.display().to_string()),
                    generator.generate_cd(&path),
                ])
            }
            UserAction::Clone { url, dest, proxy } => {
                generator.generate_git_clone(&url, &dest, proxy.as_deref())
            }
        };

        // 4. 输出脚本给 Shell eval
        println!("{}", script);
    } else {
        std::process::exit(1); // 用户取消
    }

    Ok(())
}
```

### 7.3 工作区选择流程

```rust
fn run_workspace_selector(
    shell: impl ShellIntegration,
    generator: impl ScriptGenerator,
    mut workspaces: Vec<PathBuf>,
    base_path: PathBuf,
) -> Result<()> {
    // 将当前工作目录插入到历史最前面
    if let Ok(cwd) = shell.current_dir() {
        workspaces.retain(|p| p != &cwd);
        workspaces.insert(0, cwd);
    }

    // 创建选择器 (History 模式)
    let mut selector = Selector::new(
        SelectorMode::History(workspaces),
        String::new(),
        base_path,
    );

    // 运行选择器
    if let Some(action) = selector.run()? {
        match action {
            UserAction::SetWorkspace(path) => {
                // 保存到历史
                let _ = WorkspaceManager::new(shell.config_path()?)
                    .add_workspace(&path);

                // 输出脚本
                let script = generator.generate_sequence(vec![
                    generator.generate_set_env("TRY_PATH", &path.display().to_string()),
                    generator.generate_cd(&path),
                ]);
                println!("{}", script);
            }
            _ => {}
        }
    } else {
        std::process::exit(1);
    }

    Ok(())
}
```

---

## 8. Windows 特殊处理清单

| 问题 | 处理位置 | 方案 |
|------|----------|------|
| 路径分隔符 | `PowerShellGenerator::escape_path` | 统一转为 `/` |
| 路径转义 | `PowerShellGenerator::escape_path` | `'` → `''` |
| 环境变量 | `PowerShellGenerator::generate_set_env` | `$env:KEY = 'value'` |
| 命令分隔符 | `PowerShellGenerator::generate_sequence` | `;` |
| 命令输出重定向 | `main.rs` | `2>$null` 替代 `2>/dev/null` |
| 切换目录命令 | `PowerShellGenerator::generate_cd` | `Set-Location` |
| 创建目录命令 | `PowerShellGenerator::generate_mkdir_cd` | `New-Item -ItemType Directory` |

---

## 9. 扩展性设计

### 9.1 未来支持其他 Shell

如需支持其他平台，只需：

1. 创建新文件实现接口
2. 在 `main.rs` 中添加 Shell 类型检测
3. 使用 `Box<dyn ShellIntegration>` 运行时多态或泛型编译期多态

### 9.2 运行时多态示例

```rust
fn get_shell_integration() -> Box<dyn ShellIntegration> {
    if cfg!(windows) {
        Box::new(PowerShellIntegration)
    } else if cfg!(target_os = "macos") {
        Box::new(ZshIntegration)
    } else {
        Box::new(BashIntegration)
    }
}

fn main() -> Result<()> {
    let shell = get_shell_integration();
    let generator = get_script_generator(shell.shell_type());
    // ...
}
```

### 9.3 编译期多态示例

```rust
fn run_interactive<S, G>(shell: S, generator: G, ...) -> Result<()>
where
    S: ShellIntegration,
    G: ScriptGenerator,
{
    // ...
}

fn main() -> Result<()> {
    #[cfg(windows)]
    {
        let shell = PowerShellIntegration;
        let generator = PowerShellGenerator;
        run_interactive(shell, generator, ...)?;
    }
    // ...
}
```

---

## 10. 开发优先级

| 优先级 | 任务 | 难度 |
|--------|------|------|
| P0 | 定义接口 (`shell/trait.rs`, `shell/script.rs`) | 低 |
| P0 | 提取核心模块 (`core/`) | 中 |
| P0 | 实现 PowerShell 平台层 (`platform/powershell.rs`) | 中 |
| P1 | 重构 `main.rs` 进行依赖注入 | 中 |
| P1 | Windows 终端测试 | 中 |
| P2 | 单元测试 | 低 |

---

## 11. 测试策略

### 11.1 单元测试

- `core/scorer.rs`: 纯函数，易于测试
- `core/workspace.rs`: 使用临时文件进行测试
- `platform/powershell.rs`: 测试脚本生成逻辑

### 11.2 集成测试

- 完整的交互式选择流程
- PowerShell 集成脚本执行
- Git 操作集成

### 11.3 必须测试的场景

1. ✅ PowerShell 5.1 (Windows 10 默认)
2. ✅ PowerShell 7+ (推荐)
3. ✅ Windows Terminal
4. ✅ Git Bash / WSL (验证兼容性)

---

## 12. 构建与分发

### 12.1 构建

**本地构建**:
```bash
cargo build --release --target x86_64-pc-windows-msvc
```

**交叉编译** (Linux -> Windows):
```bash
cargo build --release --target x86_64-pc-windows-gnu
```

### 12.2 安装方式

**方式一**: 预编译二进制 + 手动配置
```powershell
# 下载 try.exe
# 运行初始化
.\try.exe init C:\Users\$env:USERNAME\experiments

# 将输出添加到 $PROFILE
```

**方式二**: PowerShell One-Click 安装
```powershell
irm https://raw.githubusercontent.com/.../install.ps1 | iex
```

**方式三**: Scoop 包 (推荐)
```powershell
scoop bucket add try-rs
scoop install try-rs
```

---

## 13. 风险与限制

| 风险 | 影响 | 缓解 |
|------|------|------|
| PowerShell 版本差异 | 命令语法兼容 | 最低支持 PS 5.1，推荐 PS 7+ |
| Windows Console 旧版 | TUI 渲染问题 | 推荐使用 Windows Terminal |
| 路径长度限制 | 深层路径操作 | 依赖 Windows 长路径支持 |
| 权限问题 | 配置文件写入 | 检测并提示用户 |

---

## 14. 总结

### 14.1 架构优势

- ✅ 核心逻辑与平台完全解耦
- ✅ 接口抽象清晰，易于测试和扩展
- ✅ 依赖反转，符合 SOLID 原则
- ✅ Windows 专注 PowerShell，简化复杂度

### 14.2 实施步骤

1. 创建目录结构
2. 定义接口 (`shell/trait.rs`, `shell/script.rs`)
3. 提取核心模块 (`core/`)
4. 实现 PowerShell 平台层
5. 重构 `main.rs` 进行依赖注入
6. 编写测试
7. 文档更新

### 14.3 预期收益

- 代码可维护性提升
- 易于单元测试
- 支持未来扩展到其他平台
- 平台特定逻辑隔离清晰
