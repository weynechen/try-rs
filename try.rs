use std::env;
use std::fs;
use std::io::{self, BufRead, BufReader, Stderr, Write};
use std::path::{Path, PathBuf};
use std::sync::OnceLock;
use std::time::{Duration, SystemTime};

use anyhow::{Context, Result};
use chrono::Local;
use clap::{Parser, Subcommand};
use crossterm::{
    cursor,
    event::{self, Event, KeyCode, KeyEventKind, KeyModifiers},
    style::{Attribute, Color, Print, SetAttribute, SetForegroundColor},
    terminal::{self, Clear, ClearType},
    ExecutableCommand, QueueableCommand,
};
use regex::Regex;

// Cached regex patterns
fn date_suffix_regex() -> &'static Regex {
    static RE: OnceLock<Regex> = OnceLock::new();
    RE.get_or_init(|| Regex::new(r"^(.+)-(\d{4}-\d{2}-\d{2})$").unwrap())
}

fn git_url_regex() -> &'static Regex {
    static RE: OnceLock<Regex> = OnceLock::new();
    RE.get_or_init(|| Regex::new(r"([^/]+?)(\.git)?$").unwrap())
}

const VERSION: &str = "0.1.0";

fn today_suffix() -> String {
    Local::now().format("%Y-%m-%d").to_string()
}

struct WorkspaceManager;

impl WorkspaceManager {
    fn get_config_path() -> PathBuf {
        // Allow overriding the config location (useful for tests and for users
        // who want a custom location). Falls back to ~/.config/try/workspaces.
        if let Ok(p) = env::var("TRY_CONFIG") {
            if !p.trim().is_empty() {
                return PathBuf::from(p);
            }
        }
        dirs::home_dir()
            .map(|home| home.join(".config/try/workspaces"))
            .unwrap_or_else(|| PathBuf::from(".config/try/workspaces"))
    }

    // --- Path-parameterized core logic (testable without touching the real config) ---

    fn save_workspaces_to(config_path: &Path, workspaces: &[PathBuf]) -> Result<()> {
        if let Some(parent) = config_path.parent() {
            fs::create_dir_all(parent)?;
        }
        let mut file = std::fs::File::create(config_path)?;
        for ws in workspaces {
            writeln!(file, "{}", ws.to_string_lossy())?;
        }
        Ok(())
    }

    fn get_workspaces_from(config_path: &Path) -> Result<Vec<PathBuf>> {
        if !config_path.exists() {
            return Ok(Vec::new());
        }

        let file = std::fs::File::open(config_path)?;
        let reader = BufReader::new(file);

        let mut workspaces = Vec::new();
        for line in reader.lines() {
            let line = line?;
            if !line.trim().is_empty() {
                workspaces.push(PathBuf::from(line.trim()));
            }
        }
        Ok(workspaces)
    }

    fn add_workspace_to(config_path: &Path, path: &Path) -> Result<()> {
        let abs_path = canonicalize_clean(path);
        let path_str = abs_path.to_string_lossy().to_string();

        let mut workspaces = Self::get_workspaces_from(config_path)?;
        // Remove if exists to move to top
        workspaces.retain(|p| p.to_string_lossy() != path_str);
        // Insert at the beginning (first position)
        workspaces.insert(0, abs_path);

        Self::save_workspaces_to(config_path, &workspaces)
    }

    fn remove_workspaces_from(config_path: &Path, paths_to_remove: &[PathBuf]) -> Result<()> {
        let mut workspaces = Self::get_workspaces_from(config_path)?;

        // Remove matching paths
        workspaces.retain(|ws| {
            !paths_to_remove
                .iter()
                .any(|p| ws.to_string_lossy() == p.to_string_lossy())
        });

        Self::save_workspaces_to(config_path, &workspaces)
    }

    // --- Convenience wrappers that target the real config path ---

    fn add_workspace(path: &Path) -> Result<()> {
        Self::add_workspace_to(&Self::get_config_path(), path)
    }

    fn get_workspaces() -> Result<Vec<PathBuf>> {
        Self::get_workspaces_from(&Self::get_config_path())
    }

    fn remove_workspaces(paths_to_remove: &[PathBuf]) -> Result<()> {
        Self::remove_workspaces_from(&Self::get_config_path(), paths_to_remove)
    }
}

#[derive(Parser)]
#[command(name = "try")]
#[command(version = VERSION)]
#[command(about = "Ephemeral workspace manager", long_about = None)]
struct Cli {
    #[command(subcommand)]
    command: Option<Commands>,

    /// Optional query for interactive mode
    #[arg(index = 1)]
    query: Option<String>,
}

#[derive(Subcommand)]
enum Commands {
    /// Output shell function definition
    Init {
        #[arg(default_value = "~/project/test")]
        path: String,
        /// Target shell: bash | powershell (auto-detected if omitted)
        #[arg(short, long)]
        shell: Option<String>,
        /// Wrapper command name (default: `try` on bash, `tr` on PowerShell,
        /// since `try` is a reserved PowerShell keyword)
        #[arg(short, long)]
        name: Option<String>,
    },
    /// Clone git repo into date-prefixed directory
    Clone {
        url: String,
        name: Option<String>,
        #[arg(short, long)]
        proxy: Option<String>,
    },
    /// Select a workspace from history
    Set,
}

#[derive(Debug, Clone)]
struct TryEntry {
    basename: String,
    basename_down: String,
    path: PathBuf,
    mtime: SystemTime,
    score: f64,
}

enum SelectorMode {
    Scan(PathBuf),
    History(Vec<PathBuf>),
}

struct TrySelector {
    mode: SelectorMode,
    workspace_path: PathBuf,
    input_buffer: String,
    cursor_pos: usize,
    scroll_offset: usize,
    entries: Vec<TryEntry>,
    marked_for_deletion: Vec<PathBuf>,
    delete_mode: bool,
    delete_status: Option<String>,
    width: u16,
    height: u16,
}

impl TrySelector {
    fn new(mode: SelectorMode, search_term: String, workspace_path: PathBuf) -> Self {
        let (w, h) = terminal::size().unwrap_or((80, 24));
        Self {
            mode,
            workspace_path,
            input_buffer: search_term.clone().replace(" ", "-"),
            cursor_pos: 0,
            scroll_offset: 0,
            entries: Vec::new(),
            marked_for_deletion: Vec::new(),
            delete_mode: false,
            delete_status: None,
            width: w,
            height: h,
        }
    }

    fn cursor_up(&mut self) -> bool {
        if self.cursor_pos > 0 {
            self.cursor_pos -= 1;
            true
        } else {
            false
        }
    }

    fn cursor_down(&mut self) -> bool {
        let max_idx = self.visible_count().saturating_sub(1);
        if self.cursor_pos < max_idx {
            self.cursor_pos += 1;
            true
        } else {
            false
        }
    }

