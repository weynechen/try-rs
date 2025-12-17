
# try-rs

**try-rs** is a Rust port of the popular [try](https://github.com/tobi/try) tool. It is a command-line utility designed to help you easily manage and navigate temporary "sandbox" directories for experiments, keeping your main projects clean.

It features a fast TUI, fuzzy searching, Git integration, and multi-workspace management.

## Features

*   **⚡ Fast TUI**: Interactive interface built with `crossterm`.
*   **🔍 Fuzzy Search**: Quickly find existing experiments or create new ones.
*   **📅 Auto-Dating**: Directories are automatically suffixed with the date (e.g., `my-experiment-2025-12-16`).
*   **📦 Git Integration**: Easily clone repos or create worktrees into isolated directories, with proxy support.
*   **🗂️ Workspace Management**: Switch between different root directories (workspaces) using `try set`, with current directory prioritized.

## Installation

### Prerequisites

You need Rust and Cargo installed on your system.

### Build

```bash
git clone <this-repo-url>
cd try-rs
cargo build --release
```

The binary will be located at `./target/release/try`.

## Shell Integration (Required)

Since `try` needs to change your shell's current directory (`cd`), it cannot work as a standalone binary alone. You must configure your shell to wrap it.

Add the following to your shell configuration file (e.g., `~/.bashrc`, `~/.zshrc`):

```bash
# Replace /path/to/try-rs with the actual path to your compiled binary
eval "$(/path/to/try-rs/target/release/try init ~/experiments)"
```

*   `~/experiments` is the default directory where your "tries" will be stored. You can change this to any path you prefer.

After adding this, restart your terminal or run `source ~/.zshrc`.

## Usage

### Basic Navigation

Simply type `try` to open the interactive selector:

```bash
try
```

*   **Type** to filter directories.
*   **Up/Down** to navigate.
*   **Enter** to switch to the selected directory.
*   **Delete** to mark a directory for deletion (Batch delete supported).
*   **Esc** to cancel.

### Creating New Experiments

Type a name that doesn't exist, and select "Create new":

```bash
try my-new-idea
```

This will create `~/experiments/my-new-idea-YYYY-MM-DD` and `cd` into it.

### Git Cloning

Clone a repository into a fresh, dated directory:

```bash
try clone https://github.com/user/repo.git
```

This creates `repo-YYYY-MM-DD` and clones the source into it.

**Proxy Support**: If you need to use a proxy tool (like `proxychains` or similar) for cloning:

```bash
# Use --proxy option
try clone https://github.com/user/repo.git --proxy proxychains

# Or set TRY_PROXY environment variable
export TRY_PROXY=proxychains
try clone https://github.com/user/repo.git
```

The CLI option takes precedence over the environment variable.

### Git Worktrees

If you are inside a Git repository, you can create a detached worktree for experiments:

```bash
try worktree feature-x
```

### Workspace Management

`try-rs` allows you to manage multiple root locations (workspaces) for your experiments.

1.  **Add a new workspace**:
    Initialize a new path. This switches your current session to this path and saves it to history.
    ```bash
    # This sets the current TRY_PATH and adds it to history
    try init ~/other/projects
    ```

2.  **Switch workspaces**:
    Use `set` to choose from your previously used workspaces.
    ```bash
    try set
    ```
    This opens a TUI listing your workspace history, with the **current working directory prioritized at the top**. 
    Selecting a workspace will:
    - Update your `TRY_PATH` environment variable
    - Change to the selected directory (`cd`)
    - Save it to your workspace history

## Configuration

*   **History**: Workspace history is stored in `~/.config/try/workspaces` (on Linux).
*   **Environment**: The tool relies on the `TRY_PATH` environment variable, which is managed by the shell wrapper.

## License

MIT
