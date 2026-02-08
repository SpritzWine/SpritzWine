use anyhow::{Context, Result};
use chrono::{DateTime, Local, TimeZone};
use clap::Parser;
use crossterm::{
    event::{self, Event, KeyCode},
    execute,
    terminal::{EnterAlternateScreen, LeaveAlternateScreen, disable_raw_mode, enable_raw_mode},
};
use humansize::{DECIMAL, format_size};
use ratatui::{layout::Rect, prelude::*, widgets::*};
use std::{
    fs,
    path::{Path, PathBuf},
    time::SystemTime,
};

/// Command line arguments for the dir program
#[derive(Parser, Debug)]
#[command(name = "dir")]
#[command(about = "Directory listing with colorful TUI", long_about = None)]
struct Args {
    /// Directory to list (default: current directory)
    #[arg(default_value = ".")]
    path: String,

    /// Sort by name (default)
    #[arg(long, short = 'n')]
    sort_name: bool,

    /// Sort by size
    #[arg(long, short = 's')]
    sort_size: bool,

    /// Sort by date
    #[arg(long, short = 'd')]
    sort_date: bool,

    /// Show hidden files
    #[arg(long, short = 'a')]
    all: bool,
}

/// File information structure
#[derive(Debug, Clone)]
struct FileInfo {
    name: String,
    path: PathBuf,
    is_dir: bool,
    size: u64,
    modified: SystemTime,
    display_name: String,
}

impl FileInfo {
    fn new(path: &Path) -> Result<Self> {
        let metadata = fs::metadata(path)
            .with_context(|| format!("Failed to get metadata for {}", path.display()))?;

        let modified = metadata.modified().unwrap_or(SystemTime::now());

        Ok(Self {
            name: path
                .file_name()
                .unwrap_or_default()
                .to_string_lossy()
                .into_owned(),
            path: path.to_path_buf(),
            is_dir: metadata.is_dir(),
            size: if metadata.is_dir() { 0 } else { metadata.len() },
            modified,
            display_name: format_display_name(path),
        })
    }
}

/// Format file name for display (highlight directories, show hidden files differently)
fn format_display_name(path: &Path) -> String {
    let name = path.file_name().unwrap_or_default().to_string_lossy();

    if path.is_dir() {
        format!("[{}]", name)
    } else if name.starts_with('.') {
        format!("({})", name)
    } else {
        name.to_string()
    }
}

/// Convert SystemTime to human-readable format
fn format_time(modified: &SystemTime) -> String {
    modified
        .duration_since(SystemTime::UNIX_EPOCH)
        .map(|duration| {
            let dt: DateTime<Local> = Local.timestamp_opt(duration.as_secs() as i64, 0).unwrap();
            dt.format("%Y-%m-%d %H:%M:%S").to_string()
        })
        .unwrap_or_else(|_| "Invalid date".to_string())
}

/// Get file type indicator
fn get_file_type_indicator(is_dir: bool) -> String {
    if is_dir {
        "DIR ".to_string()
    } else {
        "FILE".to_string()
    }
}

fn main() -> Result<()> {
    let args = Args::parse();

    // Setup terminal
    enable_raw_mode()?;
    let mut stdout = std::io::stdout();
    execute!(stdout, EnterAlternateScreen)?;
    let backend = CrosstermBackend::new(stdout);
    let mut terminal = Terminal::new(backend)?;

    // Run the application
    let result = run_app(&args, &mut terminal);

    // Restore terminal
    disable_raw_mode()?;
    execute!(terminal.backend_mut(), LeaveAlternateScreen)?;
    terminal.show_cursor()?;

    if let Err(e) = result {
        println!("Application error: {}", e);
        std::process::exit(1);
    }

    Ok(())
}