    fn run(&mut self) -> Result<Option<ShellAction>> {
        // Ensure the workspace directory exists (Scan mode) *before* touching
        // the terminal, so a failure (e.g. an inaccessible path) reports a
        // clear error instead of leaving the terminal in raw mode.
        if let SelectorMode::Scan(base_path) = &self.mode {
            if !base_path.exists() {
                fs::create_dir_all(base_path).with_context(|| {
                    format!(
                        "Failed to create workspace directory '{}'. \
                         Check the path/permissions, or set a valid one with \
                         `try set` or the TRY_PATH env var.",
                        base_path.display()
                    )
                })?;
            }
        }

        self.load_entries()?;

        terminal::enable_raw_mode()?;
        let mut stderr = io::stderr();
        stderr.execute(cursor::Hide)?;
        // Clear screen once at startup to ensure clean slate
        stderr.execute(Clear(ClearType::All))?;

        let result = self.main_loop(&mut stderr);

        stderr.execute(cursor::Show)?;
        stderr.execute(Clear(ClearType::All))?;
        stderr.execute(cursor::MoveTo(0, 0))?;
        terminal::disable_raw_mode()?;

        result
    }

    fn main_loop(&mut self, stderr: &mut Stderr) -> Result<Option<ShellAction>> {
        // Initial render
        self.refresh_scores();
        self.render(stderr)?;

        loop {
            // Block until an event is available
            if event::poll(Duration::from_millis(1000))? {
                let mut needs_redraw = false;
                let mut needs_recalc = false;

                match event::read()? {
                    // On Windows, crossterm also reports key Release events.
                    // Ignore them — otherwise the key-up events left over from
                    // typing `tr<Enter>` to launch get injected as input and
                    // immediately dismiss the selector.
                    Event::Key(key) if key.kind != KeyEventKind::Release => {
                        // Check for cancel keys (Ctrl+C or Esc)
                        let is_cancel = matches!(key.code, KeyCode::Esc)
                            || (key.code == KeyCode::Char('c')
                                && key.modifiers.contains(KeyModifiers::CONTROL));

                        match key.code {
                            _ if is_cancel => {
                                if self.delete_mode {
                                    self.delete_mode = false;
                                    self.marked_for_deletion.clear();
                                    needs_redraw = true;
                                } else {
                                    return Ok(None);
                                }
                            }
                            KeyCode::Enter => {
                                if self.delete_mode && !self.marked_for_deletion.is_empty() {
                                    self.confirm_batch_delete(stderr)?;
                                    needs_redraw = true;
                                    needs_recalc = true;
                                } else if let Some(action) = self.handle_selection() {
                                    return Ok(Some(action));
                                }
                            }
                            KeyCode::Up => {
                                needs_redraw = self.cursor_up();
                            }
                            KeyCode::Char('k') if key.modifiers.contains(KeyModifiers::CONTROL) => {
                                needs_redraw = self.cursor_up();
                            }
                            KeyCode::Down => {
                                needs_redraw = self.cursor_down();
                            }
                            KeyCode::Char('j') if key.modifiers.contains(KeyModifiers::CONTROL) => {
                                needs_redraw = self.cursor_down();
                            }
                            KeyCode::Backspace => {
                                self.input_buffer.pop();
                                self.cursor_pos = 0;
                                needs_redraw = true;
                                needs_recalc = true;
                            }
                            KeyCode::Delete => {
                                // Toggle delete mark
                                self.toggle_delete_mark();
                                needs_redraw = true;
                            }
                            KeyCode::Char(c) => {
                                if is_allowed_input_char(c) {
                                    self.input_buffer.push(c);
                                    self.cursor_pos = 0;
                                    needs_redraw = true;
                                    needs_recalc = true;
                                }
                            }
                            _ => {}
                        }
                    }
                    Event::Resize(w, h) => {
                        self.width = w;
                        self.height = h;
                        needs_redraw = true;
                        // On resize, we might want to clear all once to be safe
                        stderr.execute(Clear(ClearType::All))?;
                    }
                    _ => {}
                }

                if needs_recalc {
                    self.refresh_scores();
                }

                if needs_redraw || needs_recalc {
                    self.render(stderr)?;
                }
            }
        }
    }

    fn get_filtered_entries(&self) -> Vec<&TryEntry> {
        if self.input_buffer.is_empty() {
            self.entries.iter().collect()
        } else {
            self.entries.iter().filter(|e| e.score > 0.0).collect()
        }
    }

    fn visible_count(&self) -> usize {
        let create_new_option = !self.input_buffer.is_empty();
        // Filtered entries + optional create new
        self.get_filtered_entries().len() + if create_new_option { 1 } else { 0 }
    }

    fn toggle_delete_mark(&mut self) {
        let path_to_toggle = {
            let filtered = self.get_filtered_entries();
            if self.cursor_pos < filtered.len() {
                Some(filtered[self.cursor_pos].path.clone())
            } else {
                None
            }
        };

        if let Some(path) = path_to_toggle {
            if self.marked_for_deletion.contains(&path) {
                self.marked_for_deletion.retain(|p| p != &path);
            } else {
                self.marked_for_deletion.push(path);
                self.delete_mode = true;
            }
            if self.marked_for_deletion.is_empty() {
                self.delete_mode = false;
            }
        }
    }

    fn handle_selection(&self) -> Option<ShellAction> {
        let filtered = self.get_filtered_entries();

        // Check if "Create new" / "Add path" is selected
        if !self.input_buffer.is_empty() && self.cursor_pos == filtered.len() {
            match &self.mode {
                SelectorMode::Scan(base_path) => {
                    // Create new directory with date suffix
                    let date_suffix = today_suffix();
                    let name = self.input_buffer.replace(" ", "-");
                    let dirname = format!("{}-{}", name, date_suffix);
                    let path = base_path.join(dirname);
                    return Some(ShellAction::MkdirCd(path));
                }
                SelectorMode::History(_) => {
                    // Add new path to workspace (no date suffix)
                    let path = expand_path(&self.input_buffer);
                    return Some(ShellAction::Set(path));
                }
            }
        }

        if self.cursor_pos < filtered.len() {
            match &self.mode {
                SelectorMode::Scan(_) => {
                    return Some(ShellAction::Cd(filtered[self.cursor_pos].path.clone()))
                }
                SelectorMode::History(_) => {
                    return Some(ShellAction::Set(filtered[self.cursor_pos].path.clone()))
                }
            }
        }

        None
    }

    fn load_entries(&mut self) -> Result<()> {
        let mut entries = Vec::new();
        match &self.mode {
            SelectorMode::Scan(base_path) => {
                if base_path.exists() {
                    for entry in fs::read_dir(base_path)? {
                        let entry = entry?;
                        let path = entry.path();
                        if path.is_dir() {
                            let basename = path.file_name().unwrap().to_string_lossy().to_string();
                            if basename.starts_with(".") {
                                continue;
                            }

                            let metadata = fs::metadata(&path)?;
                            let mtime = metadata.modified()?;

                            entries.push(TryEntry {
                                basename: basename.clone(),
                                basename_down: basename.to_lowercase(),
                                path,
                                mtime,
                                score: 0.0,
                            });
                        }
                    }
                }
            }
            SelectorMode::History(workspaces) => {
                for path in workspaces {
                    // Show all workspaces, even if path doesn't exist
                    let metadata = fs::metadata(path).ok();
                    let mtime = metadata
                        .and_then(|m| m.modified().ok())
                        .unwrap_or(SystemTime::UNIX_EPOCH); // Use epoch for non-existent paths

                    entries.push(TryEntry {
                        basename: path.to_string_lossy().to_string(), // Use full path for history
                        basename_down: path.to_string_lossy().to_lowercase(),
                        path: path.clone(),
                        mtime,
                        score: 0.0,
                    });
                }
                // Reverse to show latest first by default if load order is preserved
                entries.reverse();
            }
        }
        self.entries = entries;
        Ok(())
    }

