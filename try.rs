use std::env;
use std::fs;
use std::io::{self, Write, Stderr};
use std::path::{Path, PathBuf};
use std::time::{SystemTime, Duration};

use anyhow::{Result, Context};
use chrono::Local;
use clap::{Parser, Subcommand};
use directories::ProjectDirs;
use std::io::{BufRead, BufReader};
use crossterm::{
    cursor,
    event::{self, Event, KeyCode, KeyModifiers},
    style::{Attribute, Color, Print, SetAttribute, SetForegroundColor},
    terminal::{self, Clear, ClearType},
    QueueableCommand, ExecutableCommand,
};
use regex::Regex;

const VERSION: &str = "0.1.0";

struct WorkspaceManager;

impl WorkspaceManager {
    fn get_config_path() -> Option<PathBuf> {
        ProjectDirs::from("com", "try-rs", "try")
            .map(|proj| proj.config_dir().join("workspaces"))
    }

    fn add_workspace(path: &Path) -> Result<()> {
        let config_path = Self::get_config_path().context("Could not determine config path")?;
        
        if let Some(parent) = config_path.parent() {
            fs::create_dir_all(parent)?;
        }

        let abs_path = std::fs::canonicalize(path).unwrap_or(path.to_path_buf());
        let path_str = abs_path.to_string_lossy();

        let mut workspaces = Self::get_workspaces()?;
        // Remove if exists to move to top/bottom
        workspaces.retain(|p| p.to_string_lossy() != path_str);
        workspaces.push(abs_path);

        let mut file = std::fs::File::create(&config_path)?;
        for ws in workspaces {
            writeln!(file, "{}", ws.to_string_lossy())?;
        }
        
        // eprintln!("# Workspace saved to: {}", config_path.display());
        Ok(())
    }