fn run_app(args: &Args, terminal: &mut Terminal<CrosstermBackend<std::io::Stdout>>) -> Result<()> {
    // Load initial directory contents
    let path = PathBuf::from(&args.path);
    let files = load_directory_contents(&path, args.all)?;

    // Sort files based on arguments
    let mut sorted_files = files.clone();
    if args.sort_size {
        sorted_files.sort_by(|a, b| b.size.cmp(&a.size));
    } else if args.sort_date {
        sorted_files.sort_by(|a, b| b.modified.cmp(&a.modified));
    } else {
        // Default: sort by name
        sorted_files.sort_by(|a, b| a.name.cmp(&b.name));
    }

    let mut state = AppState {
        current_path: path.clone(),
        files,
        selected_index: 0,
        show_help: false,
        sort_mode: if args.sort_size {
            SortMode::Size
        } else if args.sort_date {
            SortMode::Date
        } else {
            SortMode::Name
        },
        navigation_history: vec![path.clone()],
        navigation_index: 0,
        scroll_offset: 0,
    };

    loop {
        terminal.draw(|f| ui(f, &state))?;

        // Handle input
        if let Event::Key(key) = event::read()? {
            match key.code {
                KeyCode::Char('q') | KeyCode::Esc => break,
                KeyCode::Char('h') => state.show_help = !state.show_help,
                KeyCode::Up => {
                    if state.selected_index > 0 {
                        state.selected_index -= 1;
                        // Scroll up if needed
                        if state.selected_index < state.scroll_offset {
                            state.scroll_offset = state.selected_index;
                        }
                    }
                }
                KeyCode::Down => {
                    if state.selected_index < state.files.len().saturating_sub(1) {
                        state.selected_index += 1;
                        // Scroll down if selected item goes beyond visible area
                        // Assume about 8-10 items are visible, start scrolling when we reach that limit
                        let visible_threshold = 8; // Start scrolling when selected index reaches this offset
                        if state.selected_index > state.scroll_offset + visible_threshold {
                            state.scroll_offset = state.selected_index - visible_threshold;
                        }
                    }
                }
                KeyCode::Enter => {
                    let selected_file = &state.files[state.selected_index];
                    if selected_file.is_dir {
                        // Change to directory
                        let new_path = selected_file.path.join("");
                        match load_directory_contents(&new_path, args.all) {
                            Ok(new_files) => {
                                // Update navigation history
                                state
                                    .navigation_history
                                    .truncate(state.navigation_index + 1);
                                state.navigation_history.push(new_path.clone());
                                state.navigation_index += 1;

                                state.current_path = new_path;
                                state.files = new_files;
                                state.selected_index = 0;
                            }
                            Err(e) => {
                                // Show error but continue
                                eprintln!("Error loading directory: {}", e);
                            }
                        }
                    } else {
                        // Open file (just print path for now)
                        println!("Opening file: {:?}", selected_file.path);
                    }
                }
                KeyCode::Char('n') | KeyCode::Char('s') | KeyCode::Char('d') => {
                    // Change sort mode
                    state.sort_mode = match key.code {
                        KeyCode::Char('n') => SortMode::Name,
                        KeyCode::Char('s') => SortMode::Size,
                        KeyCode::Char('d') => SortMode::Date,
                        _ => state.sort_mode,
                    };

                    // Re-sort files
                    let mut sorted_files = state.files.clone();
                    match state.sort_mode {
                        SortMode::Name => sorted_files.sort_by(|a, b| a.name.cmp(&b.name)),
                        SortMode::Size => sorted_files.sort_by(|a, b| b.size.cmp(&a.size)),
                        SortMode::Date => sorted_files.sort_by(|a, b| b.modified.cmp(&a.modified)),
                    }
                    state.files = sorted_files;
                }
                KeyCode::Left => {
                    // Go up to parent directory
                    if let Some(parent) = state.current_path.parent() {
                        match load_directory_contents(parent, args.all) {
                            Ok(new_files) => {
                                // Update navigation history
                                state
                                    .navigation_history
                                    .truncate(state.navigation_index + 1);
                                state.navigation_history.push(parent.to_path_buf());
                                state.navigation_index += 1;

                                state.current_path = parent.to_path_buf();
                                state.files = new_files;
                                state.selected_index = 0;
                            }
                            Err(e) => {
                                eprintln!("Error loading parent directory: {}", e);
                            }
                        }
                    }
                }
                KeyCode::Right => {
                    // Go forward through navigation history
                    if state.navigation_index + 1 < state.navigation_history.len() {
                        state.navigation_index += 1;
                        let forward_path = &state.navigation_history[state.navigation_index];
                        match load_directory_contents(forward_path, args.all) {
                            Ok(new_files) => {
                                state.current_path = forward_path.clone();
                                state.files = new_files;
                                state.selected_index = 0;
                            }
                            Err(e) => {
                                eprintln!("Error loading forward directory: {}", e);
                            }
                        }
                    }
                }
                _ => {}
            }
        }
    }

    Ok(())
}