    fn refresh_scores(&mut self) {
        let query = self.input_buffer.to_lowercase();
        let query_chars: Vec<char> = query.chars().collect();
        let now = SystemTime::now();

        for entry in &mut self.entries {
            entry.score = calculate_score(entry, &query, &query_chars, now);
        }

        // Sort: High score first
        self.entries.sort_by(|a, b| {
            b.score
                .partial_cmp(&a.score)
                .unwrap_or(std::cmp::Ordering::Equal)
        });
    }

    fn render(&mut self, stderr: &mut Stderr) -> Result<()> {
        // Instead of Clear(All), we move to top and overwrite.
        // This reduces flickering and bandwidth.
        stderr.queue(cursor::MoveTo(0, 0))?;

        let separator = "─".repeat((self.width as usize).saturating_sub(1));

        // Header
        stderr.queue(SetForegroundColor(Color::Red))?; // Orange-ish
        stderr.queue(SetAttribute(Attribute::Bold))?;
        stderr.queue(Print("📁 Try Selector"))?;

        // Show workspace path
        stderr.queue(SetForegroundColor(Color::DarkGrey))?;
        stderr.queue(Print(" @ "))?;
        stderr.queue(SetForegroundColor(Color::Cyan))?;
        stderr.queue(Print(self.workspace_path.display().to_string()))?;

        stderr.queue(SetAttribute(Attribute::Reset))?;
        stderr.queue(Clear(ClearType::UntilNewLine))?; // Clear rest of line
        stderr.queue(Print("\r\n"))?;

        stderr.queue(SetForegroundColor(Color::DarkGrey))?;
        stderr.queue(Print(&separator))?;
        stderr.queue(SetAttribute(Attribute::Reset))?;
        stderr.queue(Clear(ClearType::UntilNewLine))?;
        stderr.queue(Print("\r\n"))?;

        // Search bar
        stderr.queue(SetForegroundColor(Color::DarkGrey))?;
        stderr.queue(Print("Search: "))?;
        stderr.queue(SetAttribute(Attribute::Reset))?;

        // Render search text with cursor
        stderr.queue(SetAttribute(Attribute::Bold))?;
        stderr.queue(SetForegroundColor(Color::Yellow))?;
        stderr.queue(Print(&self.input_buffer))?;
        stderr.queue(SetAttribute(Attribute::Reverse))?;
        stderr.queue(Print(" "))?; // Cursor block
        stderr.queue(SetAttribute(Attribute::Reset))?;
        stderr.queue(Clear(ClearType::UntilNewLine))?;
        stderr.queue(Print("\r\n"))?;

        stderr.queue(SetForegroundColor(Color::DarkGrey))?;
        stderr.queue(Print(&separator))?;
        stderr.queue(SetAttribute(Attribute::Reset))?;
        stderr.queue(Clear(ClearType::UntilNewLine))?;
        stderr.queue(Print("\r\n"))?;

        // List
        let max_visible = (self.height as usize).saturating_sub(8).max(3);
        let show_create_new = !self.input_buffer.is_empty();

        // Calculate filtered len first to update scroll_offset
        let filtered_len = self.get_filtered_entries().len();
        let total_items = filtered_len + if show_create_new { 1 } else { 0 };

        // Adjust scroll
        if self.cursor_pos < self.scroll_offset {
            self.scroll_offset = self.cursor_pos;
        } else if self.cursor_pos >= self.scroll_offset + max_visible {
            self.scroll_offset = self.cursor_pos + 1 - max_visible;
        }

        // Get list for rendering
        let filtered = self.get_filtered_entries();
        let visible_end = (self.scroll_offset + max_visible).min(total_items);

        for i in self.scroll_offset..visible_end {
            let is_selected = i == self.cursor_pos;

            // Cursor
            if is_selected {
                stderr.queue(SetAttribute(Attribute::Bold))?;
                stderr.queue(SetForegroundColor(Color::Yellow))?;
                stderr.queue(Print("→ "))?;
                stderr.queue(SetAttribute(Attribute::Reset))?;
            } else {
                stderr.queue(Print("  "))?;
            }

            if i < filtered.len() {
                let entry = filtered[i];
                let is_marked = self.marked_for_deletion.contains(&entry.path);
                let path_exists = entry.path.exists();

                if is_marked {
                    stderr.queue(Print("🗑️  "))?;
                    stderr.queue(SetAttribute(Attribute::CrossedOut))?;
                } else if !path_exists {
                    stderr.queue(Print("❌ "))?;
                    stderr.queue(SetForegroundColor(Color::DarkGrey))?;
                } else {
                    stderr.queue(Print("📁 "))?;
                }

                if is_selected {
                    stderr.queue(SetAttribute(Attribute::Bold))?;
                }

                // Render Name (Name + Date suffix)
                // Assuming format Name-YYYY-MM-DD
                if let Some(caps) = date_suffix_regex().captures(&entry.basename) {
                    let name_part = caps.get(1).unwrap().as_str();
                    let date_part = caps.get(2).unwrap().as_str();

                    self.print_highlighted(stderr, name_part, &self.input_buffer, is_selected)?;

                    if !self.input_buffer.is_empty() && self.input_buffer.contains('-') {
                        stderr.queue(SetForegroundColor(Color::Yellow))?;
                        stderr.queue(SetAttribute(Attribute::Bold))?;
                        stderr.queue(Print("-"))?;
                        stderr.queue(SetAttribute(Attribute::Reset))?;
                        if is_selected {
                            stderr.queue(SetAttribute(Attribute::Bold))?;
                        }
                    } else {
                        stderr.queue(SetForegroundColor(Color::DarkGrey))?;
                        stderr.queue(Print("-"))?;
                    }

                    stderr.queue(SetForegroundColor(Color::DarkGrey))?;
                    stderr.queue(Print(date_part))?;

                    stderr.queue(SetAttribute(Attribute::Reset))?;
                    if is_selected {
                        stderr.queue(SetAttribute(Attribute::Bold))?;
                    }
                    if is_marked {
                        stderr.queue(SetAttribute(Attribute::CrossedOut))?;
                    }
                } else {
                    self.print_highlighted(
                        stderr,
                        &entry.basename,
                        &self.input_buffer,
                        is_selected,
                    )?;
                }

                stderr.queue(SetAttribute(Attribute::Reset))?;

                // Meta (Time) - Right aligned simplified
                // let time_str = format_relative_time(entry.mtime);
                // Basic alignment logic could go here, omitting for brevity/complexity balance
                // stderr.queue(cursor::MoveToColumn(self.width - 15))?;
                // stderr.queue(Print(time_str))?;
            } else {
                // Create New / Add Path Option
                if is_selected {
                    stderr.queue(SetAttribute(Attribute::Bold))?;
                }
                match &self.mode {
                    SelectorMode::Scan(_) => {
                        let date_suffix = today_suffix();
                        stderr.queue(Print(format!(
                            "✨ Create new: {}-{}",
                            self.input_buffer, date_suffix
                        )))?;
                    }
                    SelectorMode::History(_) => {
                        stderr.queue(Print(format!("📌 Add path: {}", self.input_buffer)))?;
                    }
                }
                stderr.queue(SetAttribute(Attribute::Reset))?;
            }

            stderr.queue(Clear(ClearType::UntilNewLine))?;
            stderr.queue(Print("\r\n"))?;
        }

        // Fill remaining empty lines in the list area with blanks/clear
        let lines_to_clear = max_visible.saturating_sub(visible_end - self.scroll_offset);
        for _ in 0..lines_to_clear {
            stderr.queue(Clear(ClearType::CurrentLine))?;
            stderr.queue(Print("\r\n"))?;
        }

        // Footer
        stderr.queue(cursor::MoveTo(0, self.height - 2))?;
        stderr.queue(SetForegroundColor(Color::DarkGrey))?;
        stderr.queue(Print(&separator))?;
        stderr.queue(SetAttribute(Attribute::Reset))?;
        stderr.queue(Clear(ClearType::UntilNewLine))?;
        stderr.queue(Print("\r\n"))?;

        if let Some(status) = &self.delete_status {
            stderr.queue(SetAttribute(Attribute::Bold))?;
            stderr.queue(Print(status))?;
            stderr.queue(SetAttribute(Attribute::Reset))?;
        } else if self.delete_mode {
            stderr.queue(SetAttribute(Attribute::Bold))?;
            stderr.queue(SetForegroundColor(Color::Red))?;
            stderr.queue(Print(format!(
                "DELETE MODE ({} marked) | Enter: Confirm | Esc: Cancel",
                self.marked_for_deletion.len()
            )))?;
            stderr.queue(SetAttribute(Attribute::Reset))?;
        } else {
            stderr.queue(SetForegroundColor(Color::DarkGrey))?;
            stderr.queue(Print(
                "↑↓: Navigate  Enter: Select  Del: Delete  Esc: Cancel",
            ))?;
            stderr.queue(SetAttribute(Attribute::Reset))?;
        }
        stderr.queue(Clear(ClearType::UntilNewLine))?;

        stderr.flush()?;
        Ok(())
    }

