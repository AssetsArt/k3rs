//! Ratatui-based TUI for `pm dev` — beautiful tabbed log viewer with live status.

use std::collections::VecDeque;
use std::io;
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};

use crossterm::event::{self, Event, KeyCode, KeyEventKind, KeyModifiers};
use crossterm::execute;
use crossterm::terminal::{
    EnterAlternateScreen, LeaveAlternateScreen, disable_raw_mode, enable_raw_mode,
};
use ratatui::Terminal;
use ratatui::backend::CrosstermBackend;
use ratatui::layout::{Constraint, Direction, Layout, Rect};
use ratatui::style::{Color, Modifier, Style, Stylize};
use ratatui::text::{Line, Span};
use ratatui::widgets::{
    Block, Borders, Paragraph, Scrollbar, ScrollbarOrientation, ScrollbarState, Tabs, Wrap,
};

// ── Constants ───────────────────────────────────────────────────

const MAX_LOG_LINES: usize = 5000;
const TICK_RATE: Duration = Duration::from_millis(100);

/// Colors matching the dev.rs ANSI colors but as ratatui Colors.
const TAB_COLORS: &[Color] = &[
    Color::Cyan,    // server
    Color::Yellow,  // agent
    Color::Magenta, // vpc
    Color::Green,   // ui
];

// ── Shared log buffer ───────────────────────────────────────────

/// Per-component log buffer shared between reader threads and the TUI.
#[derive(Clone)]
pub struct LogBuffer {
    inner: Arc<Mutex<LogBufferInner>>,
}

struct LogBufferInner {
    lines: VecDeque<LogLine>,
    total_lines: usize,
}

struct LogLine {
    content: String,
    is_stderr: bool,
}

impl LogBuffer {
    pub fn new() -> Self {
        Self {
            inner: Arc::new(Mutex::new(LogBufferInner {
                lines: VecDeque::with_capacity(MAX_LOG_LINES),
                total_lines: 0,
            })),
        }
    }

    pub fn push(&self, line: String, is_stderr: bool) {
        let mut inner = self.inner.lock().unwrap();
        if inner.lines.len() >= MAX_LOG_LINES {
            inner.lines.pop_front();
        }
        inner.lines.push_back(LogLine {
            content: line,
            is_stderr,
        });
        inner.total_lines += 1;
    }

    fn snapshot(&self) -> Vec<(String, bool)> {
        let inner = self.inner.lock().unwrap();
        inner
            .lines
            .iter()
            .map(|l| (l.content.clone(), l.is_stderr))
            .collect()
    }

    fn len(&self) -> usize {
        self.inner.lock().unwrap().lines.len()
    }

    fn total(&self) -> usize {
        self.inner.lock().unwrap().total_lines
    }
}

// ── Component info ──────────────────────────────────────────────

pub struct ComponentInfo {
    pub label: String,
    pub url: String,
    pub pid: Option<u32>,
    pub color_idx: usize,
    pub buffer: LogBuffer,
    pub status: ComponentStatus,
}

#[derive(Clone, Copy, PartialEq)]
#[allow(dead_code)]
pub enum ComponentStatus {
    Starting,
    Running,
    Rebuilding,
    Crashed,
    Stopped,
}

impl ComponentStatus {
    fn icon(&self) -> &str {
        match self {
            Self::Starting => "◎",
            Self::Running => "●",
            Self::Rebuilding => "↻",
            Self::Crashed => "✕",
            Self::Stopped => "○",
        }
    }

    fn color(&self) -> Color {
        match self {
            Self::Starting => Color::Yellow,
            Self::Running => Color::Green,
            Self::Rebuilding => Color::Yellow,
            Self::Crashed => Color::Red,
            Self::Stopped => Color::DarkGray,
        }
    }
}

// ── App state ───────────────────────────────────────────────────

pub struct App {
    pub components: Arc<Mutex<Vec<ComponentInfo>>>,
    selected_tab: usize,
    scroll_offset: usize,
    follow_mode: bool, // auto-scroll to bottom
}

impl App {
    pub fn new(components: Arc<Mutex<Vec<ComponentInfo>>>) -> Self {
        Self {
            components,
            selected_tab: 0,
            scroll_offset: 0,
            follow_mode: true,
        }
    }

    fn tab_count(&self) -> usize {
        let comps = self.components.lock().unwrap();
        comps.len() + 1 // +1 for "All" tab
    }

    fn next_tab(&mut self) {
        let count = self.tab_count();
        self.selected_tab = (self.selected_tab + 1) % count;
        self.scroll_offset = 0;
        self.follow_mode = true;
    }

    fn prev_tab(&mut self) {
        let count = self.tab_count();
        self.selected_tab = (self.selected_tab + count - 1) % count;
        self.scroll_offset = 0;
        self.follow_mode = true;
    }