/// Load directory contents
fn load_directory_contents(path: &Path, show_hidden: bool) -> Result<Vec<FileInfo>> {
    let mut files = Vec::new();

    for entry in fs::read_dir(path)
        .with_context(|| format!("Failed to read directory {}", path.display()))?
    {
        let entry = entry?;
        let path = entry.path();

        // Skip hidden files unless show_hidden is true
        if !show_hidden
            && path
                .file_name()
                .map(|n| n.to_string_lossy().starts_with('.'))
                .unwrap_or(false)
        {
            continue;
        }

        files.push(FileInfo::new(&path)?);
    }

    Ok(files)
}

/// UI rendering
fn ui(f: &mut Frame, state: &AppState) {
    let size = f.size();

    // Create header with current path and sort info
    let header = Paragraph::new(format!(
        "DIR: {} | Sort: {:?} | {} files",
        state.current_path.display(),
        state.sort_mode,
        state.files.len()
    ))
    .style(Style::default().fg(Color::White).bg(Color::Blue))
    .alignment(Alignment::Left);

    f.render_widget(header, Rect::new(0, 0, size.width, 3));

    // Calculate visible items and scrollbar
    let list_height = (size.height - 10) as usize;
    let total_items = state.files.len();
    let visible_items = list_height.min(total_items);

    // Use the scroll offset from state
    let scroll_offset = state.scroll_offset;

    // Create file list with scrolling
    let items: Vec<ListItem> = state
        .files
        .iter()
        .skip(scroll_offset)
        .take(visible_items)
        .enumerate()
        .map(|(i, file)| {
            let absolute_index = scroll_offset + i;
            let content = Line::from(vec![
                Span::styled(
                    get_file_type_indicator(file.is_dir),
                    Style::default().fg(Color::Yellow),
                ),
                Span::raw(" "),
                Span::styled(
                    &file.display_name,
                    if absolute_index == state.selected_index {
                        Style::default().fg(Color::Black).bg(Color::Green)
                    } else if file.is_dir {
                        Style::default().fg(Color::Cyan)
                    } else {
                        Style::default()
                    },
                ),
                Span::raw(" "),
                Span::styled(
                    format_size(file.size, DECIMAL),
                    Style::default().fg(Color::Magenta),
                ),
                Span::raw(" "),
                Span::styled(
                    format_time(&file.modified),
                    Style::default().fg(Color::Green),
                ),
            ]);

            ListItem::new(content)
        })
        .collect();

    let list = List::new(items)
        .block(Block::default().borders(Borders::ALL).title(" Files "))
        .highlight_style(Style::default().bg(Color::Green).fg(Color::Black))
        .highlight_symbol(">> ");

    f.render_widget(list, Rect::new(1, 4, size.width - 2, size.height - 10));

    // Add scrollbar indicator
    if total_items > visible_items {
        let scrollbar_height = if total_items > 0 {
            ((visible_items as f32 / total_items as f32) * list_height as f32).round() as u16
        } else {
            0
        };
        let scrollbar_position = if total_items > 0 {
            ((scroll_offset as f32 / total_items as f32) * list_height as f32).round() as u16
        } else {
            0
        };

        let scrollbar = Block::default()
            .borders(Borders::NONE)
            .style(Style::default().bg(Color::Gray));

        f.render_widget(
            scrollbar,
            Rect::new(size.width - 1, 4 + scrollbar_position, 1, scrollbar_height),
        );
    }

    // Help text at bottom - fixed positioning
    let help_text = if state.show_help {
        vec![
            "Controls:",
            "↑/↓ - Navigate",
            "Enter - Select directory/file",
            "←/→ - Go back/forward",
            "n/s/d - Sort by Name/Size/Date",
            "h - Toggle help",
            "q/Esc - Quit",
        ]
        .join("\n")
    } else {
        "Press 'h' for help".to_string()
    };

    let help = Paragraph::new(help_text)
        .style(Style::default().fg(Color::White))
        .alignment(Alignment::Center)
        .wrap(ratatui::widgets::Wrap { trim: true });

    f.render_widget(help, Rect::new(0, size.height - 6, size.width, 6));
}

/// Application state
struct AppState {
    current_path: PathBuf,
    files: Vec<FileInfo>,
    selected_index: usize,
    show_help: bool,
    sort_mode: SortMode,
    navigation_history: Vec<PathBuf>,
    navigation_index: usize,
    scroll_offset: usize,
}

#[derive(Debug, Clone, Copy, PartialEq)]
enum SortMode {
    Name,
    Size,
    Date,
}