    fn print_highlighted(
        &self,
        stderr: &mut Stderr,
        text: &str,
        query: &str,
        is_selected: bool,
    ) -> Result<()> {
        if query.is_empty() {
            stderr.queue(Print(text))?;
            return Ok(());
        }

        let text_chars: Vec<char> = text.chars().collect();
        let query_chars: Vec<char> = query.to_lowercase().chars().collect();
        let text_lower: Vec<char> = text.to_lowercase().chars().collect();

        let mut query_idx = 0;

        for (i, c) in text_chars.iter().enumerate() {
            if query_idx < query_chars.len() && text_lower[i] == query_chars[query_idx] {
                stderr.queue(SetForegroundColor(Color::Yellow))?;
                stderr.queue(SetAttribute(Attribute::Bold))?;
                stderr.queue(Print(c))?;

                // Reset attributes but restore selection state if needed
                stderr.queue(SetAttribute(Attribute::Reset))?;
                if is_selected {
                    stderr.queue(SetAttribute(Attribute::Bold))?;
                }

                query_idx += 1;
            } else {
                stderr.queue(Print(c))?;
            }
        }
        Ok(())
    }

    fn confirm_batch_delete(&mut self, stderr: &mut Stderr) -> Result<()> {
        // Simple confirmation via raw input (not full UI dialog for brevity)
        stderr.execute(Clear(ClearType::All))?;
        stderr.execute(cursor::MoveTo(0, 0))?;
        stderr.execute(Print(format!(
            "Delete {} directories? Type YES to confirm: ",
            self.marked_for_deletion.len()
        )))?;

        // We need to temporarily disable raw mode or handle string input manually.
        // Let's handle manually character by character
        let mut input = String::new();
        loop {
            if event::poll(Duration::from_millis(100))? {
                if let Event::Key(key) = event::read()? {
                    if key.kind == KeyEventKind::Release {
                        continue; // ignore key-up events (Windows)
                    }
                    match key.code {
                        KeyCode::Enter => break,
                        KeyCode::Char(c) => {
                            input.push(c);
                            stderr.execute(Print(c))?;
                        }
                        KeyCode::Backspace => {
                            input.pop();
                            stderr.execute(cursor::MoveLeft(1))?;
                            stderr.execute(Print(" "))?;
                            stderr.execute(cursor::MoveLeft(1))?;
                        }
                        KeyCode::Esc => {
                            input.clear();
                            break;
                        }
                        _ => {}
                    }
                }
            }
        }

        if input == "YES" {
            let count = self.marked_for_deletion.len();
            match &self.mode {
                SelectorMode::History(_) => {
                    // In History mode, remove from config file
                    if let Err(e) = WorkspaceManager::remove_workspaces(&self.marked_for_deletion) {
                        self.delete_status = Some(format!("Error removing workspaces: {}", e));
                    } else {
                        self.delete_status = Some(format!("Removed {} workspaces.", count));
                    }
                }
                SelectorMode::Scan(_) => {
                    // In Scan mode, delete directories from filesystem
                    for path in &self.marked_for_deletion {
                        if path.exists() {
                            fs::remove_dir_all(path)?;
                        }
                    }
                    self.delete_status = Some(format!("Deleted {} items.", count));
                }
            }
        } else {
            self.delete_status = Some("Delete cancelled.".to_string());
        }

        self.marked_for_deletion.clear();
        self.delete_mode = false;
        // Reload entries
        self.load_entries()?;
        Ok(())
    }
}

// Scoring Algorithm Port
fn calculate_score(entry: &TryEntry, query: &str, query_chars: &[char], now: SystemTime) -> f64 {
    let mut score = 0.0;

    // Default date suffix bonus (ends with digit)
    if entry
        .basename
        .chars()
        .last()
        .map_or(false, |c| c.is_numeric())
    {
        score += 2.0;
    }

    if !query.is_empty() {
        let text_lower: Vec<char> = entry.basename_down.chars().collect();
        let query_len = query_chars.len();
        let text_len = text_lower.len();

        let mut last_pos: isize = -1;
        let mut query_idx = 0;
        let mut i = 0;

        while i < text_len && query_idx < query_len {
            let char = text_lower[i];

            if char == query_chars[query_idx] {
                score += 1.0;

                // Boundary bonus
                let is_boundary = i == 0 || !text_lower[i - 1].is_alphanumeric();
                if is_boundary {
                    score += 1.0;
                }

                // Proximity bonus
                if last_pos >= 0 {
                    let gap = (i as isize) - last_pos - 1;
                    score += 2.0 / ((gap + 1) as f64).sqrt();
                }

                last_pos = i as isize;
                query_idx += 1;
            }
            i += 1;
        }

        if query_idx < query_len {
            return 0.0;
        }

        // Density bonus
        if last_pos >= 0 {
            score *= query_len as f64 / (last_pos as f64 + 1.0);
        }

        // Length penalty
        score *= 10.0 / (entry.basename.len() as f64 + 10.0);
    }

    // Recency bonus
    if let Ok(duration) = now.duration_since(entry.mtime) {
        let hours = duration.as_secs_f64() / 3600.0;
        score += 3.0 / (hours + 1.0).sqrt();
    }

    score
}