    fn select_tab(&mut self, idx: usize) {
        let count = self.tab_count();
        if idx < count {
            self.selected_tab = idx;
            self.scroll_offset = 0;
            self.follow_mode = true;
        }
    }
}

// ── Main TUI run loop ───────────────────────────────────────────

/// Run the ratatui TUI. Blocks until user presses `q` or `Ctrl+C`.
/// Returns `true` if the user pressed `q/Esc`, `false` if Ctrl+C.
pub fn run_tui(app: &mut App) -> io::Result<bool> {
    // Setup terminal
    enable_raw_mode()?;
    let mut stdout = io::stdout();
    execute!(stdout, EnterAlternateScreen)?;
    let backend = CrosstermBackend::new(stdout);
    let mut terminal = Terminal::new(backend)?;
    terminal.clear()?;

    let result = run_loop(&mut terminal, app);

    // Restore terminal
    disable_raw_mode()?;
    execute!(terminal.backend_mut(), LeaveAlternateScreen)?;
    terminal.show_cursor()?;

    result
}

fn run_loop(
    terminal: &mut Terminal<CrosstermBackend<io::Stdout>>,
    app: &mut App,
) -> io::Result<bool> {
    let mut last_draw = Instant::now();

    loop {
        // Draw
        if last_draw.elapsed() >= Duration::from_millis(50) {
            terminal.draw(|f| draw(f, app))?;
            last_draw = Instant::now();
        }

        // Poll events
        if event::poll(TICK_RATE)? {
            if let Event::Key(key) = event::read()? {
                if key.kind != KeyEventKind::Press {
                    continue;
                }

                match key.code {
                    KeyCode::Char('q') | KeyCode::Esc => return Ok(true),
                    KeyCode::Char('c') if key.modifiers.contains(KeyModifiers::CONTROL) => {
                        return Ok(false);
                    }
                    KeyCode::Tab | KeyCode::Right => app.next_tab(),
                    KeyCode::BackTab | KeyCode::Left => app.prev_tab(),
                    KeyCode::Char('1') => app.select_tab(0),
                    KeyCode::Char('2') => app.select_tab(1),
                    KeyCode::Char('3') => app.select_tab(2),
                    KeyCode::Char('4') => app.select_tab(3),
                    KeyCode::Char('0') | KeyCode::Char('a') => {
                        let count = app.tab_count();
                        app.select_tab(count - 1); // "All" is last
                    }
                    KeyCode::Up | KeyCode::Char('k') => {
                        app.follow_mode = false;
                        app.scroll_offset = app.scroll_offset.saturating_add(3);
                    }
                    KeyCode::Down | KeyCode::Char('j') => {
                        app.follow_mode = false;
                        if app.scroll_offset > 0 {
                            app.scroll_offset = app.scroll_offset.saturating_sub(3);
                        } else {
                            app.follow_mode = true;
                        }
                    }
                    KeyCode::Char('f') | KeyCode::End => {
                        app.follow_mode = true;
                        app.scroll_offset = 0;
                    }
                    KeyCode::Char('g') | KeyCode::Home => {
                        app.follow_mode = false;
                        // Set to max scroll
                        app.scroll_offset = usize::MAX;
                    }
                    KeyCode::PageUp => {
                        app.follow_mode = false;
                        app.scroll_offset = app.scroll_offset.saturating_add(30);
                    }
                    KeyCode::PageDown => {
                        app.follow_mode = false;
                        if app.scroll_offset > 0 {
                            app.scroll_offset = app.scroll_offset.saturating_sub(30);
                        } else {
                            app.follow_mode = true;
                        }
                    }
                    _ => {}
                }
            }
        }
    }
}

// ── Drawing ─────────────────────────────────────────────────────

fn draw(f: &mut ratatui::Frame, app: &mut App) {
    let area = f.area();

    // Layout: [status bar 1] [tabs 3] [logs flex] [help bar 1]
    let chunks = Layout::default()
        .direction(Direction::Vertical)
        .constraints([
            Constraint::Length(3), // Status bar
            Constraint::Length(3), // Tabs
            Constraint::Min(5),    // Log area
            Constraint::Length(1), // Help bar
        ])
        .split(area);

    draw_status_bar(f, app, chunks[0]);
    draw_tabs(f, app, chunks[1]);
    draw_logs(f, app, chunks[2]);
    draw_help_bar(f, app, chunks[3]);
}