    fn get_workspaces() -> Result<Vec<PathBuf>> {
        let config_path = Self::get_config_path();
        if config_path.is_none() || !config_path.as_ref().unwrap().exists() {
            return Ok(Vec::new());
        }
        
        let file = std::fs::File::open(config_path.unwrap())?;
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
    },
    /// Clone git repo into date-prefixed directory
    Clone {
        url: String,
        name: Option<String>,
    },
    /// Create worktree in dated directory
    Worktree {
        name: String,
        #[arg(short, long)]
        base: Option<String>,
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

    fn run(&mut self) -> Result<Option<ShellAction>> {
        self.load_entries()?;
        
        terminal::enable_raw_mode()?;
        let mut stderr = io::stderr();
        stderr.execute(cursor::Hide)?;
        // Clear screen once at startup to ensure clean slate
        stderr.execute(Clear(ClearType::All))?;

        // Ensure directory exists only in Scan mode
        if let SelectorMode::Scan(base_path) = &self.mode {
            if !base_path.exists() {
                fs::create_dir_all(base_path)?;
            }
        }

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
                    Event::Key(key) => {
                        match key.code {
                            KeyCode::Char('c') if key.modifiers.contains(KeyModifiers::CONTROL) => {
                                if self.delete_mode {
                                    self.delete_mode = false;
                                    self.marked_for_deletion.clear();
                                    needs_redraw = true;
                                } else {
                                    return Ok(None);
                                }
                            }
                            KeyCode::Esc => {
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
                            KeyCode::Up | KeyCode::Char('p') if key.modifiers.contains(KeyModifiers::CONTROL) || key.code == KeyCode::Up => {
                                if self.cursor_pos > 0 {
                                    self.cursor_pos -= 1;
                                    needs_redraw = true;
                                }
                            }
                            KeyCode::Down | KeyCode::Char('n') if key.modifiers.contains(KeyModifiers::CONTROL) || key.code == KeyCode::Down => {
                                let max_idx = self.visible_count().saturating_sub(1);
                                if self.cursor_pos < max_idx {
                                    self.cursor_pos += 1;
                                    needs_redraw = true;
                                }
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
                                 if c.is_alphanumeric() || c == '-' || c == '_' || c == '.' || c == ' ' {
                                    self.input_buffer.push(c);
                                    self.cursor_pos = 0;
                                    needs_redraw = true;
                                    needs_recalc = true;
                                }
                            }
                            _ => {}
                        }
                    },
                    Event::Resize(w, h) => {
                        self.width = w;
                        self.height = h;
                        needs_redraw = true;
                        // On resize, we might want to clear all once to be safe
                        stderr.execute(Clear(ClearType::All))?;
                    },
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
        self.get_filtered_entries().len()
        + if create_new_option { 1 } else { 0 }
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
        
        // Check if "Create new" is selected
        if !self.input_buffer.is_empty() && self.cursor_pos == filtered.len() {
            // Create new
            if let SelectorMode::Scan(base_path) = &self.mode {
                let date_suffix = Local::now().format("%Y-%m-%d").to_string();
                let name = self.input_buffer.replace(" ", "-");
                let dirname = format!("{}-{}", name, date_suffix);
                let path = base_path.join(dirname);
                return Some(ShellAction::MkdirCd(path));
            } else {
                return None; // Cannot create new in History mode (or implement later)
            }
        }

        if self.cursor_pos < filtered.len() {
            match &self.mode {
                SelectorMode::Scan(_) => return Some(ShellAction::Cd(filtered[self.cursor_pos].path.clone())),
                SelectorMode::History(_) => return Some(ShellAction::Set(filtered[self.cursor_pos].path.clone())),
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
                            if basename.starts_with(".") { continue; }
                            
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
                    if path.exists() {
                        let metadata = fs::metadata(path).ok();
                        let mtime = metadata.and_then(|m| m.modified().ok()).unwrap_or(SystemTime::now());

                        entries.push(TryEntry {
                            basename: path.to_string_lossy().to_string(), // Use full path for history
                            basename_down: path.to_string_lossy().to_lowercase(),
                            path: path.clone(),
                            mtime,
                            score: 0.0,
                        });
                    }
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
        self.entries.sort_by(|a, b| b.score.partial_cmp(&a.score).unwrap_or(std::cmp::Ordering::Equal));
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
                
                if is_marked {
                    stderr.queue(Print("🗑️  "))?;
                    stderr.queue(SetAttribute(Attribute::CrossedOut))?;
                } else {
                    stderr.queue(Print("📁 "))?;
                }

                if is_selected {
                    stderr.queue(SetAttribute(Attribute::Bold))?;
                }

                // Render Name (Name + Date suffix)
                // Assuming format Name-YYYY-MM-DD
                let date_regex = Regex::new(r"^(.+)-(\d{4}-\d{2}-\d{2})$").unwrap();
                if let Some(caps) = date_regex.captures(&entry.basename) {
                    let name_part = caps.get(1).unwrap().as_str();
                    let date_part = caps.get(2).unwrap().as_str();

                    self.print_highlighted(stderr, name_part, &self.input_buffer, is_selected)?;

                    if !self.input_buffer.is_empty() && self.input_buffer.contains('-') {
                         stderr.queue(SetForegroundColor(Color::Yellow))?;
                         stderr.queue(SetAttribute(Attribute::Bold))?;
                         stderr.queue(Print("-"))?;
                         stderr.queue(SetAttribute(Attribute::Reset))?;
                         if is_selected { stderr.queue(SetAttribute(Attribute::Bold))?; }
                    } else {
                         stderr.queue(SetForegroundColor(Color::DarkGrey))?;
                         stderr.queue(Print("-"))?;
                    }

                    stderr.queue(SetForegroundColor(Color::DarkGrey))?;
                    stderr.queue(Print(date_part))?;
                    
                    stderr.queue(SetAttribute(Attribute::Reset))?;
                    if is_selected { stderr.queue(SetAttribute(Attribute::Bold))?; }
                    if is_marked { stderr.queue(SetAttribute(Attribute::CrossedOut))?; }
                    
                } else {
                    self.print_highlighted(stderr, &entry.basename, &self.input_buffer, is_selected)?;
                }

                stderr.queue(SetAttribute(Attribute::Reset))?;

                // Meta (Time) - Right aligned simplified
                // let time_str = format_relative_time(entry.mtime);
                // Basic alignment logic could go here, omitting for brevity/complexity balance
                // stderr.queue(cursor::MoveToColumn(self.width - 15))?;
                // stderr.queue(Print(time_str))?;

            } else {
                // Create New Option
                if is_selected {
                     stderr.queue(SetAttribute(Attribute::Bold))?;
                }
                let date_suffix = Local::now().format("%Y-%m-%d").to_string();
                stderr.queue(Print(format!("✨ Create new: {}-{}", self.input_buffer, date_suffix)))?;
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
            stderr.queue(Print(format!("DELETE MODE ({} marked) | Enter: Confirm | Esc: Cancel", self.marked_for_deletion.len())))?;
            stderr.queue(SetAttribute(Attribute::Reset))?;
        } else {
            stderr.queue(SetForegroundColor(Color::DarkGrey))?;
            stderr.queue(Print("↑↓: Navigate  Enter: Select  Del: Delete  Esc: Cancel"))?;
            stderr.queue(SetAttribute(Attribute::Reset))?;
        }
        stderr.queue(Clear(ClearType::UntilNewLine))?;

        stderr.flush()?;
        Ok(())
    }

    fn print_highlighted(&self, stderr: &mut Stderr, text: &str, query: &str, is_selected: bool) -> Result<()> {
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
        stderr.execute(Print(format!("Delete {} directories? Type YES to confirm: ", self.marked_for_deletion.len())))?;
        
        // We need to temporarily disable raw mode or handle string input manually.
        // Let's handle manually character by character
        let mut input = String::new();
        loop {
             if event::poll(Duration::from_millis(100))? {
                if let Event::Key(key) = event::read()? {
                    match key.code {
                        KeyCode::Enter => break,
                        KeyCode::Char(c) => {
                            input.push(c);
                            stderr.execute(Print(c))?;
                        },
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
             for path in &self.marked_for_deletion {
                 if path.exists() {
                     fs::remove_dir_all(path)?;
                 }
             }
             self.delete_status = Some(format!("Deleted {} items.", self.marked_for_deletion.len()));
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
    if entry.basename.chars().last().map_or(false, |c| c.is_numeric()) {
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
                let is_boundary = i == 0 || !text_lower[i-1].is_alphanumeric();
                if is_boundary { score += 1.0; }

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

fn main() -> Result<()> {
    // Manually check for subcommands to redirect execution flow similar to Ruby script
    // Or use Clap properly.
    // The Ruby script uses a clever `try exec` pattern. We will emulate that.
    
    let cli = Cli::parse();
    
    // Resolve base path
    let base_path = match env::var("TRY_PATH") {
        Ok(p) => expand_path(&p),
        Err(_) => {
            expand_path("~/project/test")
        }
    };
    
    // If command is None, it defaults to interactive (or query)
    match cli.command {
        Some(Commands::Init { path }) => {
            let path_buf = expand_path(&path);
            if let Err(e) = WorkspaceManager::add_workspace(&path_buf) {
                eprintln!("Warning: Failed to save workspace: {}", e);
            }
            print_init_script(&path);
        },
        Some(Commands::Clone { url, name }) => {
            generate_clone_script(&base_path, &url, name)?;
        },
        Some(Commands::Worktree { name, base }) => {
            generate_worktree_script(&base_path, &name, base)?;
        },
        Some(Commands::Set) => {
            let workspaces = WorkspaceManager::get_workspaces().unwrap_or_default();
            run_interactive(SelectorMode::History(workspaces), String::new(), base_path)?;
        },
        None => {
            // Default: try [query] -> mapped to try exec cd [query] by the shell wrapper
            // But if called directly without wrapper:
            let query_str = cli.query.unwrap_or_default();
            
            // Check if query looks like a git url
            if query_str.starts_with("http") || query_str.starts_with("git@") {
                 generate_clone_script(&base_path, &query_str, None)?;
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
        match action {
            ShellAction::Cd(path) => {
                emit_script(vec![
                    format!("touch '{}'", path.display()),
                    format!("cd '{}'", path.display())
                ]);
            }
            ShellAction::MkdirCd(path) => {
                emit_script(vec![
                    format!("mkdir -p '{}'", path.display()),
                    format!("touch '{}'", path.display()),
                    format!("cd '{}'", path.display())
                ]);
            }
            ShellAction::Set(path) => {
                // Export TRY_PATH and switch there maybe? Usually just set TRY_PATH.
                // Or maybe cd to it as well? Let's just set variable for now.
                println!("export TRY_PATH='{}'", path.display());
                // Also update history to put this one at top?
                let _ = WorkspaceManager::add_workspace(&path);
            }
        }
    } else {
        // Cancelled
        std::process::exit(1);
    }
    Ok(())
}


fn print_init_script(default_path: &str) {
    let exe = env::current_exe().unwrap_or(PathBuf::from("try"));
    let exe_str = exe.display();
    
    println!(r#"
try() {{
    local out
    # Use absolute path to the binary to ensure consistency
    out=$('{}' "$@" 2>/dev/tty)
    if [ $? -eq 0 ]; then
        eval "$out"
    else
        # Echo error to stderr if needed, or do nothing on cancel
        :
    fi
}}
export TRY_PATH="{}"
"#, exe_str, default_path);
}

fn emit_script(cmds: Vec<String>) {
    // print to stdout
    let joined = cmds.join(" && \\\n  ");
    println!("{}", joined);
}

fn generate_clone_script(base_path: &Path, url: &str, name: Option<String>) -> Result<()> {
    let dir_name = if let Some(n) = name {
        n
    } else {
        // Parse git url for name
        let re = Regex::new(r"([^/]+?)(\.git)?$").unwrap();
        let caps = re.captures(url).context("Invalid git url")?;
        let repo_name = caps.get(1).unwrap().as_str();
        let date_suffix = Local::now().format("%Y-%m-%d").to_string();
        // Assuming simplistic parsing: user-repo-date style or just date-repo
        // Ruby version does: date-user-repo
        format!("{}-{}", repo_name, date_suffix)
    };
    
    let full_path = base_path.join(&dir_name);
    
    emit_script(vec![
        format!("mkdir -p '{}'", full_path.display()),
        format!("echo 'Cloning {}...'", url),
        format!("git clone '{}' '{}'", url, full_path.display()),
        format!("cd '{}'", full_path.display())
    ]);
    
    Ok(())
}

fn generate_worktree_script(base_path: &Path, name: &str, _base: Option<String>) -> Result<()> {
    // Simplified worktree logic
    let date_suffix = Local::now().format("%Y-%m-%d").to_string();
    let dir_name = format!("{}-{}", name, date_suffix);
    let full_path = base_path.join(dir_name);
    
    // Check if inside git repo happens in shell script usually, but we can generate the command
    let cmd = format!(
        "if git rev-parse --is-inside-work-tree >/dev/null 2>&1; then \
            repo=$(git rev-parse --show-toplevel); \
            git -C \"$repo\" worktree add --detach '{}'; \
         fi",
        full_path.display()
    );
    
    emit_script(vec![
        format!("mkdir -p '{}'", full_path.display()),
        cmd,
        format!("cd '{}'", full_path.display())
    ]);
    
    Ok(())
}