#[derive(Debug)]
enum ShellAction {
    Cd(PathBuf),
    MkdirCd(PathBuf),
    Set(PathBuf),
}

fn expand_path(path: &str) -> PathBuf {
    if path.starts_with("~/") {
        let home = dirs::home_dir().expect("Could not find home directory");
        home.join(&path[2..])
    } else {
        PathBuf::from(path)
    }
}

/// Characters accepted into the search/path input buffer. Includes `:` and `\`
/// so Windows absolute paths (e.g. `D:\tests`) can be typed in History mode.
fn is_allowed_input_char(c: char) -> bool {
    c.is_alphanumeric()
        || matches!(c, '-' | '_' | '.' | ' ' | '/' | '~' | ':' | '\\')
}

/// Strip Windows extended-length (verbatim) path prefixes. `std::fs::canonicalize`
/// returns paths like `\\?\D:\foo`, which PowerShell can't map back to a drive
/// and which leak into prompts. Returns the path unchanged when no prefix is
/// present (always the case on non-Windows).
fn strip_verbatim_prefix(path: &Path) -> PathBuf {
    let s = path.to_string_lossy();
    if let Some(rest) = s.strip_prefix(r"\\?\UNC\") {
        PathBuf::from(format!(r"\\{}", rest))
    } else if let Some(rest) = s.strip_prefix(r"\\?\") {
        PathBuf::from(rest)
    } else {
        path.to_path_buf()
    }
}

/// Canonicalize a path for storage, without the verbatim prefix. Falls back to
/// the input path if canonicalization fails (e.g. the path doesn't exist yet).
fn canonicalize_clean(path: &Path) -> PathBuf {
    std::fs::canonicalize(path)
        .map(|p| strip_verbatim_prefix(&p))
        .unwrap_or_else(|_| path.to_path_buf())
}

/// Parse a repository name out of a git URL (the last path segment, sans `.git`).
fn parse_repo_name(url: &str) -> Option<String> {
    git_url_regex()
        .captures(url)
        .and_then(|caps| caps.get(1))
        .map(|m| m.as_str().to_string())
}

// ============================================================================
// Shell integration layer
//
// The TUI (crossterm) is cross-platform, but the *scripts* `try` emits for the
// shell wrapper to `eval` are shell-specific. `Shell` selects the right
// `ScriptGenerator` so the exact same core logic drives Bash/Zsh on Unix and
// PowerShell on Windows.
// ============================================================================

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Shell {
    Bash,
    PowerShell,
}

impl Shell {
    /// Parse a shell name (case-insensitive). Recognizes bash/zsh/sh and
    /// powershell/pwsh/ps.
    fn parse(name: &str) -> Option<Shell> {
        match name.trim().to_lowercase().as_str() {
            "bash" | "zsh" | "sh" => Some(Shell::Bash),
            "powershell" | "pwsh" | "ps" | "ps1" => Some(Shell::PowerShell),
            _ => None,
        }
    }

    /// Detect the active shell from the environment.
    ///
    /// Priority: explicit `TRY_SHELL` (set by our own init wrapper) > presence
    /// of POSIX `SHELL` (bash/zsh) > PowerShell markers > compile-time OS.
    fn detect() -> Shell {
        if let Ok(s) = env::var("TRY_SHELL") {
            if let Some(shell) = Shell::parse(&s) {
                return shell;
            }
        }
        Self::detect_from(|k| env::var(k).ok())
    }

    /// Pure detection helper, parameterized over an env lookup for testability.
    fn detect_from(get: impl Fn(&str) -> Option<String>) -> Shell {
        // A POSIX-style $SHELL strongly implies bash/zsh, even on Windows
        // (e.g. Git Bash / WSL).
        if get("SHELL").is_some() {
            return Shell::Bash;
        }
        // PowerShell sets PSModulePath; cmd does not export $SHELL either.
        if get("PSModulePath").is_some() {
            return Shell::PowerShell;
        }
        if cfg!(windows) {
            Shell::PowerShell
        } else {
            Shell::Bash
        }
    }

    fn generator(self) -> Box<dyn ScriptGenerator> {
        match self {
            Shell::Bash => Box::new(BashGenerator),
            Shell::PowerShell => Box::new(PowerShellGenerator),
        }
    }
}

/// Default wrapper command name per shell. `try` is a reserved keyword in
/// PowerShell, so PowerShell uses `tr` instead.
fn default_fn_name(shell: Shell) -> &'static str {
    match shell {
        Shell::Bash => "try",
        Shell::PowerShell => "tr",
    }
}

/// Abstraction over emitting shell commands. Each method returns a single
/// command string; `join` combines a sequence into one script.
trait ScriptGenerator {
    /// Quote/escape a path for safe interpolation into a command.
    fn escape(&self, path: &Path) -> String;
    fn cd(&self, path: &Path) -> String;
    fn mkdir(&self, path: &Path) -> String;
    /// Update a directory's mtime so recency scoring bumps it to the top.
    fn touch(&self, path: &Path) -> String;
    fn set_env(&self, key: &str, value: &str) -> String;
    fn echo(&self, msg: &str) -> String;
    /// Combine commands into a single line the shell can `eval`.
    fn join(&self, cmds: &[String]) -> String;
    /// The shell function + env setup printed by `try init`.
    /// `fn_name` is the wrapper command the user will type.
    fn init_script(&self, fn_name: &str, exe: &str, default_path: &str) -> String;

    fn git_clone(&self, url: &str, dest: &Path, proxy: Option<&str>) -> String {
        let escaped = self.escape(dest);
        if let Some(proxy_tool) = proxy {
            format!("{} git clone '{}' '{}'", proxy_tool, url, escaped)
        } else {
            format!("git clone '{}' '{}'", url, escaped)
        }
    }
}

struct BashGenerator;

impl ScriptGenerator for BashGenerator {
    fn escape(&self, path: &Path) -> String {
        let s = path.to_string_lossy();
        // On Windows (Git Bash), normalize mixed separators to '/' so paths
        // joined by PathBuf don't break `cd`. On real Unix, a backslash is a
        // legal filename character and must be left untouched.
        let s = if cfg!(windows) {
            s.replace('\\', "/")
        } else {
            s.into_owned()
        };
        // single-quote escape: ' -> '\''
        s.replace('\'', "'\\''")
    }

    fn cd(&self, path: &Path) -> String {
        format!("cd '{}'", self.escape(path))
    }

    fn mkdir(&self, path: &Path) -> String {
        format!("mkdir -p '{}'", self.escape(path))
    }

    fn touch(&self, path: &Path) -> String {
        format!("touch '{}'", self.escape(path))
    }

    fn set_env(&self, key: &str, value: &str) -> String {
        format!("export {}='{}'", key, value.replace('\'', "'\\''"))
    }

    fn echo(&self, msg: &str) -> String {
        format!("echo '{}'", msg.replace('\'', "'\\''"))
    }