fn draw_status_bar(f: &mut ratatui::Frame, app: &App, area: Rect) {
    let comps = app.components.lock().unwrap();

    let mut spans = vec![
        Span::styled(
            " k3rs dev ",
            Style::default()
                .fg(Color::Cyan)
                .add_modifier(Modifier::BOLD),
        ),
        Span::styled(" │ ", Style::default().fg(Color::DarkGray)),
    ];

    for (i, comp) in comps.iter().enumerate() {
        let status = comp.status;
        let color = if status == ComponentStatus::Running {
            TAB_COLORS[comp.color_idx % TAB_COLORS.len()]
        } else {
            status.color()
        };

        spans.push(Span::styled(
            format!("{}", status.icon()),
            Style::default().fg(status.color()),
        ));
        spans.push(Span::styled(
            format!(" {} ", comp.label),
            Style::default().fg(color),
        ));

        if let Some(pid) = comp.pid {
            spans.push(Span::styled(
                format!(":{} ", pid),
                Style::default().fg(Color::DarkGray),
            ));
        }

        if i < comps.len() - 1 {
            spans.push(Span::styled("│ ", Style::default().fg(Color::DarkGray)));
        }
    }

    // Add total lines count
    let total: usize = comps.iter().map(|c| c.buffer.total()).sum();
    spans.push(Span::styled(
        format!("  {} lines", total),
        Style::default().fg(Color::DarkGray),
    ));

    let status_bar = Paragraph::new(Line::from(spans)).block(
        Block::default()
            .borders(Borders::ALL)
            .border_style(Style::default().fg(Color::DarkGray))
            .title_top(
                Line::from(vec![
                    Span::styled(" ⚡ ", Style::default().fg(Color::Yellow)),
                    Span::styled("k3rs ", Style::default().fg(Color::Cyan).bold()),
                    Span::styled("process manager ", Style::default().fg(Color::White)),
                ])
                .centered(),
            ),
    );
    f.render_widget(status_bar, area);
}

fn draw_tabs(f: &mut ratatui::Frame, app: &App, area: Rect) {
    let comps = app.components.lock().unwrap();

    let mut titles: Vec<Line> = comps
        .iter()
        .enumerate()
        .map(|(i, comp)| {
            let color = TAB_COLORS[comp.color_idx % TAB_COLORS.len()];
            let buf_len = comp.buffer.len();
            Line::from(vec![
                Span::styled(format!(" {} ", i + 1), Style::default().fg(Color::DarkGray)),
                Span::styled(
                    comp.label.clone(),
                    Style::default().fg(color).add_modifier(Modifier::BOLD),
                ),
                Span::styled(
                    format!(" ({})", buf_len),
                    Style::default().fg(Color::DarkGray),
                ),
            ])
        })
        .collect();

    // "All" tab
    let all_count: usize = comps.iter().map(|c| c.buffer.len()).sum();
    titles.push(Line::from(vec![
        Span::styled(" 0 ", Style::default().fg(Color::DarkGray)),
        Span::styled(
            "All",
            Style::default()
                .fg(Color::White)
                .add_modifier(Modifier::BOLD),
        ),
        Span::styled(
            format!(" ({})", all_count),
            Style::default().fg(Color::DarkGray),
        ),
    ]));

    let tabs = Tabs::new(titles)
        .select(app.selected_tab)
        .highlight_style(
            Style::default()
                .add_modifier(Modifier::BOLD)
                .add_modifier(Modifier::REVERSED),
        )
        .divider(Span::styled(" │ ", Style::default().fg(Color::DarkGray)))
        .block(
            Block::default()
                .borders(Borders::LEFT | Borders::RIGHT)
                .border_style(Style::default().fg(Color::DarkGray)),
        );

    f.render_widget(tabs, area);
}