    fn join(&self, cmds: &[String]) -> String {
        cmds.join(" && \\\n  ")
    }

    fn init_script(&self, fn_name: &str, exe: &str, default_path: &str) -> String {
        format!(
            r#"
{name}() {{
    local out
    # Use absolute path to the binary to ensure consistency
    out=$('{exe}' "$@" 2>/dev/tty)
    if [ $? -eq 0 ]; then
        eval "$out"
    else
        # Echo error to stderr if needed, or do nothing on cancel
        :
    fi
}}
export TRY_PATH="{path}"
export TRY_SHELL="bash"
"#,
            name = fn_name,
            exe = exe,
            path = default_path
        )
    }
}

struct PowerShellGenerator;

impl PowerShellGenerator {
    /// PowerShell single-quote escaping: ' -> ''
    fn ps_quote(s: &str) -> String {
        s.replace('\'', "''")
    }
}

impl ScriptGenerator for PowerShellGenerator {
    fn escape(&self, path: &Path) -> String {
        // Normalize separators to '/' (PowerShell accepts them) then
        // PowerShell single-quote escape: ' -> ''
        Self::ps_quote(&path.to_string_lossy().replace('\\', "/"))
    }

    fn cd(&self, path: &Path) -> String {
        format!("Set-Location -LiteralPath '{}'", self.escape(path))
    }

    fn mkdir(&self, path: &Path) -> String {
        format!(
            "New-Item -ItemType Directory -Force -Path '{}' | Out-Null",
            self.escape(path)
        )
    }

    fn touch(&self, path: &Path) -> String {
        // Bump LastWriteTime so recency scoring promotes the directory.
        format!(
            "(Get-Item -LiteralPath '{}').LastWriteTime = Get-Date",
            self.escape(path)
        )
    }

    fn set_env(&self, key: &str, value: &str) -> String {
        format!("$env:{} = '{}'", key, Self::ps_quote(value))
    }

    fn echo(&self, msg: &str) -> String {
        format!("Write-Host '{}'", Self::ps_quote(msg))
    }

    fn join(&self, cmds: &[String]) -> String {
        cmds.join("; ")
    }

    fn git_clone(&self, url: &str, dest: &Path, proxy: Option<&str>) -> String {
        let escaped = self.escape(dest);
        if let Some(proxy_tool) = proxy {
            format!("{} git clone '{}' '{}'", proxy_tool, url, escaped)
        } else {
            format!("git clone '{}' '{}'", url, escaped)
        }
    }

    fn init_script(&self, fn_name: &str, exe: &str, default_path: &str) -> String {
        // NOTE: `try` is a reserved keyword in PowerShell, so the wrapper must
        // use a different name (default `tr`).
        format!(
            r#"
function {name} {{
    $out = & '{exe}' @args
    if ($LASTEXITCODE -eq 0 -and $out) {{
        Invoke-Expression ($out -join "`n")
    }}
}}
$env:TRY_PATH = '{path}'
$env:TRY_SHELL = 'powershell'
"#,
            name = fn_name,
            exe = PowerShellGenerator::ps_quote(exe),
            path = PowerShellGenerator::ps_quote(default_path)
        )
    }
}

/// Build the command sequence for a resolved user action.
fn build_action_script(gen: &dyn ScriptGenerator, action: &ShellAction) -> String {
    match action {
        ShellAction::Cd(path) => gen.join(&[gen.touch(path), gen.cd(path)]),
        ShellAction::MkdirCd(path) => {
            gen.join(&[gen.mkdir(path), gen.touch(path), gen.cd(path)])
        }
        ShellAction::Set(path) => {
            // Update the live shell's TRY_PATH to the chosen workspace, then cd.
            let env_cmd = gen.set_env("TRY_PATH", &path.to_string_lossy());
            gen.join(&[env_cmd, gen.cd(path)])
        }
    }
}

fn main() -> Result<()> {
    // Manually check for subcommands to redirect execution flow similar to Ruby script
    // Or use Clap properly.
    // The Ruby script uses a clever `try exec` pattern. We will emulate that.

    let cli = Cli::parse();

    // Resolve base path: workspaces config takes priority over TRY_PATH env var
    let base_path = {
        let workspaces = WorkspaceManager::get_workspaces().unwrap_or_default();
        if let Some(first) = workspaces.first() {
            // Use the first workspace from config (set by `try set`).
            // Strip any stale verbatim prefix from older configs.
            strip_verbatim_prefix(first)
        } else if let Ok(p) = env::var("TRY_PATH") {
            // Fall back to TRY_PATH env var if no workspaces configured
            expand_path(&p)
        } else {
            // Ultimate fallback
            expand_path("~/project/test")
        }
    };

    // If command is None, it defaults to interactive (or query)
    match cli.command {
        Some(Commands::Init { path, shell, name }) => {
            let path_buf = expand_path(&path);
            // Only add workspace if the list is empty (first time init)
            let workspaces = WorkspaceManager::get_workspaces().unwrap_or_default();
            if workspaces.is_empty() {
                if let Err(e) = WorkspaceManager::add_workspace(&path_buf) {
                    eprintln!("Warning: Failed to save workspace: {}", e);
                }
            }
            let shell = shell
                .as_deref()
                .and_then(Shell::parse)
                .unwrap_or_else(Shell::detect);
            let fn_name = name.unwrap_or_else(|| default_fn_name(shell).to_string());
            print_init_script(shell, &fn_name, &path);
        }
        Some(Commands::Clone { url, name, proxy }) => {
            generate_clone_script(&base_path, &url, name, proxy)?;
        }
        Some(Commands::Set) => {
            let workspaces = WorkspaceManager::get_workspaces().unwrap_or_default();

            run_interactive(SelectorMode::History(workspaces), String::new(), base_path)?;
        }
        None => {
            // Default: try [query] -> mapped to try exec cd [query] by the shell wrapper
            // But if called directly without wrapper:
            let query_str = cli.query.unwrap_or_default();

            // Check if query looks like a git url
            if query_str.starts_with("http") || query_str.starts_with("git@") {
                generate_clone_script(&base_path, &query_str, None, None)?;
            } else {
                // The wrapper usually calls `try exec ...`.
                // If we are here, we should output the script for the wrapper to eval.
                run_interactive(SelectorMode::Scan(base_path.clone()), query_str, base_path)?;
            }
        }
    }

    Ok(())
}

fn run_interactive(mode: SelectorMode, query: String, workspace_path: PathBuf) -> Result<()> {
    let mut selector = TrySelector::new(mode, query, workspace_path);
    if let Some(action) = selector.run()? {
        let gen = Shell::detect().generator();
        // For `Set`, update workspace history before emitting the cd script.
        if let ShellAction::Set(path) = &action {
            let _ = WorkspaceManager::add_workspace(path);
        }
        let script = build_action_script(gen.as_ref(), &action);
        println!("{}", script);
    } else {
        // Cancelled
        std::process::exit(1);
    }
    Ok(())
}

fn print_init_script(shell: Shell, fn_name: &str, default_path: &str) {
    let exe = env::current_exe().unwrap_or(PathBuf::from("try"));
    let exe_str = exe.to_string_lossy().to_string();
    let gen = shell.generator();
    print!("{}", gen.init_script(fn_name, &exe_str, default_path));
}

fn generate_clone_script(
    base_path: &Path,
    url: &str,
    name: Option<String>,
    proxy: Option<String>,
) -> Result<()> {
    let dir_name = if let Some(n) = name {
        n
    } else {
        // Parse git url for name; Ruby version produces repo-date style.
        let repo_name = parse_repo_name(url).context("Invalid git url")?;
        let date_suffix = today_suffix();
        format!("{}-{}", repo_name, date_suffix)
    };

    let full_path = base_path.join(&dir_name);

    // Determine proxy command: CLI option > environment variable
    let proxy_cmd = proxy.or_else(|| env::var("TRY_PROXY").ok());

    let gen = Shell::detect().generator();
    let script = gen.join(&[
        gen.mkdir(&full_path),
        gen.echo(&format!("Cloning {}...", url)),
        gen.git_clone(url, &full_path, proxy_cmd.as_deref()),
        gen.cd(&full_path),
    ]);
    println!("{}", script);

    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::sync::atomic::{AtomicUsize, Ordering};

    static TMP_COUNTER: AtomicUsize = AtomicUsize::new(0);

    fn unique_tmp_dir(tag: &str) -> PathBuf {
        let n = TMP_COUNTER.fetch_add(1, Ordering::SeqCst);
        let dir = std::env::temp_dir().join(format!(
            "try-rs-test-{}-{}-{}",
            tag,
            std::process::id(),
            n
        ));
        fs::create_dir_all(&dir).unwrap();
        dir
    }

    fn entry(basename: &str, mtime: SystemTime) -> TryEntry {
        TryEntry {
            basename: basename.to_string(),
            basename_down: basename.to_lowercase(),
            path: PathBuf::from(basename),
            mtime,
            score: 0.0,
        }
    }

    fn score_for(basename: &str, query: &str) -> f64 {
        let q = query.to_lowercase();
        let qc: Vec<char> = q.chars().collect();
        calculate_score(
            &entry(basename, SystemTime::UNIX_EPOCH),
            &q,
            &qc,
            SystemTime::now(),
        )
    }

    #[test]
    fn parse_repo_name_https_with_git_suffix() {
        assert_eq!(
            parse_repo_name("https://github.com/user/repo.git").as_deref(),
            Some("repo")
        );
    }

    #[test]
    fn parse_repo_name_https_without_git_suffix() {
        assert_eq!(
            parse_repo_name("https://github.com/user/cool-thing").as_deref(),
            Some("cool-thing")
        );
    }

    #[test]
    fn parse_repo_name_ssh_scp_syntax() {
        assert_eq!(
            parse_repo_name("git@github.com:user/repo.git").as_deref(),
            Some("repo")
        );
    }

    #[test]
    fn expand_path_tilde() {
        let home = dirs::home_dir().unwrap();
        assert_eq!(expand_path("~/foo/bar"), home.join("foo/bar"));
    }

    #[test]
    fn expand_path_absolute_untouched() {
        assert_eq!(expand_path("/abs/path"), PathBuf::from("/abs/path"));
    }

    #[test]
    fn score_subsequence_match_is_positive() {
        assert!(score_for("myproject-2025-01-01", "myp") > 0.0);
    }

    #[test]
    fn score_non_subsequence_is_zero() {
        assert_eq!(score_for("myproject", "zzz"), 0.0);
    }

    #[test]
    fn score_empty_query_still_ranks_by_recency_and_suffix() {
        let e = entry("proj-2025-01-01", SystemTime::now());
        let s = calculate_score(&e, "", &[], SystemTime::now());
        assert!(s > 0.0);
    }

    #[test]
    fn score_prefers_contiguous_match() {
        let contiguous = score_for("test-2025-01-01", "test");
        let spread = score_for("t-e-s-t-2025-01-01", "test");
        assert!(contiguous > spread, "{} !> {}", contiguous, spread);
    }

    #[test]
    fn shell_parse_known_names() {
        assert_eq!(Shell::parse("bash"), Some(Shell::Bash));
        assert_eq!(Shell::parse("ZSH"), Some(Shell::Bash));
        assert_eq!(Shell::parse("powershell"), Some(Shell::PowerShell));
        assert_eq!(Shell::parse("pwsh"), Some(Shell::PowerShell));
        assert_eq!(Shell::parse("cmd"), None);
    }

    #[test]
    fn detect_prefers_posix_shell_env() {
        let s = Shell::detect_from(|k| match k {
            "SHELL" => Some("/bin/bash".to_string()),
            "PSModulePath" => Some("C:/foo".to_string()),
            _ => None,
        });
        assert_eq!(s, Shell::Bash);
    }

    #[test]
    fn detect_powershell_when_only_psmodulepath() {
        let s = Shell::detect_from(|k| match k {
            "PSModulePath" => Some("C:/foo".to_string()),
            _ => None,
        });
        assert_eq!(s, Shell::PowerShell);
    }

    #[test]
    fn bash_cd_and_mkdir() {
        let g = BashGenerator;
        assert_eq!(g.cd(Path::new("/tmp/x")), "cd '/tmp/x'");
        assert_eq!(g.mkdir(Path::new("/tmp/x")), "mkdir -p '/tmp/x'");
        assert_eq!(g.touch(Path::new("/tmp/x")), "touch '/tmp/x'");
    }

    #[test]
    fn bash_escape_single_quote() {
        let g = BashGenerator;
        assert_eq!(g.escape(Path::new("/tmp/it's")), "/tmp/it'\\''s");
    }

    #[test]
    fn bash_escape_normalizes_backslashes() {
        let g = BashGenerator;
        if cfg!(windows) {
            // Git Bash: mixed separators normalized to '/'.
            assert_eq!(g.escape(Path::new(r"C:\a\b")), "C:/a/b");
        } else {
            // Unix: backslash is a legal filename char, left untouched.
            assert_eq!(g.escape(Path::new(r"C:\a\b")), r"C:\a\b");
        }
    }

    #[test]
    fn bash_join_uses_and_chain() {
        let g = BashGenerator;
        let joined = g.join(&["a".to_string(), "b".to_string()]);
        assert!(joined.contains("&&"));
        assert!(joined.contains('a') && joined.contains('b'));
    }

    #[test]
    fn bash_git_clone_with_and_without_proxy() {
        let g = BashGenerator;
        let plain = g.git_clone("https://x/y.git", Path::new("/d"), None);
        assert_eq!(plain, "git clone 'https://x/y.git' '/d'");
        let proxied = g.git_clone("https://x/y.git", Path::new("/d"), Some("proxychains"));
        assert!(proxied.starts_with("proxychains git clone"));
    }

    #[test]
    fn bash_init_script_shape() {
        let g = BashGenerator;
        let s = g.init_script("try", "/usr/local/bin/try", "~/experiments");
        assert!(s.contains("try()"));
        assert!(s.contains("eval \"$out\""));
        assert!(s.contains(r#"export TRY_PATH="~/experiments""#));
        assert!(s.contains(r#"export TRY_SHELL="bash""#));
    }

    #[test]
    fn bash_init_script_custom_name() {
        let g = BashGenerator;
        let s = g.init_script("t", "/usr/local/bin/try", "~/x");
        assert!(s.contains("t()"));
    }

    #[test]
    fn powershell_cd_and_mkdir() {
        let g = PowerShellGenerator;
        assert_eq!(
            g.cd(Path::new("C:/tmp/x")),
            "Set-Location -LiteralPath 'C:/tmp/x'"
        );
        assert!(g
            .mkdir(Path::new("C:/tmp/x"))
            .starts_with("New-Item -ItemType Directory"));
        assert!(g.touch(Path::new("C:/tmp/x")).contains("LastWriteTime"));
    }

    #[test]
    fn powershell_escape_doubles_single_quote() {
        let g = PowerShellGenerator;
        assert_eq!(g.escape(Path::new("C:/it's")), "C:/it''s");
    }

    #[test]
    fn powershell_escape_normalizes_backslashes() {
        let g = PowerShellGenerator;
        assert_eq!(g.escape(Path::new(r"C:\a\b")), "C:/a/b");
    }

    #[test]
    fn powershell_join_uses_semicolons() {
        let g = PowerShellGenerator;
        assert_eq!(g.join(&["a".to_string(), "b".to_string()]), "a; b");
    }

    #[test]
    fn powershell_set_env() {
        let g = PowerShellGenerator;
        assert_eq!(g.set_env("TRY_PATH", "C:/x"), "$env:TRY_PATH = 'C:/x'");
    }

    #[test]
    fn powershell_init_script_shape() {
        let g = PowerShellGenerator;
        let s = g.init_script("tr", r"C:\bin\try.exe", r"C:\experiments");
        assert!(s.contains("function tr"));
        // must NOT define a function literally named `try` (reserved keyword)
        assert!(!s.contains("function try"));
        assert!(s.contains("Invoke-Expression"));
        assert!(s.contains("$LASTEXITCODE"));
        assert!(s.contains("$env:TRY_SHELL = 'powershell'"));
    }

    #[test]
    fn default_fn_name_per_shell() {
        assert_eq!(default_fn_name(Shell::Bash), "try");
        assert_eq!(default_fn_name(Shell::PowerShell), "tr");
    }

    #[test]
    fn build_action_script_cd_bash() {
        let g = BashGenerator;
        let s = build_action_script(&g, &ShellAction::Cd(PathBuf::from("/tmp/x")));
        assert!(s.contains("touch '/tmp/x'"));
        assert!(s.contains("cd '/tmp/x'"));
    }

    #[test]
    fn build_action_script_mkdircd_powershell() {
        let g = PowerShellGenerator;
        let s = build_action_script(&g, &ShellAction::MkdirCd(PathBuf::from("C:/tmp/x")));
        assert!(s.contains("New-Item"));
        assert!(s.contains("Set-Location"));
        assert!(s.contains(';'));
    }

    #[test]
    fn build_action_script_set_updates_env_and_cds() {
        let g = BashGenerator;
        let s = build_action_script(&g, &ShellAction::Set(PathBuf::from("/tmp/ws")));
        assert!(s.contains("export TRY_PATH='/tmp/ws'"));
        assert!(s.contains("cd '/tmp/ws'"));
    }

    #[test]
    fn build_action_script_set_powershell_updates_env() {
        let g = PowerShellGenerator;
        let s = build_action_script(&g, &ShellAction::Set(PathBuf::from("C:/ws")));
        assert!(s.contains("$env:TRY_PATH = 'C:/ws'"));
        assert!(s.contains("Set-Location -LiteralPath 'C:/ws'"));
    }

    #[test]
    fn workspace_add_get_roundtrip_and_dedup_to_top() {
        let dir = unique_tmp_dir("ws-roundtrip");
        let cfg = dir.join("workspaces");

        let a = dir.join("a");
        let b = dir.join("b");
        fs::create_dir_all(&a).unwrap();
        fs::create_dir_all(&b).unwrap();

        WorkspaceManager::add_workspace_to(&cfg, &a).unwrap();
        WorkspaceManager::add_workspace_to(&cfg, &b).unwrap();
        WorkspaceManager::add_workspace_to(&cfg, &a).unwrap();

        let ws = WorkspaceManager::get_workspaces_from(&cfg).unwrap();
        assert_eq!(ws.len(), 2);
        assert_eq!(ws[0], canonicalize_clean(&a));
    }

    #[test]
    fn workspace_remove() {
        let dir = unique_tmp_dir("ws-remove");
        let cfg = dir.join("workspaces");
        let a = dir.join("a");
        let b = dir.join("b");
        fs::create_dir_all(&a).unwrap();
        fs::create_dir_all(&b).unwrap();

        WorkspaceManager::add_workspace_to(&cfg, &a).unwrap();
        WorkspaceManager::add_workspace_to(&cfg, &b).unwrap();

        let canon_a = canonicalize_clean(&a);
        WorkspaceManager::remove_workspaces_from(&cfg, &[canon_a.clone()]).unwrap();

        let ws = WorkspaceManager::get_workspaces_from(&cfg).unwrap();
        assert_eq!(ws.len(), 1);
        assert!(!ws.contains(&canon_a));
    }

    #[test]
    fn workspace_get_missing_file_is_empty() {
        let dir = unique_tmp_dir("ws-missing");
        let cfg = dir.join("does-not-exist");
        let ws = WorkspaceManager::get_workspaces_from(&cfg).unwrap();
        assert!(ws.is_empty());
    }

    #[test]
    fn config_path_honors_try_config_env() {
        // Note: env mutation is process-global; keep this test self-contained.
        let dir = unique_tmp_dir("cfg-env");
        let custom = dir.join("custom-workspaces");
        std::env::set_var("TRY_CONFIG", &custom);
        assert_eq!(WorkspaceManager::get_config_path(), custom);
        std::env::remove_var("TRY_CONFIG");
    }

    #[test]
    fn input_allows_windows_path_chars() {
        // Drive colon and backslash must be accepted so `D:\tests` is typable.
        for c in "D:\\tests".chars() {
            assert!(is_allowed_input_char(c), "char {:?} should be allowed", c);
        }
        assert!(is_allowed_input_char('/'));
        assert!(is_allowed_input_char('~'));
        // Characters that should still be rejected.
        assert!(!is_allowed_input_char('"'));
        assert!(!is_allowed_input_char('*'));
        assert!(!is_allowed_input_char('?'));
    }

    #[test]
    fn strip_verbatim_prefix_removes_drive_prefix() {
        assert_eq!(
            strip_verbatim_prefix(Path::new(r"\\?\D:\tests")),
            PathBuf::from(r"D:\tests")
        );
    }

    #[test]
    fn strip_verbatim_prefix_removes_unc_prefix() {
        assert_eq!(
            strip_verbatim_prefix(Path::new(r"\\?\UNC\server\share")),
            PathBuf::from(r"\\server\share")
        );
    }

    #[test]
    fn strip_verbatim_prefix_leaves_normal_path() {
        assert_eq!(
            strip_verbatim_prefix(Path::new(r"D:\tests")),
            PathBuf::from(r"D:\tests")
        );
        assert_eq!(
            strip_verbatim_prefix(Path::new("/home/me/x")),
            PathBuf::from("/home/me/x")
        );
    }
}