fn draw_logs(f: &mut ratatui::Frame, app: &mut App, area: Rect) {
    let comps = app.components.lock().unwrap();
    let comp_count = comps.len();
    let is_all_tab = app.selected_tab >= comp_count;

    // Collect log lines
    let log_lines: Vec<Line> = if is_all_tab {
        // Merge all logs — interleave by showing each buffer's snapshot
        // For simplicity, we snapshot each and merge by order
        let mut all: Vec<(String, Color, bool)> = Vec::new();
        for comp in comps.iter() {
            let color = TAB_COLORS[comp.color_idx % TAB_COLORS.len()];
            let snapshot = comp.buffer.snapshot();
            for (line, is_stderr) in snapshot {
                all.push((format!("{:>8} │ {}", comp.label, line), color, is_stderr));
            }
        }
        // We can't perfectly interleave without timestamps, so we just concatenate
        // In practice the thread ordering gives a rough interleave already
        all.into_iter()
            .map(|(content, color, is_stderr)| {
                let style = if is_stderr {
                    Style::default().fg(Color::Red)
                } else {
                    Style::default().fg(color)
                };
                Line::from(Span::styled(content, style))
            })
            .collect()
    } else if app.selected_tab < comp_count {
        let comp = &comps[app.selected_tab];
        let color = TAB_COLORS[comp.color_idx % TAB_COLORS.len()];
        comp.buffer
            .snapshot()
            .into_iter()
            .map(|(content, is_stderr)| {
                let style = if is_stderr {
                    Style::default().fg(Color::Red)
                } else {
                    Style::default().fg(color)
                };
                Line::from(Span::styled(content, style))
            })
            .collect()
    } else {
        vec![]
    };

    let total_lines = log_lines.len();
    let visible_height = area.height.saturating_sub(2) as usize; // borders

    // Clamp scroll offset
    if app.scroll_offset > total_lines.saturating_sub(visible_height) {
        app.scroll_offset = total_lines.saturating_sub(visible_height);
    }

    // Calculate scroll position (scroll_offset is from bottom)
    let scroll_pos = if app.follow_mode || app.scroll_offset == 0 {
        total_lines.saturating_sub(visible_height)
    } else {
        total_lines
            .saturating_sub(visible_height)
            .saturating_sub(app.scroll_offset)
    };

    // Build title with info
    let title = if is_all_tab {
        " All Components ".to_string()
    } else if app.selected_tab < comp_count {
        let comp = &comps[app.selected_tab];
        let url_info = if comp.url.is_empty() {
            String::new()
        } else {
            format!(" — {} ", comp.url)
        };
        format!(" {} {}", comp.label, url_info)
    } else {
        String::new()
    };

    let follow_indicator = if app.follow_mode {
        Span::styled(" ↓ FOLLOW ", Style::default().fg(Color::Green).bold())
    } else {
        Span::styled(
            format!(" ↑ +{} ", app.scroll_offset),
            Style::default().fg(Color::Yellow),
        )
    };

    let log_block = Block::default()
        .borders(Borders::ALL)
        .border_style(Style::default().fg(Color::DarkGray))
        .title_top(Line::from(vec![Span::styled(
            title,
            Style::default().fg(Color::White).bold(),
        )]))
        .title_bottom(Line::from(vec![follow_indicator]).right_aligned());

    let paragraph = Paragraph::new(log_lines)
        .block(log_block)
        .wrap(Wrap { trim: false })
        .scroll((scroll_pos as u16, 0));

    f.render_widget(paragraph, area);

    // Scrollbar
    if total_lines > visible_height {
        let mut scrollbar_state = ScrollbarState::new(total_lines)
            .position(scroll_pos)
            .viewport_content_length(visible_height);

        f.render_stateful_widget(
            Scrollbar::new(ScrollbarOrientation::VerticalRight)
                .begin_symbol(Some("▲"))
                .end_symbol(Some("▼"))
                .track_symbol(Some("│"))
                .thumb_symbol("█"),
            area.inner(ratatui::layout::Margin {
                vertical: 1,
                horizontal: 0,
            }),
            &mut scrollbar_state,
        );
    }
}

fn draw_help_bar(f: &mut ratatui::Frame, _app: &App, area: Rect) {
    let help = Line::from(vec![
        Span::styled(" Tab", Style::default().fg(Color::Cyan).bold()),
        Span::styled("/", Style::default().fg(Color::DarkGray)),
        Span::styled("←→", Style::default().fg(Color::Cyan).bold()),
        Span::styled(" switch ", Style::default().fg(Color::Gray)),
        Span::styled("│", Style::default().fg(Color::DarkGray)),
        Span::styled(" ↑↓", Style::default().fg(Color::Cyan).bold()),
        Span::styled("/", Style::default().fg(Color::DarkGray)),
        Span::styled("jk", Style::default().fg(Color::Cyan).bold()),
        Span::styled(" scroll ", Style::default().fg(Color::Gray)),
        Span::styled("│", Style::default().fg(Color::DarkGray)),
        Span::styled(" f", Style::default().fg(Color::Cyan).bold()),
        Span::styled(" follow ", Style::default().fg(Color::Gray)),
        Span::styled("│", Style::default().fg(Color::DarkGray)),
        Span::styled(" 1-4", Style::default().fg(Color::Cyan).bold()),
        Span::styled(" tab ", Style::default().fg(Color::Gray)),
        Span::styled("│", Style::default().fg(Color::DarkGray)),
        Span::styled(" 0", Style::default().fg(Color::Cyan).bold()),
        Span::styled("/", Style::default().fg(Color::DarkGray)),
        Span::styled("a", Style::default().fg(Color::Cyan).bold()),
        Span::styled(" all ", Style::default().fg(Color::Gray)),
        Span::styled("│", Style::default().fg(Color::DarkGray)),
        Span::styled(" q", Style::default().fg(Color::Red).bold()),
        Span::styled(" quit ", Style::default().fg(Color::Gray)),
    ]);

    f.render_widget(Paragraph::new(help), area);
}
