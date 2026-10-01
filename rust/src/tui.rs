use ansi_to_tui::IntoText;
use anyhow::{anyhow, Result};
use crossterm::event::{
    self, DisableMouseCapture, EnableMouseCapture, Event, KeyCode, KeyEventKind, KeyModifiers,
    KeyboardEnhancementFlags, MouseButton, MouseEventKind, PopKeyboardEnhancementFlags,
    PushKeyboardEnhancementFlags,
};
use crossterm::terminal::{disable_raw_mode, enable_raw_mode};
use crossterm::{execute, terminal};
use ratatui::backend::CrosstermBackend;
use ratatui::layout::{Constraint, Direction, Layout, Position, Rect};
use ratatui::style::{Color, Modifier, Style};
use ratatui::text::{Line, Span, Text};
use ratatui::widgets::{Block, Borders, List, ListItem, ListState, Paragraph, Wrap};
use ratatui::{Frame, Terminal};
use ratatui_core::layout::Alignment as CoreAlignment;
use ratatui_core::style::{Color as CoreColor, Modifier as CoreModifier, Style as CoreStyle};
use ratatui_core::text::{Line as CoreLine, Span as CoreSpan, Text as CoreText};
use std::fs;
use std::io::{stdout, Stdout, Write};
use std::path::{Path, PathBuf};
use std::process::Command;
use std::time::{Duration, Instant};
use syntect::easy::HighlightLines;
use syntect::highlighting::ThemeSet;
use syntect::parsing::SyntaxSet;
use syntect::util::as_24_bit_terminal_escaped;
use tempfile::TempDir;

use crate::config::ResolvedConfig;
use crate::hyprlock;
use crate::paths::{normalize_theme_name, title_case_theme};
use crate::presets;
use crate::preview;
use crate::starship;
use crate::theme_ops;
use crate::unlock;
use crate::walker;
use crate::waybar;

const APP_TITLE: &str = concat!("ChromaCon Style Manager v", env!("THEME_MANAGER_VERSION"));
const NO_THEME_CHANGE_VALUE: &str = "__no_theme_change__";
const NO_THEME_CHANGE_LABEL: &str = "No theme change";

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum FocusArea {
    List,
    Code,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum BrowseTab {
    Theme,
    Waybar,
    Walker,
    Hyprlock,
    Unlock,
    Starship,
    Presets,
    Review,
}

#[derive(Debug)]
pub struct BrowseSelection {
    pub theme: String,
    pub no_theme_change: bool,
    pub waybar: WaybarSelection,
    pub walker: WalkerSelection,
    pub hyprlock: HyprlockSelection,
    pub unlock: UnlockSelection,
    pub starship: StarshipSelection,
}

#[derive(Debug)]
pub enum WaybarSelection {
    NoChange,
    None,
    Auto,
    Named(String),
}

#[derive(Debug)]
pub enum WalkerSelection {
    NoChange,
    None,
    Auto,
    Named(String),
}

#[derive(Debug)]
pub enum HyprlockSelection {
    NoChange,
    None,
    Auto,
    Named(String),
}

#[derive(Debug)]
pub enum UnlockSelection {
    NoChange,
    Default,
    Named(String),
}

#[derive(Debug)]
pub enum StarshipSelection {
    NoChange,
    None,
    Preset(String),
    Named(String),
    Theme(PathBuf),
}

struct PickerState {
    list_state: ListState,
    last_code_index: Option<usize>,
    last_code: Text<'static>,
    last_preview_index: Option<usize>,
    last_preview: Option<PathBuf>,
    preview_dirty: bool,
    last_preview_text: Text<'static>,
    last_image_area: Option<Rect>,
    code_scroll: u16,
    focus: FocusArea,
    image_visible: bool,
    force_clear: bool,
    search_query: String,
    last_query: String,
    filtered_indices: Vec<usize>,
    last_selected: Option<usize>,
    collapsed_groups: std::collections::HashSet<String>,
}

impl PickerState {
    fn new() -> Self {
        let mut list_state = ListState::default();
        list_state.select(Some(0));
        Self {
            list_state,
            last_code_index: None,
            last_code: Text::from("Loading preview..."),
            last_preview_index: None,
            last_preview: None,
            preview_dirty: false,
            last_preview_text: Text::default(),
            last_image_area: None,
            code_scroll: 0,
            focus: FocusArea::List,
            image_visible: false,
            force_clear: false,
            search_query: String::new(),
            last_query: String::new(),
            filtered_indices: Vec::new(),
            last_selected: None,
            collapsed_groups: std::collections::HashSet::new(),
        }
    }
}

struct PreviewBackend {
    kind: PreviewBackendKind,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum PreviewBackendKind {
    Kitty,
    Sixel,
    Chafa,
    None,
}

impl PreviewBackend {
    fn detect() -> Self {
        PreviewBackend {
            kind: detect_preview_backend_kind(
                command_exists("kitty"),
                command_exists("chafa"),
                is_kitty_terminal(),
                is_foot_terminal(),
            ),
        }
    }

    fn render(&self, path: Option<&Path>, rect: Rect) {
        match self.kind {
            PreviewBackendKind::Kitty => {
                if let Some(path) = path {
                    let place = format!("{}x{}@{}x{}", rect.width, rect.height, rect.x, rect.y);
                    let _ = Command::new("kitty")
                        .args([
                            "+kitten",
                            "icat",
                            "--clear",
                            "--transfer-mode=stream",
                            "--stdin=no",
                            "--place",
                            &place,
                            path.to_string_lossy().as_ref(),
                        ])
                        .status();
                } else {
                    let _ = Command::new("kitty")
                        .args(["+kitten", "icat", "--clear", "--stdin=no"])
                        .status();
                }
            }
            PreviewBackendKind::Sixel => {
                clear_preview_rect(rect);
                if let Some(path) = path {
                    render_sixel_preview(path, rect);
                }
            }
            _ => {}
        }
    }

    fn text_preview(&self, path: Option<&Path>, rect: Rect) -> Text<'_> {
        match self.kind {
            PreviewBackendKind::Kitty | PreviewBackendKind::Sixel => {
                if path.is_some() {
                    Text::from("")
                } else {
                    Text::from("No preview available.")
                }
            }
            PreviewBackendKind::Chafa => {
                if let Some(path) = path {
                    let size = format!("{}x{}", rect.width.max(1), rect.height.max(1));
                    if let Ok(output) = Command::new("chafa")
                        .args([
                            "--format=symbols",
                            "--size",
                            &size,
                            path.to_string_lossy().as_ref(),
                        ])
                        .output()
                    {
                        if output.status.success() {
                            match output.stdout.as_slice().into_text() {
                                Ok(text) => return convert_text(text),
                                Err(_) => {
                                    if let Ok(rendered) = String::from_utf8(output.stdout) {
                                        return Text::from(rendered);
                                    }
                                }
                            }
                        }
                    }
                }
                Text::from("No preview available.")
            }
            _ => {
                if let Some(path) = path {
                    Text::from(path.to_string_lossy().to_string())
                } else {
                    Text::from("No preview available.")
                }
            }
        }
    }
}

fn detect_preview_backend_kind(
    has_kitty: bool,
    has_chafa: bool,
    is_kitty_term: bool,
    is_foot_term: bool,
) -> PreviewBackendKind {
    if has_kitty && is_kitty_term {
        PreviewBackendKind::Kitty
    } else if has_chafa && is_foot_term {
        PreviewBackendKind::Sixel
    } else if has_chafa {
        PreviewBackendKind::Chafa
    } else {
        PreviewBackendKind::None
    }
}

fn is_kitty_terminal() -> bool {
    std::env::var("KITTY_WINDOW_ID").is_ok() || term_contains("kitty") || term_contains("ghostty")
}

fn is_foot_terminal() -> bool {
    term_contains("foot") || term_program_contains("foot")
}

fn render_sixel_preview(path: &Path, rect: Rect) {
    let size = format!("{}x{}", rect.width.max(1), rect.height.max(1));
    if let Ok(output) = Command::new("chafa")
        .args([
            "--format=sixels",
            "--size",
            &size,
            path.to_string_lossy().as_ref(),
        ])
        .output()
    {
        if output.status.success() {
            let mut out = stdout();
            let row = rect.y.saturating_add(1);
            let col = rect.x.saturating_add(1);
            let _ = write!(out, "\x1b[{};{}H", row, col);
            let _ = out.write_all(&output.stdout);
            let _ = out.flush();
        }
    }
}

fn clear_preview_rect(rect: Rect) {
    if rect.width == 0 || rect.height == 0 {
        return;
    }
    let mut out = stdout();
    let row_start = rect.y.saturating_add(1);
    let col_start = rect.x.saturating_add(1);
    let blank = " ".repeat(rect.width as usize);
    for offset in 0..rect.height {
        let row = row_start.saturating_add(offset);
        let _ = write!(out, "\x1b[{};{}H{}", row, col_start, blank);
    }
    let _ = out.flush();
}

pub fn browse(config: &ResolvedConfig, quiet: bool) -> Result<Option<BrowseSelection>> {
    if quiet {
        // currently unused, but reserved for future use
    }
    let discovered_themes = theme_ops::list_theme_entries_with_groups(config)?;
    if discovered_themes.is_empty() {
        return Err(anyhow!("no themes available"));
    }

    let theme_items: Vec<OptionItem> = std::iter::once(OptionItem {
        label: NO_THEME_CHANGE_LABEL.to_string(),
        value: NO_THEME_CHANGE_VALUE.to_string(),
        preview: None,
        group: None,
    })
    .chain(discovered_themes.into_iter().map(|entry| {
        let label = title_case_theme(&entry.name);
        let preview_path = preview::find_theme_preview(&entry.path);
        OptionItem {
            label,
            value: entry.name,
            preview: preview_path,
            group: entry.group,
        }
    }))
    .collect();

    let backend = PreviewBackend::detect();
    let mut terminal = setup_terminal()?;
    let mut tab = BrowseTab::Theme;
    let tab_titles = [
        "Theme", "Waybar", "Walker", "Hyprlock", "Unlock", "Starship", "Review", "Presets",
    ];
    let mut tab_ranges: Vec<(u16, u16, usize)> = Vec::new();
    let mut active_search_area = Rect::ZERO;
    let mut active_list_inner = Rect::ZERO;
    let mut active_code_inner = Rect::ZERO;
    let mut active_code_area = Rect::ZERO;
    let mut tab_area = Rect::ZERO;
    let mut status_area = Rect::ZERO;
    let mut last_repeat_key: Option<(KeyCode, KeyModifiers)> = None;
    let mut last_repeat_at = Instant::now();
    let mut last_press_key: Option<(KeyCode, KeyModifiers, Instant)> = None;
    let mut status_message = String::new();
    let mut status_tab = BrowseTab::Theme;
    let mut status_at = Instant::now();
    let mut preset_save_active = false;
    let mut preset_save_input = String::new();

    let mut theme_state = PickerState::new();
    let initial_theme_value = crate::paths::current_theme_name(&config.current_theme_link)
        .ok()
        .flatten()
        .unwrap_or_else(|| NO_THEME_CHANGE_VALUE.to_string());
    // Groups start collapsed, except whichever one holds the already-active
    // theme, so the current selection is visible without extra keystrokes.
    theme_state.collapsed_groups = initial_collapsed_groups(&theme_items, Some(&initial_theme_value));
    rebuild_filtered(&mut theme_state, &theme_items);
    select_option_by_value(&mut theme_state, &theme_items, &initial_theme_value);
    let mut selected_theme = current_theme_value(&theme_items, &theme_state)
        .ok_or_else(|| anyhow!("no themes available"))?;
    let mut theme_path = resolve_theme_path_for_selection(config, &selected_theme)?;

    let mut waybar_items = build_waybar_items(config, &theme_path)?;
    let mut walker_items = build_walker_items(config, &theme_path)?;
    let mut hyprlock_items = build_hyprlock_items(config, &theme_path)?;
    let unlock_items = build_unlock_items(config)?;
    let mut starship_items = build_starship_items(config, &theme_path)?;
    let mut waybar_state = PickerState::new();
    let mut walker_state = PickerState::new();
    let mut hyprlock_state = PickerState::new();
    let mut unlock_state = PickerState::new();
    let mut starship_state = PickerState::new();
    // Waybar/Walker always start on their ungrouped "No change" option, so
    // every group can safely start collapsed with nothing hidden from view.
    waybar_state.collapsed_groups = all_group_names(&waybar_items);
    walker_state.collapsed_groups = all_group_names(&walker_items);
    rebuild_filtered(&mut waybar_state, &waybar_items);
    rebuild_filtered(&mut walker_state, &walker_items);
    rebuild_filtered(&mut hyprlock_state, &hyprlock_items);
    rebuild_filtered(&mut unlock_state, &unlock_items);
    rebuild_filtered(&mut starship_state, &starship_items);

    let mut preset_file = presets::load_presets()?;
    let mut preset_items = build_preset_items(&preset_file);
    let mut preset_state = PickerState::new();
    rebuild_filtered(&mut preset_state, &preset_items);

    loop {
        terminal.draw(|frame| {
            let size = frame.area();
            let chunks = Layout::default()
                .direction(Direction::Vertical)
                .constraints([
                    Constraint::Length(2),
                    Constraint::Min(0),
                    Constraint::Length(1),
                ])
                .split(size);
            tab_area = chunks[0];
            let content_area = chunks[1];
            status_area = chunks[2];

            render_tab_bar(frame, tab_area, &tab_titles, tab, &mut tab_ranges);
            let status_active =
                !status_message.is_empty() && status_at.elapsed() < Duration::from_millis(1200);

            match tab {
                BrowseTab::Theme => {
                    let areas = render_picker(
                        frame,
                        content_area,
                        "Select theme",
                        "Image Preview",
                        &theme_items,
                        &mut theme_state,
                        &backend,
                        |idx| {
                            if theme_items[idx].value == NO_THEME_CHANGE_VALUE {
                                return Text::from("Keeping current theme.");
                            }
                            match theme_ops::resolve_theme_path(config, &theme_items[idx].value) {
                                Ok(theme_path) => load_code_preview(
                                    "hyprland.conf",
                                    theme_path.join("hyprland.conf"),
                                    "conf",
                                ),
                                Err(_) => Text::from("Theme preview unavailable."),
                            }
                        },
                        |idx| theme_items[idx].preview.clone(),
                        |_idx| None,
                        true,
                        if status_active && status_tab == BrowseTab::Theme {
                            Some(status_message.as_str())
                        } else {
                            None
                        },
                    );
                    active_search_area = areas.search_area;
                    active_list_inner = areas.list_inner;
                    active_code_inner = areas.code_inner;
                    active_code_area = areas.code_area;
                }
                BrowseTab::Waybar => {
                    let areas = render_picker(
                        frame,
                        content_area,
                        "Select Waybar",
                        "Image Preview",
                        &waybar_items,
                        &mut waybar_state,
                        &backend,
                        |idx| build_waybar_code_preview(config, &theme_path, &waybar_items[idx]),
                        |idx| waybar_items[idx].preview.clone(),
                        |_idx| None,
                        true,
                        if status_active && status_tab == BrowseTab::Waybar {
                            Some(status_message.as_str())
                        } else {
                            None
                        },
                    );
                    active_search_area = areas.search_area;
                    active_list_inner = areas.list_inner;
                    active_code_inner = areas.code_inner;
                    active_code_area = areas.code_area;
                }
                BrowseTab::Walker => {
                    let areas = render_picker(
                        frame,
                        content_area,
                        "Select Walker",
                        "Image Preview",
                        &walker_items,
                        &mut walker_state,
                        &backend,
                        |idx| build_walker_code_preview(config, &theme_path, &walker_items[idx]),
                        |idx| walker_items[idx].preview.clone(),
                        |_idx| None,
                        true,
                        if status_active && status_tab == BrowseTab::Walker {
                            Some(status_message.as_str())
                        } else {
                            None
                        },
                    );
                    active_search_area = areas.search_area;
                    active_list_inner = areas.list_inner;
                    active_code_inner = areas.code_inner;
                    active_code_area = areas.code_area;
                }
                BrowseTab::Hyprlock => {
                    let areas = render_picker(
                        frame,
                        content_area,
                        "Select Hyprlock",
                        "Image Preview",
                        &hyprlock_items,
                        &mut hyprlock_state,
                        &backend,
                        |idx| {
                            build_hyprlock_code_preview(config, &theme_path, &hyprlock_items[idx])
                        },
                        |idx| hyprlock_items[idx].preview.clone(),
                        |_idx| None,
                        true,
                        if status_active && status_tab == BrowseTab::Hyprlock {
                            Some(status_message.as_str())
                        } else {
                            None
                        },
                    );
                    active_search_area = areas.search_area;
                    active_list_inner = areas.list_inner;
                    active_code_inner = areas.code_inner;
                    active_code_area = areas.code_area;
                }
                BrowseTab::Unlock => {
                    let areas = render_picker(
                        frame,
                        content_area,
                        "Select Unlock",
                        "Image Preview",
                        &unlock_items,
                        &mut unlock_state,
                        &backend,
                        |idx| build_unlock_code_preview(config, &unlock_items[idx]),
                        |idx| unlock_items[idx].preview.clone(),
                        |_idx| None,
                        true,
                        if status_active && status_tab == BrowseTab::Unlock {
                            Some(status_message.as_str())
                        } else {
                            None
                        },
                    );
                    active_search_area = areas.search_area;
                    active_list_inner = areas.list_inner;
                    active_code_inner = areas.code_inner;
                    active_code_area = areas.code_area;
                }
                BrowseTab::Starship => {
                    let areas = render_picker(
                        frame,
                        content_area,
                        "Select Starship",
                        "Prompt Preview",
                        &starship_items,
                        &mut starship_state,
                        &backend,
                        |idx| {
                            build_starship_code_preview(config, &theme_path, &starship_items[idx])
                        },
                        |_idx| None,
                        |idx| {
                            Some(build_starship_prompt_preview(
                                config,
                                &theme_path,
                                &starship_items[idx],
                            ))
                        },
                        false,
                        if status_active && status_tab == BrowseTab::Starship {
                            Some(status_message.as_str())
                        } else {
                            None
                        },
                    );
                    active_search_area = areas.search_area;
                    active_list_inner = areas.list_inner;
                    active_code_inner = areas.code_inner;
                    active_code_area = areas.code_area;
                }
                BrowseTab::Presets => {
                    let areas = render_preset_picker(
                        frame,
                        content_area,
                        &preset_items,
                        &mut preset_state,
                        |idx| preset_summary_text(config, &preset_file, &preset_items[idx]),
                        if status_active && status_tab == BrowseTab::Presets {
                            Some(status_message.as_str())
                        } else {
                            None
                        },
                    );
                    active_search_area = areas.search_area;
                    active_list_inner = areas.list_inner;
                    active_code_inner = areas.code_inner;
                    active_code_area = areas.code_area;
                }
                BrowseTab::Review => {
                    active_search_area = Rect::ZERO;
                    active_list_inner = Rect::ZERO;
                    active_code_inner = Rect::ZERO;
                    active_code_area = Rect::ZERO;
                    render_review(
                        frame,
                        content_area,
                        &selected_theme,
                        current_waybar_label(&waybar_items, &waybar_state),
                        current_walker_label(&walker_items, &walker_state),
                        current_hyprlock_label(&hyprlock_items, &hyprlock_state),
                        current_unlock_label(&unlock_items, &unlock_state),
                        current_starship_label(&starship_items, &starship_state),
                    );
                }
            }

            let grouped_tab = match tab {
                BrowseTab::Theme => grouping_active(&theme_items, &theme_state.search_query),
                BrowseTab::Waybar => grouping_active(&waybar_items, &waybar_state.search_query),
                BrowseTab::Walker => grouping_active(&walker_items, &walker_state.search_query),
                _ => false,
            };
            render_status_bar(
                frame,
                status_area,
                tab,
                &theme_label_for_display(&selected_theme),
                current_waybar_label(&waybar_items, &waybar_state),
                current_walker_label(&walker_items, &walker_state),
                current_hyprlock_label(&hyprlock_items, &hyprlock_state),
                current_unlock_label(&unlock_items, &unlock_state),
                current_starship_label(&starship_items, &starship_state),
                status_active.then_some(status_message.as_str()),
                preset_save_active,
                &preset_save_input,
                grouped_tab,
            );
        })?;

        if event::poll(Duration::from_millis(200))? {
            let mut handled_nav = false;
            'event_loop: loop {
                let next_event = event::read()?;
                match next_event {
                    Event::Key(key) => {
                        if key.kind == KeyEventKind::Release {
                            if !event::poll(Duration::from_millis(0))? {
                                break 'event_loop;
                            }
                            continue 'event_loop;
                        }
                        let is_nav_key = matches!(
                            key.code,
                            KeyCode::Up
                                | KeyCode::Down
                                | KeyCode::PageUp
                                | KeyCode::PageDown
                                | KeyCode::Home
                                | KeyCode::End
                        );
                        if is_nav_key {
                            if handled_nav {
                                if !event::poll(Duration::from_millis(0))? {
                                    break 'event_loop;
                                }
                                continue 'event_loop;
                            }
                            handled_nav = true;
                        }
                        let now = Instant::now();
                        if preset_save_active {
                            if key.kind == KeyEventKind::Repeat {
                                if !event::poll(Duration::from_millis(0))? {
                                    break 'event_loop;
                                }
                                continue 'event_loop;
                            }
                            match key.code {
                                KeyCode::Esc => {
                                    preset_save_active = false;
                                    preset_save_input.clear();
                                    status_tab = BrowseTab::Review;
                                    status_at = Instant::now();
                                    status_message = "Preset save canceled".to_string();
                                }
                                KeyCode::Enter => {
                                    let name = preset_save_input.trim();
                                    status_tab = BrowseTab::Review;
                                    status_at = Instant::now();
                                    if name.is_empty() {
                                        status_message = "Preset name required".to_string();
                                    } else {
                                        let entry = build_preset_entry_from_selection(
                                            &selected_theme,
                                            current_waybar_selection(&waybar_items, &waybar_state),
                                            current_walker_selection(&walker_items, &walker_state),
                                            current_hyprlock_selection(
                                                &hyprlock_items,
                                                &hyprlock_state,
                                            ),
                                            current_starship_selection(
                                                &starship_items,
                                                &starship_state,
                                                &theme_path,
                                            ),
                                        );
                                        match presets::save_preset(name, entry, config) {
                                            Ok(()) => {
                                                status_message = "Preset saved".to_string();
                                                preset_file = presets::load_presets()?;
                                                preset_items = build_preset_items(&preset_file);
                                                reset_picker_cache(&mut preset_state);
                                                rebuild_filtered(&mut preset_state, &preset_items);
                                                select_preset_by_name(
                                                    &mut preset_state,
                                                    &preset_items,
                                                    name,
                                                );
                                            }
                                            Err(err) => {
                                                status_message = err.to_string();
                                            }
                                        }
                                    }
                                    preset_save_active = false;
                                    preset_save_input.clear();
                                }
                                KeyCode::Backspace => {
                                    preset_save_input.pop();
                                }
                                KeyCode::Char('u')
                                    if key.modifiers.contains(KeyModifiers::CONTROL) =>
                                {
                                    preset_save_input.clear();
                                }
                                KeyCode::Char(ch) => {
                                    if !key.modifiers.contains(KeyModifiers::CONTROL)
                                        && !key.modifiers.contains(KeyModifiers::ALT)
                                    {
                                        preset_save_input.push(ch);
                                    }
                                }
                                _ => {}
                            }
                            if !event::poll(Duration::from_millis(0))? {
                                break 'event_loop;
                            }
                            continue 'event_loop;
                        }
                        let is_repeat = key.kind == event::KeyEventKind::Repeat;
                        if is_repeat {
                            if let Some((last_code, last_mod, last_at)) = last_press_key {
                                if last_code == key.code && last_mod == key.modifiers {
                                    if now.duration_since(last_at) < Duration::from_millis(150) {
                                        if !event::poll(Duration::from_millis(0))? {
                                            break 'event_loop;
                                        }
                                        continue 'event_loop;
                                    }
                                }
                            }
                            if let Some((last_code, last_mod)) = last_repeat_key {
                                if last_code == key.code && last_mod == key.modifiers {
                                    if now.duration_since(last_repeat_at)
                                        < Duration::from_millis(35)
                                    {
                                        if !event::poll(Duration::from_millis(0))? {
                                            break 'event_loop;
                                        }
                                        continue 'event_loop;
                                    }
                                }
                            }
                            last_repeat_key = Some((key.code, key.modifiers));
                            last_repeat_at = now;
                        } else {
                            last_press_key = Some((key.code, key.modifiers, now));
                        }
                        if let Some(state) = active_picker_mut(
                            tab,
                            &mut theme_state,
                            &mut waybar_state,
                            &mut walker_state,
                            &mut hyprlock_state,
                            &mut unlock_state,
                            &mut starship_state,
                            &mut preset_state,
                        ) {
                            if tab != BrowseTab::Review && state.focus == FocusArea::List {
                                let mut handled = false;
                                match key.code {
                                    KeyCode::Backspace => {
                                        state.search_query.pop();
                                        handled = true;
                                    }
                                    KeyCode::Char('u')
                                        if key.modifiers.contains(KeyModifiers::CONTROL) =>
                                    {
                                        state.search_query.clear();
                                        handled = true;
                                    }
                                    KeyCode::Char(ch) => {
                                        if !key.modifiers.contains(KeyModifiers::CONTROL)
                                            && !key.modifiers.contains(KeyModifiers::ALT)
                                        {
                                            state.search_query.push(ch);
                                            handled = true;
                                        }
                                    }
                                    _ => {}
                                }
                                if handled {
                                    rebuild_active_filtered(
                                        tab,
                                        &mut theme_state,
                                        &mut waybar_state,
                                        &mut walker_state,
                                        &mut hyprlock_state,
                                        &mut unlock_state,
                                        &mut starship_state,
                                        &mut preset_state,
                                        &theme_items,
                                        &waybar_items,
                                        &walker_items,
                                        &hyprlock_items,
                                        &unlock_items,
                                        &starship_items,
                                        &preset_items,
                                    );
                                    if !event::poll(Duration::from_millis(0))? {
                                        break 'event_loop;
                                    }
                                    continue 'event_loop;
                                }
                            }
                        }
                        if key.code == KeyCode::Char('q') || key.code == KeyCode::Esc {
                            cleanup_terminal(&mut terminal)?;
                            return Ok(None);
                        }
                        if key.code == KeyCode::Tab {
                            clear_active_preview(
                                &backend,
                                tab,
                                &mut theme_state,
                                &mut waybar_state,
                                &mut walker_state,
                                &mut hyprlock_state,
                                &mut unlock_state,
                                &mut starship_state,
                                &mut preset_state,
                            );
                            tab = next_tab(tab);
                            clear_kitty_preview(&backend);
                            mark_force_clear(
                                &mut theme_state,
                                &mut waybar_state,
                                &mut walker_state,
                                &mut hyprlock_state,
                                &mut unlock_state,
                                &mut starship_state,
                                &mut preset_state,
                            );
                            if !event::poll(Duration::from_millis(0))? {
                                break 'event_loop;
                            }
                            continue 'event_loop;
                        }
                        if key.code == KeyCode::BackTab {
                            clear_active_preview(
                                &backend,
                                tab,
                                &mut theme_state,
                                &mut waybar_state,
                                &mut walker_state,
                                &mut hyprlock_state,
                                &mut unlock_state,
                                &mut starship_state,
                                &mut preset_state,
                            );
                            tab = previous_tab(tab);
                            clear_kitty_preview(&backend);
                            mark_force_clear(
                                &mut theme_state,
                                &mut waybar_state,
                                &mut walker_state,
                                &mut hyprlock_state,
                                &mut unlock_state,
                                &mut starship_state,
                                &mut preset_state,
                            );
                            if !event::poll(Duration::from_millis(0))? {
                                break 'event_loop;
                            }
                            continue 'event_loop;
                        }
                        if tab == BrowseTab::Review
                            && key.modifiers.contains(KeyModifiers::CONTROL)
                            && key.code == KeyCode::Char('s')
                        {
                            preset_save_active = true;
                            preset_save_input.clear();
                            if !event::poll(Duration::from_millis(0))? {
                                break 'event_loop;
                            }
                            continue 'event_loop;
                        }
                        if tab == BrowseTab::Review && apply_key_matches(config, key) {
                            let selection_theme = if selected_theme == NO_THEME_CHANGE_VALUE {
                                crate::paths::current_theme_name(&config.current_theme_link)?
                                    .ok_or_else(|| anyhow!("current theme not set"))?
                            } else {
                                selected_theme.clone()
                            };
                            let selection = BrowseSelection {
                                theme: selection_theme,
                                no_theme_change: selected_theme == NO_THEME_CHANGE_VALUE,
                                waybar: current_waybar_selection(&waybar_items, &waybar_state),
                                walker: current_walker_selection(&walker_items, &walker_state),
                                hyprlock: current_hyprlock_selection(
                                    &hyprlock_items,
                                    &hyprlock_state,
                                ),
                                unlock: current_unlock_selection(&unlock_items, &unlock_state),
                                starship: current_starship_selection(
                                    &starship_items,
                                    &starship_state,
                                    &theme_path,
                                ),
                            };
                            cleanup_terminal(&mut terminal)?;
                            return Ok(Some(selection));
                        }
                        if key.code == KeyCode::Enter && tab == BrowseTab::Presets {
                            status_tab = tab;
                            status_at = Instant::now();
                            match apply_preset_to_states(
                                config,
                                &preset_items,
                                &mut preset_state,
                                &theme_items,
                                &mut theme_state,
                                &mut selected_theme,
                                &mut theme_path,
                                &mut waybar_items,
                                &mut waybar_state,
                                &mut walker_items,
                                &mut walker_state,
                                &mut hyprlock_items,
                                &mut hyprlock_state,
                                &mut starship_items,
                                &mut starship_state,
                            ) {
                                Ok(()) => {
                                    status_message = "Preset loaded".to_string();
                                    clear_active_preview(
                                        &backend,
                                        tab,
                                        &mut theme_state,
                                        &mut waybar_state,
                                        &mut walker_state,
                                        &mut hyprlock_state,
                                        &mut unlock_state,
                                        &mut starship_state,
                                        &mut preset_state,
                                    );
                                    tab = BrowseTab::Review;
                                    clear_kitty_preview(&backend);
                                    mark_force_clear(
                                        &mut theme_state,
                                        &mut waybar_state,
                                        &mut walker_state,
                                        &mut hyprlock_state,
                                        &mut unlock_state,
                                        &mut starship_state,
                                        &mut preset_state,
                                    );
                                }
                                Err(err) => {
                                    status_message = err.to_string();
                                }
                            }
                            if !event::poll(Duration::from_millis(0))? {
                                break 'event_loop;
                            }
                            continue 'event_loop;
                        }
                        if key.code == KeyCode::Enter && tab != BrowseTab::Review {
                            status_tab = tab;
                            status_at = Instant::now();
                            status_message = match tab {
                                BrowseTab::Theme => "Theme selected".to_string(),
                                BrowseTab::Waybar => "Waybar selected".to_string(),
                                BrowseTab::Walker => "Walker selected".to_string(),
                                BrowseTab::Hyprlock => "Hyprlock selected".to_string(),
                                BrowseTab::Unlock => "Unlock selected".to_string(),
                                BrowseTab::Starship => "Starship selected".to_string(),
                                BrowseTab::Presets => "Preset selected".to_string(),
                                BrowseTab::Review => String::new(),
                            };
                            clear_active_preview(
                                &backend,
                                tab,
                                &mut theme_state,
                                &mut waybar_state,
                                &mut walker_state,
                                &mut hyprlock_state,
                                &mut unlock_state,
                                &mut starship_state,
                                &mut preset_state,
                            );
                            tab = next_tab(tab);
                            if tab == BrowseTab::Review {
                                clear_kitty_preview(&backend);
                                mark_force_clear(
                                    &mut theme_state,
                                    &mut waybar_state,
                                    &mut walker_state,
                                    &mut hyprlock_state,
                                    &mut unlock_state,
                                    &mut starship_state,
                                    &mut preset_state,
                                );
                            }
                            let items_len = match tab {
                                BrowseTab::Theme => theme_state.filtered_indices.len(),
                                BrowseTab::Waybar => waybar_state.filtered_indices.len(),
                                BrowseTab::Walker => walker_state.filtered_indices.len(),
                                BrowseTab::Hyprlock => hyprlock_state.filtered_indices.len(),
                                BrowseTab::Unlock => unlock_state.filtered_indices.len(),
                                BrowseTab::Starship => starship_state.filtered_indices.len(),
                                BrowseTab::Presets => preset_state.filtered_indices.len(),
                                BrowseTab::Review => 0,
                            };
                            if let Some(state) = active_picker_mut(
                                tab,
                                &mut theme_state,
                                &mut waybar_state,
                                &mut walker_state,
                                &mut hyprlock_state,
                                &mut unlock_state,
                                &mut starship_state,
                                &mut preset_state,
                            ) {
                                if items_len > 0 {
                                    state.list_state.select(Some(0));
                                } else {
                                    state.list_state.select(None);
                                }
                                state.focus = FocusArea::List;
                            }
                            if !event::poll(Duration::from_millis(0))? {
                                break 'event_loop;
                            }
                            continue 'event_loop;
                        }

                        if matches!(key.code, KeyCode::Left | KeyCode::Right) {
                            let collapse = key.code == KeyCode::Left;
                            let handled = match tab {
                                BrowseTab::Theme if theme_state.focus == FocusArea::List => {
                                    toggle_group_collapse(&mut theme_state, &theme_items, collapse);
                                    true
                                }
                                BrowseTab::Waybar if waybar_state.focus == FocusArea::List => {
                                    toggle_group_collapse(&mut waybar_state, &waybar_items, collapse);
                                    true
                                }
                                BrowseTab::Walker if walker_state.focus == FocusArea::List => {
                                    toggle_group_collapse(&mut walker_state, &walker_items, collapse);
                                    true
                                }
                                _ => false,
                            };
                            if handled {
                                if !event::poll(Duration::from_millis(0))? {
                                    break 'event_loop;
                                }
                                continue 'event_loop;
                            }
                        }

                        let items_len = match tab {
                            BrowseTab::Theme => nav_len(&theme_items, &theme_state),
                            BrowseTab::Waybar => nav_len(&waybar_items, &waybar_state),
                            BrowseTab::Walker => nav_len(&walker_items, &walker_state),
                            BrowseTab::Hyprlock => hyprlock_state.filtered_indices.len(),
                            BrowseTab::Unlock => unlock_state.filtered_indices.len(),
                            BrowseTab::Starship => starship_state.filtered_indices.len(),
                            BrowseTab::Presets => preset_state.filtered_indices.len(),
                            BrowseTab::Review => 0,
                        };
                        if let Some(state) = active_picker_mut(
                            tab,
                            &mut theme_state,
                            &mut waybar_state,
                            &mut walker_state,
                            &mut hyprlock_state,
                            &mut unlock_state,
                            &mut starship_state,
                            &mut preset_state,
                        ) {
                            match key.code {
                                KeyCode::Up => match state.focus {
                                    FocusArea::List => {
                                        let new_index =
                                            previous_index(state.list_state.selected(), items_len);
                                        state.list_state.select(Some(new_index));
                                    }
                                    FocusArea::Code => {
                                        state.code_scroll = state.code_scroll.saturating_sub(1);
                                    }
                                },
                                KeyCode::Down => match state.focus {
                                    FocusArea::List => {
                                        let new_index =
                                            next_index(state.list_state.selected(), items_len);
                                        state.list_state.select(Some(new_index));
                                    }
                                    FocusArea::Code => {
                                        state.code_scroll = state.code_scroll.saturating_add(1);
                                    }
                                },
                                KeyCode::PageUp => {
                                    let step = inner_rect(active_code_area).height.max(1);
                                    state.code_scroll = state.code_scroll.saturating_sub(step);
                                }
                                KeyCode::PageDown => {
                                    let step = inner_rect(active_code_area).height.max(1);
                                    state.code_scroll = state.code_scroll.saturating_add(step);
                                }
                                KeyCode::Home => match state.focus {
                                    FocusArea::List => state.list_state.select(Some(0)),
                                    FocusArea::Code => state.code_scroll = 0,
                                },
                                KeyCode::End => match state.focus {
                                    FocusArea::List => {
                                        state.list_state.select(Some(items_len.saturating_sub(1)));
                                    }
                                    FocusArea::Code => {
                                        let code_height =
                                            inner_rect(active_code_area).height as usize;
                                        let max_scroll = state
                                            .last_code
                                            .lines
                                            .len()
                                            .saturating_sub(code_height.max(1));
                                        state.code_scroll = max_scroll as u16;
                                    }
                                },
                                _ => {}
                            }
                        }
                    }
                    Event::Mouse(mouse) => match mouse.kind {
                        MouseEventKind::Down(MouseButton::Left) => {
                            if tab_area.contains(Position {
                                x: mouse.column,
                                y: mouse.row,
                            }) {
                                if let Some(index) = tab_index_from_click(&tab_ranges, mouse.column)
                                {
                                    clear_active_preview(
                                        &backend,
                                        tab,
                                        &mut theme_state,
                                        &mut waybar_state,
                                        &mut walker_state,
                                        &mut hyprlock_state,
                                        &mut unlock_state,
                                        &mut starship_state,
                                        &mut preset_state,
                                    );
                                    tab = tab_from_index(index);
                                    clear_kitty_preview(&backend);
                                    mark_force_clear(
                                        &mut theme_state,
                                        &mut waybar_state,
                                        &mut walker_state,
                                        &mut hyprlock_state,
                                        &mut unlock_state,
                                        &mut starship_state,
                                        &mut preset_state,
                                    );
                                    if !event::poll(Duration::from_millis(0))? {
                                        break 'event_loop;
                                    }
                                    continue 'event_loop;
                                }
                            }

                            let position = Position {
                                x: mouse.column,
                                y: mouse.row,
                            };
                            match tab {
                                BrowseTab::Theme => handle_list_mouse_click(
                                    &mut theme_state,
                                    &theme_items,
                                    position,
                                    active_search_area,
                                    active_list_inner,
                                    active_code_inner,
                                ),
                                BrowseTab::Waybar => handle_list_mouse_click(
                                    &mut waybar_state,
                                    &waybar_items,
                                    position,
                                    active_search_area,
                                    active_list_inner,
                                    active_code_inner,
                                ),
                                BrowseTab::Walker => handle_list_mouse_click(
                                    &mut walker_state,
                                    &walker_items,
                                    position,
                                    active_search_area,
                                    active_list_inner,
                                    active_code_inner,
                                ),
                                BrowseTab::Hyprlock => handle_list_mouse_click(
                                    &mut hyprlock_state,
                                    &hyprlock_items,
                                    position,
                                    active_search_area,
                                    active_list_inner,
                                    active_code_inner,
                                ),
                                BrowseTab::Unlock => handle_list_mouse_click(
                                    &mut unlock_state,
                                    &unlock_items,
                                    position,
                                    active_search_area,
                                    active_list_inner,
                                    active_code_inner,
                                ),
                                BrowseTab::Starship => handle_list_mouse_click(
                                    &mut starship_state,
                                    &starship_items,
                                    position,
                                    active_search_area,
                                    active_list_inner,
                                    active_code_inner,
                                ),
                                BrowseTab::Presets => handle_list_mouse_click(
                                    &mut preset_state,
                                    &preset_items,
                                    position,
                                    active_search_area,
                                    active_list_inner,
                                    active_code_inner,
                                ),
                                BrowseTab::Review => {}
                            }
                        }
                        MouseEventKind::ScrollUp => {
                            let items_len = match tab {
                                BrowseTab::Theme => nav_len(&theme_items, &theme_state),
                                BrowseTab::Waybar => nav_len(&waybar_items, &waybar_state),
                                BrowseTab::Walker => nav_len(&walker_items, &walker_state),
                                BrowseTab::Hyprlock => hyprlock_state.filtered_indices.len(),
                                BrowseTab::Unlock => unlock_state.filtered_indices.len(),
                                BrowseTab::Starship => starship_state.filtered_indices.len(),
                                BrowseTab::Presets => preset_state.filtered_indices.len(),
                                BrowseTab::Review => 0,
                            };
                            if let Some(state) = active_picker_mut(
                                tab,
                                &mut theme_state,
                                &mut waybar_state,
                                &mut walker_state,
                                &mut hyprlock_state,
                                &mut unlock_state,
                                &mut starship_state,
                                &mut preset_state,
                            ) {
                                let position = Position {
                                    x: mouse.column,
                                    y: mouse.row,
                                };
                                if active_list_inner.contains(position) {
                                    state.focus = FocusArea::List;
                                    let new_index =
                                        previous_index(state.list_state.selected(), items_len);
                                    state.list_state.select(Some(new_index));
                                } else if active_code_inner.contains(position) {
                                    state.focus = FocusArea::Code;
                                    state.code_scroll = state.code_scroll.saturating_sub(1);
                                }
                            }
                        }
                        MouseEventKind::ScrollDown => {
                            let items_len = match tab {
                                BrowseTab::Theme => nav_len(&theme_items, &theme_state),
                                BrowseTab::Waybar => nav_len(&waybar_items, &waybar_state),
                                BrowseTab::Walker => nav_len(&walker_items, &walker_state),
                                BrowseTab::Hyprlock => hyprlock_state.filtered_indices.len(),
                                BrowseTab::Unlock => unlock_state.filtered_indices.len(),
                                BrowseTab::Starship => starship_state.filtered_indices.len(),
                                BrowseTab::Presets => preset_state.filtered_indices.len(),
                                BrowseTab::Review => 0,
                            };
                            if let Some(state) = active_picker_mut(
                                tab,
                                &mut theme_state,
                                &mut waybar_state,
                                &mut walker_state,
                                &mut hyprlock_state,
                                &mut unlock_state,
                                &mut starship_state,
                                &mut preset_state,
                            ) {
                                let position = Position {
                                    x: mouse.column,
                                    y: mouse.row,
                                };
                                if active_list_inner.contains(position) {
                                    state.focus = FocusArea::List;
                                    let new_index =
                                        next_index(state.list_state.selected(), items_len);
                                    state.list_state.select(Some(new_index));
                                } else if active_code_inner.contains(position) {
                                    state.focus = FocusArea::Code;
                                    state.code_scroll = state.code_scroll.saturating_add(1);
                                }
                            }
                        }
                        _ => {}
                    },
                    _ => {}
                }
                if !event::poll(Duration::from_millis(0))? {
                    break;
                }
            }
        }

        if let Some(new_theme) = current_theme_value(&theme_items, &theme_state) {
            if new_theme != selected_theme {
                selected_theme = new_theme;
                theme_path = resolve_theme_path_for_selection(config, &selected_theme)?;
                let waybar_key = selected_item_key(&waybar_items, &waybar_state);
                let walker_key = selected_item_key(&walker_items, &walker_state);
                let hyprlock_key = selected_item_key(&hyprlock_items, &hyprlock_state);
                let starship_key = selected_item_key(&starship_items, &starship_state);

                waybar_items = build_waybar_items(config, &theme_path)?;
                walker_items = build_walker_items(config, &theme_path)?;
                hyprlock_items = build_hyprlock_items(config, &theme_path)?;
                starship_items = build_starship_items(config, &theme_path)?;

                reset_picker_cache(&mut waybar_state);
                reset_picker_cache(&mut walker_state);
                reset_picker_cache(&mut hyprlock_state);
                reset_picker_cache(&mut starship_state);

                rebuild_filtered(&mut waybar_state, &waybar_items);
                rebuild_filtered(&mut walker_state, &walker_items);
                rebuild_filtered(&mut hyprlock_state, &hyprlock_items);
                rebuild_filtered(&mut starship_state, &starship_items);
                select_item_by_key(&mut waybar_state, &waybar_items, waybar_key);
                select_item_by_key(&mut walker_state, &walker_items, walker_key);
                select_item_by_key(&mut hyprlock_state, &hyprlock_items, hyprlock_key);
                select_item_by_key(&mut starship_state, &starship_items, starship_key);
                ensure_selected(
                    &mut waybar_state.list_state,
                    waybar_state.filtered_indices.len(),
                );
                ensure_selected(
                    &mut walker_state.list_state,
                    walker_state.filtered_indices.len(),
                );
                ensure_selected(
                    &mut hyprlock_state.list_state,
                    hyprlock_state.filtered_indices.len(),
                );
                ensure_selected(
                    &mut starship_state.list_state,
                    starship_state.filtered_indices.len(),
                );
            }
        }
    }
}

fn resolve_theme_path_for_selection(config: &ResolvedConfig, value: &str) -> Result<PathBuf> {
    if value == NO_THEME_CHANGE_VALUE {
        return crate::paths::current_theme_dir(&config.current_theme_link);
    }
    theme_ops::resolve_theme_path(config, value)
}

fn theme_label_for_display(value: &str) -> String {
    if value == NO_THEME_CHANGE_VALUE {
        NO_THEME_CHANGE_LABEL.to_string()
    } else {
        value.to_string()
    }
}

struct OptionItem {
    label: String,
    value: String,
    preview: Option<PathBuf>,
    group: Option<String>,
}

impl OptionItem {
    fn with_kind(
        label: String,
        value: String,
        kind: &str,
        preview: Option<PathBuf>,
    ) -> LabeledItem {
        Self::with_kind_and_group(label, value, kind, preview, None)
    }

    fn with_kind_and_group(
        label: String,
        value: String,
        kind: &str,
        preview: Option<PathBuf>,
        group: Option<String>,
    ) -> LabeledItem {
        LabeledItem {
            label,
            value,
            kind: kind.to_string(),
            preview,
            group,
        }
    }
}

#[derive(Clone)]
struct LabeledItem {
    label: String,
    value: String,
    kind: String,
    preview: Option<PathBuf>,
    group: Option<String>,
}

struct PresetItem {
    label: String,
    name: String,
}

/// A theme/config entry discovered under a themes directory, optionally
/// nested one level inside an organizational group folder.
struct DiscoveredTheme {
    name: String,
    group: Option<String>,
    path: PathBuf,
}

/// Discover entries directly under `themes_dir`, plus one level of nested
/// entries inside folders that aren't themselves a valid theme (treated as
/// an organizational group). `is_theme_dir` decides what counts as a theme;
/// `skip_name` filters out internal/auto-generated entries at either level.
fn discover_grouped_themes(
    themes_dir: &Path,
    is_theme_dir: impl Fn(&Path) -> bool,
    skip_name: impl Fn(&str) -> bool,
) -> Result<Vec<DiscoveredTheme>> {
    if !themes_dir.is_dir() {
        return Ok(Vec::new());
    }
    let mut out = Vec::new();
    for entry in fs::read_dir(themes_dir)? {
        let entry = entry?;
        let path = entry.path();
        if !path.is_dir() {
            continue;
        }
        let Some(name) = path.file_name().and_then(|n| n.to_str()) else {
            continue;
        };
        if skip_name(name) {
            continue;
        }
        if is_theme_dir(&path) {
            out.push(DiscoveredTheme {
                name: name.to_string(),
                group: None,
                path: path.clone(),
            });
            continue;
        }
        let group_name = name.to_string();
        if let Ok(children) = fs::read_dir(&path) {
            for child in children.flatten() {
                let child_path = child.path();
                if !child_path.is_dir() {
                    continue;
                }
                let Some(child_name) = child_path.file_name().and_then(|n| n.to_str()) else {
                    continue;
                };
                if skip_name(child_name) || !is_theme_dir(&child_path) {
                    continue;
                }
                out.push(DiscoveredTheme {
                    name: child_name.to_string(),
                    group: Some(group_name.clone()),
                    path: child_path.clone(),
                });
            }
        }
    }
    out.sort_by(|a, b| {
        a.group
            .is_some()
            .cmp(&b.group.is_some())
            .then_with(|| a.group.cmp(&b.group))
            .then_with(|| a.name.cmp(&b.name))
    });
    Ok(out)
}

fn pin_omarchy_default_first_entries(entries: &mut Vec<DiscoveredTheme>) {
    if let Some(index) = entries.iter().position(|e| e.name == "omarchy-default") {
        if index != 0 {
            let value = entries.remove(index);
            entries.insert(0, value);
        }
    }
}

fn build_waybar_items(config: &ResolvedConfig, theme_path: &Path) -> Result<Vec<LabeledItem>> {
    waybar::ensure_omarchy_default_theme_link(config, true)?;

    let mut items = Vec::new();
    items.push(OptionItem::with_kind(
        "No Waybar change".to_string(),
        "none".to_string(),
        "none",
        None,
    ));

    let theme_waybar = theme_path.join("waybar-theme");
    if theme_waybar.join("config.jsonc").is_file() && theme_waybar.join("style.css").is_file() {
        let preview_path = preview::find_waybar_preview(&theme_waybar);
        items.push(OptionItem::with_kind(
            "Use theme waybar".to_string(),
            "theme".to_string(),
            "theme",
            preview_path,
        ));
    }

    let mut entries = list_waybar_themes(&config.waybar_themes_dir)?;
    pin_omarchy_default_first_entries(&mut entries);
    for entry in entries {
        let preview_path = preview::find_waybar_preview(&entry.path);
        items.push(OptionItem::with_kind_and_group(
            display_theme_name(&entry.name),
            entry.name,
            "named",
            preview_path,
            entry.group,
        ));
    }

    Ok(items)
}

fn build_starship_items(config: &ResolvedConfig, theme_path: &Path) -> Result<Vec<LabeledItem>> {
    starship::ensure_omarchy_default_theme_link(config, true)?;

    let mut items = Vec::new();
    items.push(OptionItem::with_kind(
        "No Starship change".to_string(),
        "none".to_string(),
        "none",
        None,
    ));

    if theme_path.join("starship.toml").is_file() {
        items.push(OptionItem::with_kind(
            "Use theme starship".to_string(),
            "theme".to_string(),
            "theme",
            None,
        ));
    }

    for preset in list_starship_presets() {
        items.push(OptionItem::with_kind(
            format!("Preset: {preset}"),
            preset,
            "preset",
            None,
        ));
    }

    let mut themes = list_starship_themes(&config.starship_themes_dir)?;
    pin_omarchy_default_first(&mut themes);
    for theme in themes {
        items.push(OptionItem::with_kind(
            format!("Theme: {}", display_theme_name(&theme)),
            theme,
            "named",
            None,
        ));
    }

    Ok(items)
}

fn build_preset_items(file: &presets::PresetFile) -> Vec<PresetItem> {
    let mut names: Vec<String> = file.preset.keys().cloned().collect();
    names.sort();
    names
        .into_iter()
        .map(|name| PresetItem {
            label: name.clone(),
            name,
        })
        .collect()
}

fn build_walker_items(config: &ResolvedConfig, theme_path: &Path) -> Result<Vec<LabeledItem>> {
    walker::ensure_omarchy_default_theme_link(config, true)?;

    let mut items = Vec::new();
    items.push(OptionItem::with_kind(
        "No Walker change".to_string(),
        "none".to_string(),
        "none",
        None,
    ));

    let theme_walker = theme_path.join("walker-theme");
    if theme_walker.join("style.css").is_file() {
        let preview_path = crate::preview::find_walker_preview(&theme_walker);
        items.push(OptionItem::with_kind(
            "Use theme walker".to_string(),
            "theme".to_string(),
            "theme",
            preview_path,
        ));
    }

    let mut entries = list_walker_themes(&config.walker_themes_dir)?;
    pin_omarchy_default_first_entries(&mut entries);
    for entry in entries {
        let preview_path = crate::preview::find_walker_preview(&entry.path);
        items.push(OptionItem::with_kind_and_group(
            display_theme_name(&entry.name),
            entry.name,
            "named",
            preview_path,
            entry.group,
        ));
    }

    Ok(items)
}

fn build_hyprlock_items(config: &ResolvedConfig, theme_path: &Path) -> Result<Vec<LabeledItem>> {
    hyprlock::ensure_omarchy_default_theme_link(config, true)?;

    let mut items = Vec::new();
    items.push(OptionItem::with_kind(
        "No Hyprlock change".to_string(),
        "none".to_string(),
        "none",
        None,
    ));

    let theme_hyprlock = theme_path.join("hyprlock-theme");
    if theme_hyprlock.join("hyprlock.conf").is_file() {
        let preview_path = preview::find_theme_preview(&theme_hyprlock);
        items.push(OptionItem::with_kind(
            "Use theme hyprlock".to_string(),
            "theme".to_string(),
            "theme",
            preview_path,
        ));
    }

    let mut names = list_hyprlock_themes(&config.hyprlock_themes_dir)?;
    if hyprlock::omarchy_default_theme_available(config)
        && !names.iter().any(|name| name == "omarchy-default")
    {
        names.push("omarchy-default".to_string());
    }
    pin_omarchy_default_first(&mut names);

    for name in names {
        let preview_path = preview::find_theme_preview(&config.hyprlock_themes_dir.join(&name));
        items.push(OptionItem::with_kind(
            display_theme_name(&name),
            name,
            "named",
            preview_path,
        ));
    }

    Ok(items)
}

fn build_unlock_items(config: &ResolvedConfig) -> Result<Vec<LabeledItem>> {
    let mut items = Vec::new();
    items.push(OptionItem::with_kind(
        "No Unlock change".to_string(),
        "none".to_string(),
        "none",
        None,
    ));

    items.push(OptionItem::with_kind(
        "Default".to_string(),
        "default".to_string(),
        "default",
        omarchy_default_unlock_preview(config),
    ));

    for name in unlock::list_unlock_theme_entries(config)? {
        let preview_path = unlock_theme_preview(config, &name);
        items.push(OptionItem::with_kind(
            display_theme_name(&name),
            name,
            "named",
            preview_path,
        ));
    }

    Ok(items)
}

fn build_walker_code_preview(
    config: &ResolvedConfig,
    theme_path: &Path,
    item: &LabeledItem,
) -> Text<'static> {
    match item.kind.as_str() {
        "none" => Text::from("No Walker change."),
        "theme" => {
            let base = theme_path.join("walker-theme");
            let mut parts = vec![("style.css", base.join("style.css"), "css")];
            let layout = base.join("layout.xml");
            if layout.is_file() {
                parts.insert(0, ("layout.xml", layout, "xml"));
            }
            load_multi_code_preview(&parts)
        }
        _ => {
            let base = config.walker_themes_dir.join(&item.value);
            let mut parts = vec![("style.css", base.join("style.css"), "css")];
            let layout = base.join("layout.xml");
            if layout.is_file() {
                parts.insert(0, ("layout.xml", layout, "xml"));
            }
            load_multi_code_preview(&parts)
        }
    }
}

fn build_unlock_code_preview(config: &ResolvedConfig, item: &LabeledItem) -> Text<'static> {
    match item.kind.as_str() {
        "none" => Text::from("No Unlock change."),
        "default" => Text::from(
            "Restore the default Omarchy Plymouth and SDDM unlock theme.\n\nThis may prompt for sudo.",
        ),
        _ => {
            let Some(theme_dir) = unlock_theme_dir(config, &item.value) else {
                return Text::from("Unlock theme not found.");
            };
            let parts = vec![("colors.toml", theme_dir.join("colors.toml"), "toml")];
            load_multi_code_preview(&parts)
        }
    }
}

fn build_hyprlock_code_preview(
    config: &ResolvedConfig,
    theme_path: &Path,
    item: &LabeledItem,
) -> Text<'static> {
    match item.kind.as_str() {
        "none" => Text::from("No Hyprlock change."),
        "theme" => load_code_preview(
            "hyprlock.conf",
            theme_path.join("hyprlock-theme/hyprlock.conf"),
            "conf",
        ),
        _ => load_code_preview(
            "hyprlock.conf",
            config
                .hyprlock_themes_dir
                .join(&item.value)
                .join("hyprlock.conf"),
            "conf",
        ),
    }
}

fn build_waybar_code_preview(
    config: &ResolvedConfig,
    theme_path: &Path,
    item: &LabeledItem,
) -> Text<'static> {
    match item.kind.as_str() {
        "none" => Text::from("No Waybar change."),
        "theme" => {
            let base = theme_path.join("waybar-theme");
            let parts = vec![
                ("config.jsonc", base.join("config.jsonc"), "json"),
                ("style.css", base.join("style.css"), "css"),
            ];
            load_multi_code_preview(&parts)
        }
        _ => {
            let base = config.waybar_themes_dir.join(&item.value);
            let parts = vec![
                ("config.jsonc", base.join("config.jsonc"), "json"),
                ("style.css", base.join("style.css"), "css"),
            ];
            load_multi_code_preview(&parts)
        }
    }
}

fn build_starship_code_preview(
    config: &ResolvedConfig,
    theme_path: &Path,
    item: &LabeledItem,
) -> Text<'static> {
    match item.kind.as_str() {
        "none" => Text::from("No Starship change."),
        "theme" => load_code_preview("starship.toml", theme_path.join("starship.toml"), "yaml"),
        "preset" => {
            let preset = item.value.as_str();
            let output = Command::new("starship").args(["preset", preset]).output();
            let output = match output {
                Ok(output) if output.status.success() => output.stdout,
                _ => return Text::from(format!("Failed to load preset: {preset}")),
            };
            load_code_preview_from_string("preset.toml", &String::from_utf8_lossy(&output), "toml")
        }
        _ => load_code_preview(
            &format!("{}.toml", item.value),
            config
                .starship_themes_dir
                .join(format!("{}.toml", item.value)),
            "toml",
        ),
    }
}

fn build_starship_prompt_preview(
    config: &ResolvedConfig,
    theme_path: &Path,
    item: &LabeledItem,
) -> Text<'static> {
    render_starship_prompt_preview(config, theme_path, item)
}

fn load_multi_code_preview(parts: &[(&str, PathBuf, &str)]) -> Text<'static> {
    let mut combined = Text::from("");
    let mut first = true;
    for (title, path, syntax) in parts {
        if !first {
            combined.lines.push(Line::from(""));
        }
        first = false;
        let mut header = Text::from(vec![
            Line::from(format!("=== {} ===", title)),
            Line::from(""),
        ]);
        combined.lines.append(&mut header.lines);
        let block = load_code_preview(title, path.clone(), syntax);
        combined.lines.extend(block.lines);
    }
    combined
}

fn load_code_preview(title: &str, path: PathBuf, syntax: &str) -> Text<'static> {
    if !path.is_file() {
        return Text::from(format!("Missing {} at {}", title, path.to_string_lossy()));
    }
    match fs::read_to_string(&path) {
        Ok(content) => load_code_preview_from_string(title, &content, syntax),
        Err(_) => Text::from(format!("Failed to read {}", title)),
    }
}

fn load_code_preview_from_string(title: &str, content: &str, syntax: &str) -> Text<'static> {
    let mut lines = Vec::new();
    lines.push(Line::from(format!("=== {} ===", title)));
    lines.push(Line::from(""));
    let highlighted = highlight_code(content, syntax);
    lines.extend(highlighted.lines);
    Text::from(lines)
}

fn highlight_code(content: &str, syntax: &str) -> Text<'static> {
    let ps = SyntaxSet::load_defaults_newlines();
    let ts = ThemeSet::load_defaults();
    let theme = ts
        .themes
        .get("base16-ocean.dark")
        .or_else(|| ts.themes.values().next())
        .expect("theme");
    let syntax_ref = ps
        .find_syntax_by_extension(syntax)
        .unwrap_or_else(|| ps.find_syntax_plain_text());
    let mut h = HighlightLines::new(syntax_ref, theme);
    let mut out = String::new();
    for line in content.lines() {
        let ranges = h.highlight_line(line, &ps).unwrap_or_default();
        out.push_str(&as_24_bit_terminal_escaped(&ranges[..], false));
        out.push('\n');
    }
    match out.as_bytes().into_text() {
        Ok(text) => convert_text(text),
        Err(_) => Text::from(content.to_string()),
    }
}

fn render_starship_prompt_preview(
    config: &ResolvedConfig,
    theme_path: &Path,
    item: &LabeledItem,
) -> Text<'static> {
    if item.kind.as_str() == "none" {
        return Text::from("No Starship change.\n\nThe current prompt config remains as-is.");
    }

    if !command_exists("starship") {
        return Text::from("Starship not found in PATH.");
    }

    let temp_dir = match TempDir::new() {
        Ok(dir) => dir,
        Err(_) => return Text::from("Failed to create preview temp dir."),
    };
    let preview_root = temp_dir.path();
    let _ = Command::new("git")
        .arg("init")
        .arg("-q")
        .current_dir(preview_root)
        .status();
    let _ = fs::write(preview_root.join("README.md"), "mock");
    let _ = Command::new("git")
        .arg("add")
        .arg(".")
        .current_dir(preview_root)
        .status();

    let config_path = match item.kind.as_str() {
        "theme" => {
            let path = theme_path.join("starship.toml");
            if !path.is_file() {
                return Text::from("Theme-specific Starship config not found.");
            }
            path
        }
        "preset" => {
            let preset_name = item.value.as_str();
            let output = Command::new("starship")
                .args(["preset", preset_name])
                .output();
            let output = match output {
                Ok(output) if output.status.success() => output,
                _ => return Text::from(format!("Failed to load preset: {preset_name}")),
            };
            let preset_path = preview_root.join("preset.toml");
            if fs::write(&preset_path, output.stdout).is_err() {
                return Text::from("Failed to write preset file.");
            }
            preset_path
        }
        "named" => {
            let path = config
                .starship_themes_dir
                .join(format!("{}.toml", item.value));
            if !path.is_file() {
                return Text::from(format!(
                    "Theme config not found: {}",
                    path.to_string_lossy()
                ));
            }
            path
        }
        _ => {
            return Text::from("Unknown selection.");
        }
    };

    let width = 100u16;
    let width_str = width.to_string();
    let prompt_output = Command::new("starship")
        .args([
            "prompt",
            "--path",
            preview_root.to_string_lossy().as_ref(),
            "--terminal-width",
            &width_str,
            "--jobs",
            "0",
        ])
        .env("STARSHIP_CONFIG", &config_path)
        .output();

    let prompt = match prompt_output {
        Ok(output) if output.status.success() => {
            String::from_utf8_lossy(&output.stdout).to_string()
        }
        _ => "Failed to render prompt.".to_string(),
    };

    let right_output = Command::new("starship")
        .args([
            "prompt",
            "--right",
            "--path",
            preview_root.to_string_lossy().as_ref(),
            "--terminal-width",
            &width_str,
        ])
        .env("STARSHIP_CONFIG", &config_path)
        .output();

    let right_prompt = match right_output {
        Ok(output) if output.status.success() => {
            String::from_utf8_lossy(&output.stdout).to_string()
        }
        _ => String::new(),
    };

    let mut lines: Vec<Line<'static>> = Vec::new();
    lines.push(Line::from("=== Starship Prompt Preview ==="));
    lines.push(Line::from(""));

    let left_lines = trim_empty_lines(parse_ansi_lines(&strip_prompt_markers(&prompt)));
    let right_trimmed = strip_prompt_markers(right_prompt.trim());
    if !right_trimmed.is_empty() {
        let right_lines = trim_empty_lines(parse_ansi_lines(&right_trimmed));
        lines.extend(combine_prompt_lines(&left_lines, &right_lines, width));
    } else {
        lines.extend(left_lines);
    }

    Text::from(lines)
}

fn strip_prompt_markers(input: &str) -> String {
    input.replace("\\[", "").replace("\\]", "")
}

fn parse_ansi_lines(input: &str) -> Vec<Line<'static>> {
    match input.as_bytes().into_text() {
        Ok(text) => convert_text(text).lines,
        Err(_) => input
            .lines()
            .map(|line| Line::from(line.to_string()))
            .collect(),
    }
}

fn combine_prompt_lines(
    left_lines: &[Line<'static>],
    right_lines: &[Line<'static>],
    width: u16,
) -> Vec<Line<'static>> {
    if left_lines.is_empty() {
        return right_lines.to_vec();
    }
    if right_lines.is_empty() {
        return left_lines.to_vec();
    }

    let mut out = Vec::new();
    let total_width = width as usize;

    if left_lines.len() > 1 {
        out.extend_from_slice(&left_lines[..left_lines.len() - 1]);
    }

    let left_last = left_lines[left_lines.len() - 1].clone();
    let right_first = right_lines[0].clone();
    let left_width = left_last.width();
    let right_width = right_first.width();
    let spacer_width = total_width.saturating_sub(left_width + right_width);

    let mut spans = left_last.spans;
    if spacer_width > 0 {
        spans.push(ratatui::text::Span::raw(" ".repeat(spacer_width)));
    }
    spans.extend(right_first.spans);
    out.push(Line::from(spans));

    if right_lines.len() > 1 {
        out.extend_from_slice(&right_lines[1..]);
    }
    out
}

fn trim_empty_lines(mut lines: Vec<Line<'static>>) -> Vec<Line<'static>> {
    while lines.first().map(|l| l.width() == 0).unwrap_or(false) {
        lines.remove(0);
    }
    while lines.last().map(|l| l.width() == 0).unwrap_or(false) {
        lines.pop();
    }
    lines
}

struct PickerAreas {
    search_area: Rect,
    list_inner: Rect,
    code_inner: Rect,
    code_area: Rect,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum PreviewAction {
    None,
    Clear,
    Render,
    ClearAndRender,
}

fn decide_preview_action(
    wants_image: bool,
    image_visible: bool,
    invalidate: bool,
) -> PreviewAction {
    if wants_image {
        if invalidate || !image_visible {
            if image_visible {
                PreviewAction::ClearAndRender
            } else {
                PreviewAction::Render
            }
        } else {
            PreviewAction::None
        }
    } else if image_visible || invalidate {
        PreviewAction::Clear
    } else {
        PreviewAction::None
    }
}

fn text_is_blank(text: &Text<'_>) -> bool {
    text.lines.is_empty() || text.lines.iter().all(|line| line.width() == 0)
}

fn preview_debug_enabled() -> bool {
    std::env::var("THEME_MANAGER_DEBUG_PREVIEW").is_ok()
}

enum DisplayRow {
    Header { name: String, count: usize },
    Item(usize),
}

/// Only group tabs that actually have at least one grouped item, and only
/// while there's no active search (search already reorders by relevance, so
/// group headers would be meaningless; search also bypasses collapsed state
/// so results are never hidden).
fn grouping_active<T: ItemView>(items: &[T], search_query: &str) -> bool {
    search_query.trim().is_empty() && items.iter().any(|item| item.group().is_some())
}

/// The length `list_state.selected()` should be bounded by for Up/Down/
/// Home/End navigation: the flattened header+item row count when grouping
/// is active, otherwise the plain `filtered_indices` count.
fn nav_len<T: ItemView>(items: &[T], state: &PickerState) -> usize {
    if grouping_active(items, &state.search_query) {
        build_display_rows(items, &state.filtered_indices, &state.collapsed_groups).len()
    } else {
        state.filtered_indices.len()
    }
}

/// Every distinct group name present in `items`.
fn all_group_names<T: ItemView>(items: &[T]) -> std::collections::HashSet<String> {
    items
        .iter()
        .filter_map(|item| item.group().map(|g| g.to_string()))
        .collect()
}

/// Starting collapsed-groups set: every group collapsed, except the one
/// containing `active_value` (if any) so whatever is already selected stays
/// visible without the user having to go find and expand it first.
fn initial_collapsed_groups<T: ItemView>(
    items: &[T],
    active_value: Option<&str>,
) -> std::collections::HashSet<String> {
    let mut collapsed = all_group_names(items);
    if let Some(value) = active_value {
        if let Some(group) = items
            .iter()
            .find(|item| item.value() == value)
            .and_then(|item| item.group())
        {
            collapsed.remove(group);
        }
    }
    collapsed
}

/// Flattens `items` (already sorted so same-group entries are contiguous)
/// into header + item rows, skipping item rows for collapsed groups while
/// still showing their header (with a disclosure glyph and count) so they
/// can be re-expanded.
fn build_display_rows<T: ItemView>(
    items: &[T],
    filtered_indices: &[usize],
    collapsed: &std::collections::HashSet<String>,
) -> Vec<DisplayRow> {
    let visible: std::collections::HashSet<usize> = filtered_indices.iter().copied().collect();
    let mut rows = Vec::new();
    let mut current_group: Option<String> = None;
    let mut started = false;
    let mut i = 0;
    while i < items.len() {
        let group = items[i].group().map(|g| g.to_string());
        if !started || current_group != group {
            if let Some(name) = &group {
                let mut count = 0;
                let mut j = i;
                while j < items.len() && items[j].group() == Some(name.as_str()) {
                    count += 1;
                    j += 1;
                }
                rows.push(DisplayRow::Header {
                    name: name.clone(),
                    count,
                });
            }
            current_group = group.clone();
            started = true;
        }
        let hidden = group.as_ref().is_some_and(|g| collapsed.contains(g));
        if visible.contains(&i) && !hidden {
            rows.push(DisplayRow::Item(i));
        }
        i += 1;
    }
    rows
}

fn render_picker<T: ItemView>(
    frame: &mut Frame,
    area: Rect,
    title: &str,
    preview_title: &str,
    items: &[T],
    state: &mut PickerState,
    backend: &PreviewBackend,
    code_preview: impl Fn(usize) -> Text<'static>,
    image_preview: impl Fn(usize) -> Option<PathBuf>,
    preview_text: impl Fn(usize) -> Option<Text<'static>>,
    tall_image_preview: bool,
    status: Option<&str>,
) -> PickerAreas {
    let chunks = Layout::default()
        .direction(Direction::Vertical)
        .constraints(if tall_image_preview {
            [Constraint::Percentage(30), Constraint::Percentage(70)].as_ref()
        } else {
            [Constraint::Percentage(65), Constraint::Percentage(35)].as_ref()
        })
        .split(area);
    let top_chunks = Layout::default()
        .direction(Direction::Horizontal)
        .constraints(
            [
                Constraint::Percentage(25),
                Constraint::Length(1),
                Constraint::Min(0),
            ]
            .as_ref(),
        )
        .split(chunks[0]);
    let image_area = inner_rect(chunks[1]);
    let list_column = top_chunks[0];
    let list_chunks = Layout::default()
        .direction(Direction::Vertical)
        .constraints([Constraint::Length(3), Constraint::Min(0)].as_ref())
        .split(list_column);
    let search_area = list_chunks[0];
    let list_area = list_chunks[1];
    let list_inner = list_inner_rect(list_area);
    let code_area = top_chunks[2];
    let code_inner = inner_rect(code_area);

    render_search_input(
        frame,
        search_area,
        &state.search_query,
        state.focus == FocusArea::List,
    );

    let list_title = build_list_title(title, status);
    let list_block = Block::default()
        .title(list_title)
        .borders(Borders::ALL)
        .border_style(if state.focus == FocusArea::List {
            Style::default().fg(if status.is_some() {
                Color::Green
            } else {
                Color::Yellow
            })
        } else {
            Style::default()
        });
    // When grouping is active, `state.list_state` indexes into the flattened
    // header+item row list (built below) rather than directly into
    // `filtered_indices` — this is what lets Up/Down land on a header row so
    // it can be expanded/collapsed. `selected_item_index` is the single
    // place that knows how to go from "whatever list_state.selected() means
    // right now" to a real item index, in either mode.
    if grouping_active(items, &state.search_query) {
        let rows = build_display_rows(items, &state.filtered_indices, &state.collapsed_groups);
        let list_items: Vec<ListItem> = rows
            .iter()
            .map(|row| match row {
                DisplayRow::Header { name, count } => {
                    let glyph = if state.collapsed_groups.contains(name) {
                        "\u{25b8}"
                    } else {
                        "\u{25be}"
                    };
                    ListItem::new(Line::from(Span::styled(
                        format!("{glyph} {name} ({count})"),
                        Style::default()
                            .fg(Color::DarkGray)
                            .add_modifier(Modifier::BOLD),
                    )))
                }
                DisplayRow::Item(idx) => ListItem::new(Line::from(items[*idx].label())),
            })
            .collect();
        let list = List::new(list_items)
            .block(list_block)
            .highlight_style(
                Style::default()
                    .fg(Color::Yellow)
                    .add_modifier(Modifier::BOLD),
            )
            .highlight_symbol(">> ");
        frame.render_stateful_widget(list, list_area, &mut state.list_state);
    } else {
        let list_items: Vec<ListItem> = state
            .filtered_indices
            .iter()
            .map(|&idx| ListItem::new(Line::from(items[idx].label())))
            .collect();
        let list = List::new(list_items)
            .block(list_block)
            .highlight_style(
                Style::default()
                    .fg(Color::Yellow)
                    .add_modifier(Modifier::BOLD),
            )
            .highlight_symbol(">> ");
        frame.render_stateful_widget(list, list_area, &mut state.list_state);
    }

    let selected_item = selected_item_index(state, items);
    let preview_path = selected_item.and_then(|idx| image_preview(idx));
    let previous_preview_index = state.last_preview_index;
    let previous_preview_path = state.last_preview.clone();

    if let Some(item_index) = selected_item {
        state.last_selected = Some(item_index);
        if Some(item_index) != state.last_code_index {
            state.last_code = code_preview(item_index);
            state.code_scroll = 0;
            state.last_code_index = Some(item_index);
        }
    } else {
        state.last_code = Text::from("No matches.");
        state.last_code_index = None;
    }

    let max_scroll = state
        .last_code
        .lines
        .len()
        .saturating_sub(code_inner.height.max(1) as usize);
    if state.code_scroll as usize > max_scroll {
        state.code_scroll = max_scroll as u16;
    }
    let code_block = Block::default()
        .title("Code Preview")
        .borders(Borders::ALL)
        .border_style(if state.focus == FocusArea::Code {
            Style::default().fg(Color::Yellow)
        } else {
            Style::default()
        });
    let code = Paragraph::new(state.last_code.clone())
        .block(code_block)
        .scroll((state.code_scroll, 0))
        .wrap(Wrap { trim: false });
    frame.render_widget(code, code_area);

    if let Some(item_index) = selected_item {
        if let Some(text) = preview_text(item_index) {
            state.last_preview_text = text;
            state.last_preview = None;
        } else {
            state.last_preview_text = Text::default();
            state.last_preview = preview_path.clone();
        }
        state.last_preview_index = Some(item_index);
    } else {
        state.last_preview_text = Text::from("No matches.");
        state.last_preview = None;
        state.last_preview_index = None;
    }

    let selection_changed = previous_preview_index != state.last_preview_index;
    let path_changed = previous_preview_path != state.last_preview;
    let rect_changed = state.last_image_area != Some(image_area);
    state.last_image_area = Some(image_area);
    let invalidate = state.force_clear
        || state.preview_dirty
        || selection_changed
        || path_changed
        || rect_changed;
    let wants_image = text_is_blank(&state.last_preview_text) && state.last_preview.is_some();
    let action = decide_preview_action(wants_image, state.image_visible, invalidate);
    if preview_debug_enabled() {
        eprintln!(
      "chromacon-style-manager: preview {:?} tab={} sel={:?} path={:?} rect={}x{}@{}x{} invalidate={} visible={} wants_image={}",
      action,
      title,
      state.last_preview_index,
      state.last_preview.as_ref().map(|p| p.to_string_lossy().to_string()),
      image_area.width,
      image_area.height,
      image_area.x,
      image_area.y,
      invalidate,
      state.image_visible,
      wants_image
    );
    }
    if matches!(action, PreviewAction::Clear | PreviewAction::ClearAndRender) {
        match action {
            PreviewAction::Clear | PreviewAction::ClearAndRender => {
                backend.render(None, image_area);
            }
            _ => {}
        }
    }
    state.force_clear = false;
    state.preview_dirty = false;

    let preview_text_rendered = if text_is_blank(&state.last_preview_text) {
        backend.text_preview(state.last_preview.as_deref(), image_area)
    } else {
        state.last_preview_text.clone()
    };
    let preview = Paragraph::new(preview_text_rendered)
        .block(Block::default().title(preview_title).borders(Borders::ALL));
    frame.render_widget(preview, chunks[1]);

    match action {
        PreviewAction::None => {}
        PreviewAction::Clear => {
            state.image_visible = false;
        }
        PreviewAction::Render | PreviewAction::ClearAndRender => {
            backend.render(state.last_preview.as_deref(), image_area);
            state.image_visible = wants_image;
        }
    }

    PickerAreas {
        search_area,
        list_inner,
        code_inner,
        code_area,
    }
}

fn render_preset_picker(
    frame: &mut Frame,
    area: Rect,
    items: &[PresetItem],
    state: &mut PickerState,
    summary: impl Fn(usize) -> Text<'static>,
    status: Option<&str>,
) -> PickerAreas {
    let chunks = Layout::default()
        .direction(Direction::Horizontal)
        .constraints(
            [
                Constraint::Percentage(30),
                Constraint::Length(1),
                Constraint::Min(0),
            ]
            .as_ref(),
        )
        .split(area);
    let list_column = chunks[0];
    let list_chunks = Layout::default()
        .direction(Direction::Vertical)
        .constraints([Constraint::Length(3), Constraint::Min(0)].as_ref())
        .split(list_column);
    let search_area = list_chunks[0];
    let list_area = list_chunks[1];
    let list_inner = list_inner_rect(list_area);
    let summary_area = chunks[2];
    let summary_inner = inner_rect(summary_area);

    render_search_input(
        frame,
        search_area,
        &state.search_query,
        state.focus == FocusArea::List,
    );

    let list_items: Vec<ListItem> = state
        .filtered_indices
        .iter()
        .map(|&idx| ListItem::new(Line::from(items[idx].label())))
        .collect();
    let list_title = build_list_title("Select preset", status);
    let list_block = Block::default()
        .title(list_title)
        .borders(Borders::ALL)
        .border_style(if state.focus == FocusArea::List {
            Style::default().fg(if status.is_some() {
                Color::Green
            } else {
                Color::Yellow
            })
        } else {
            Style::default()
        });
    let list = List::new(list_items)
        .block(list_block)
        .highlight_style(
            Style::default()
                .fg(Color::Yellow)
                .add_modifier(Modifier::BOLD),
        )
        .highlight_symbol(">> ");
    frame.render_stateful_widget(list, list_area, &mut state.list_state);

    let selected = selected_index(&state.list_state, state.filtered_indices.len());
    let selected_item = state.filtered_indices.get(selected).copied();
    if let Some(item_index) = selected_item {
        state.last_selected = Some(item_index);
        if Some(item_index) != state.last_code_index {
            state.last_code = summary(item_index);
            state.code_scroll = 0;
            state.last_code_index = Some(item_index);
        }
    } else {
        state.last_code = Text::from("No presets found.");
        state.last_code_index = None;
    }

    let max_scroll = state
        .last_code
        .lines
        .len()
        .saturating_sub(summary_inner.height.max(1) as usize);
    if state.code_scroll as usize > max_scroll {
        state.code_scroll = max_scroll as u16;
    }
    let summary_block = Block::default()
        .title("Preset Summary")
        .borders(Borders::ALL)
        .border_style(if state.focus == FocusArea::Code {
            Style::default().fg(Color::Yellow)
        } else {
            Style::default()
        });
    let summary_panel = Paragraph::new(state.last_code.clone())
        .block(summary_block)
        .scroll((state.code_scroll, 0))
        .wrap(Wrap { trim: false });
    frame.render_widget(summary_panel, summary_area);

    PickerAreas {
        search_area,
        list_inner,
        code_inner: summary_inner,
        code_area: summary_area,
    }
}

fn render_review(
    frame: &mut Frame,
    area: Rect,
    selected_theme: &str,
    waybar_label: String,
    walker_label: String,
    hyprlock_label: String,
    unlock_label: String,
    starship_label: String,
) {
    let lines = vec![
        Line::from("=== Review Selections ==="),
        Line::from(""),
        Line::from(format!("Theme: {}", title_case_theme(selected_theme))),
        Line::from(format!("Waybar: {}", waybar_label)),
        Line::from(format!("Walker: {}", walker_label)),
        Line::from(format!("Hyprlock: {}", hyprlock_label)),
        Line::from(format!("Unlock: {}", unlock_label)),
        Line::from(format!("Starship: {}", starship_label)),
        Line::from(""),
        Line::from("Apply: Ctrl+Enter"),
        Line::from("Cancel: Esc"),
        Line::from("Switch tabs: Tab / Shift+Tab (or click tab bar)"),
    ];
    let review = Paragraph::new(Text::from(lines))
        .block(Block::default().title("Review").borders(Borders::ALL))
        .wrap(Wrap { trim: false });
    frame.render_widget(review, area);
}

fn render_status_bar(
    frame: &mut Frame,
    area: Rect,
    tab: BrowseTab,
    theme: &str,
    waybar: String,
    walker: String,
    hyprlock: String,
    unlock: String,
    starship: String,
    status: Option<&str>,
    save_active: bool,
    save_input: &str,
    grouped_tab: bool,
) {
    let mut spans = Vec::new();
    let mut segments: Vec<(String, Color, Color)> = Vec::new();
    let tab_label = match tab {
        BrowseTab::Theme => "Theme",
        BrowseTab::Waybar => "Waybar",
        BrowseTab::Walker => "Walker",
        BrowseTab::Hyprlock => "Hyprlock",
        BrowseTab::Unlock => "Unlock",
        BrowseTab::Starship => "Starship",
        BrowseTab::Presets => "Presets",
        BrowseTab::Review => "Review",
    };

    segments.push((tab_label.to_string(), Color::Black, Color::Yellow));
    segments.push((
        format!("Theme: {}", title_case_theme(theme)),
        Color::Black,
        Color::Cyan,
    ));
    segments.push((format!("Waybar: {waybar}"), Color::Black, Color::Green));
    segments.push((format!("Walker: {walker}"), Color::Black, Color::Blue));
    segments.push((
        format!("Hyprlock: {hyprlock}"),
        Color::Black,
        Color::LightGreen,
    ));
    segments.push((format!("Unlock: {unlock}"), Color::Black, Color::Gray));
    segments.push((
        format!("Starship: {starship}"),
        Color::Black,
        Color::Magenta,
    ));

    if grouped_tab && !save_active {
        segments.push((
            "\u{2190}/\u{2192} Collapse/Expand Group".to_string(),
            Color::Black,
            Color::LightMagenta,
        ));
    }

    if tab == BrowseTab::Review && !save_active {
        segments.push((
            "Ctrl+Enter Apply".to_string(),
            Color::Black,
            Color::LightYellow,
        ));
        segments.push((
            "Ctrl+S Save Preset".to_string(),
            Color::Black,
            Color::LightYellow,
        ));
    }

    if save_active {
        let cursor = "_";
        segments.push((
            format!("Save preset: {save_input}{cursor}"),
            Color::Black,
            Color::Blue,
        ));
    }

    if let Some(message) = status {
        segments.push((message.to_string(), Color::Black, Color::LightBlue));
    }

    for (idx, (label, fg, bg)) in segments.iter().enumerate() {
        push_status_segment(&mut spans, label, *fg, *bg);
        if idx + 1 < segments.len() {
            let next_bg = segments[idx + 1].2;
            spans.push(Span::styled("", Style::default().fg(*bg).bg(next_bg)));
        }
    }
    if let Some((_, _, last_bg)) = segments.last() {
        spans.push(Span::styled(
            "",
            Style::default().fg(*last_bg).bg(Color::Reset),
        ));
    }

    let line = Line::from(spans);
    let bar = Paragraph::new(line);
    frame.render_widget(bar, area);
}

fn push_status_segment(spans: &mut Vec<Span<'static>>, label: &str, fg: Color, bg: Color) {
    spans.push(Span::styled(
        format!(" {} ", label),
        Style::default().fg(fg).bg(bg).add_modifier(Modifier::BOLD),
    ));
}

fn preset_summary_text(
    config: &ResolvedConfig,
    file: &presets::PresetFile,
    item: &PresetItem,
) -> Text<'static> {
    let entry = match file.preset.get(&item.name) {
        Some(entry) => entry,
        None => return Text::from("Preset not found."),
    };
    let summary = presets::summarize_preset(config, &item.name, entry);
    let mut lines = vec![
        Line::from(format!("Preset: {}", item.name)),
        Line::from(""),
        Line::from(format!("Theme: {}", summary.theme)),
        Line::from(format!("Waybar: {}", summary.waybar)),
        Line::from(format!("Walker: {}", summary.walker)),
        Line::from(format!("Hyprlock: {}", summary.hyprlock)),
        Line::from(format!("Starship: {}", summary.starship)),
    ];
    if !summary.errors.is_empty() {
        lines.push(Line::from(""));
        lines.push(Line::from("Issues:"));
        for err in summary.errors {
            lines.push(Line::from(format!("- {}", err)));
        }
    }
    Text::from(lines)
}

fn render_tab_bar(
    frame: &mut Frame,
    area: Rect,
    titles: &[&str],
    active: BrowseTab,
    ranges: &mut Vec<(u16, u16, usize)>,
) {
    ranges.clear();
    let block = Block::default().borders(Borders::BOTTOM);
    let inner = block.inner(area);
    frame.render_widget(block, area);

    let mut spans = Vec::new();
    let mut cursor = inner.x;
    let active_index = tab_index(active);

    for (idx, title) in titles.iter().enumerate() {
        let label = format!(" {} ", title);
        let width = label.len() as u16;
        let style = if idx == active_index {
            Style::default()
                .fg(Color::Black)
                .bg(Color::Yellow)
                .add_modifier(Modifier::BOLD)
        } else {
            Style::default()
        };
        spans.push(Span::styled(label, style));
        ranges.push((cursor, cursor + width.saturating_sub(1), idx));
        cursor = cursor.saturating_add(width);
        if idx + 1 < titles.len() {
            spans.push(Span::raw("│"));
            cursor = cursor.saturating_add(1);
        }
    }

    let title_label = format!(" {} ", APP_TITLE);
    let title_width = title_label.len() as u16;
    let sep_width = 1u16;
    let used_width = cursor.saturating_sub(inner.x);
    let total_needed = used_width
        .saturating_add(sep_width)
        .saturating_add(title_width);
    let spacer_len = if inner.width > total_needed {
        (inner.width - total_needed) as usize
    } else {
        1
    };
    spans.push(Span::raw(" ".repeat(spacer_len)));
    spans.push(Span::raw("│"));
    spans.push(Span::styled(
        title_label,
        Style::default()
            .fg(Color::Cyan)
            .add_modifier(Modifier::BOLD),
    ));

    let line = Line::from(spans);
    let tabs = Paragraph::new(line).alignment(ratatui::layout::Alignment::Left);
    frame.render_widget(tabs, inner);
}

fn build_list_title(title: &str, status: Option<&str>) -> String {
    let out = match status {
        Some(status) => format!("{title}  [{status}]"),
        None => title.to_string(),
    };
    out
}

fn render_search_input(frame: &mut Frame, area: Rect, query: &str, focused: bool) {
    let (content, style) = if query.is_empty() {
        (
            "󰍉 Search...".to_string(),
            Style::default().fg(Color::DarkGray),
        )
    } else {
        (format!("󰍉 {}", query), Style::default())
    };
    let block = Block::default()
        .title("Search")
        .borders(Borders::ALL)
        .border_style(if focused {
            Style::default().fg(Color::Yellow)
        } else {
            Style::default()
        });
    let input = Paragraph::new(Line::from(Span::styled(content, style))).block(block);
    frame.render_widget(input, area);
}

fn clear_kitty_preview(backend: &PreviewBackend) {
    if matches!(backend.kind, PreviewBackendKind::Kitty) {
        let _ = Command::new("kitty")
            .args(["+kitten", "icat", "--clear", "--stdin=no"])
            .status();
    }
}

fn clear_picker_preview(backend: &PreviewBackend, state: &mut PickerState) {
    if let Some(area) = state.last_image_area {
        backend.render(None, area);
    }
    state.image_visible = false;
    state.preview_dirty = false;
    state.force_clear = true;
}

fn clear_active_preview(
    backend: &PreviewBackend,
    tab: BrowseTab,
    theme: &mut PickerState,
    waybar: &mut PickerState,
    walker: &mut PickerState,
    hyprlock: &mut PickerState,
    unlock: &mut PickerState,
    starship: &mut PickerState,
    presets: &mut PickerState,
) {
    if let Some(state) = active_picker_mut(
        tab, theme, waybar, walker, hyprlock, unlock, starship, presets,
    ) {
        clear_picker_preview(backend, state);
    }
}

fn mark_force_clear(
    theme: &mut PickerState,
    waybar: &mut PickerState,
    walker: &mut PickerState,
    hyprlock: &mut PickerState,
    unlock: &mut PickerState,
    starship: &mut PickerState,
    presets: &mut PickerState,
) {
    theme.force_clear = true;
    waybar.force_clear = true;
    walker.force_clear = true;
    hyprlock.force_clear = true;
    unlock.force_clear = true;
    starship.force_clear = true;
    presets.force_clear = true;
}

fn active_picker_mut<'a>(
    tab: BrowseTab,
    theme: &'a mut PickerState,
    waybar: &'a mut PickerState,
    walker: &'a mut PickerState,
    hyprlock: &'a mut PickerState,
    unlock: &'a mut PickerState,
    starship: &'a mut PickerState,
    presets: &'a mut PickerState,
) -> Option<&'a mut PickerState> {
    match tab {
        BrowseTab::Theme => Some(theme),
        BrowseTab::Waybar => Some(waybar),
        BrowseTab::Walker => Some(walker),
        BrowseTab::Hyprlock => Some(hyprlock),
        BrowseTab::Unlock => Some(unlock),
        BrowseTab::Starship => Some(starship),
        BrowseTab::Presets => Some(presets),
        BrowseTab::Review => None,
    }
}

fn rebuild_active_filtered(
    tab: BrowseTab,
    theme: &mut PickerState,
    waybar: &mut PickerState,
    walker: &mut PickerState,
    hyprlock: &mut PickerState,
    unlock: &mut PickerState,
    starship: &mut PickerState,
    presets: &mut PickerState,
    theme_items: &[OptionItem],
    waybar_items: &[LabeledItem],
    walker_items: &[LabeledItem],
    hyprlock_items: &[LabeledItem],
    unlock_items: &[LabeledItem],
    starship_items: &[LabeledItem],
    preset_items: &[PresetItem],
) {
    match tab {
        BrowseTab::Theme => rebuild_filtered(theme, theme_items),
        BrowseTab::Waybar => rebuild_filtered(waybar, waybar_items),
        BrowseTab::Walker => rebuild_filtered(walker, walker_items),
        BrowseTab::Hyprlock => rebuild_filtered(hyprlock, hyprlock_items),
        BrowseTab::Unlock => rebuild_filtered(unlock, unlock_items),
        BrowseTab::Starship => rebuild_filtered(starship, starship_items),
        BrowseTab::Presets => rebuild_filtered(presets, preset_items),
        BrowseTab::Review => {}
    }
}

fn tab_index(tab: BrowseTab) -> usize {
    match tab {
        BrowseTab::Theme => 0,
        BrowseTab::Waybar => 1,
        BrowseTab::Walker => 2,
        BrowseTab::Hyprlock => 3,
        BrowseTab::Unlock => 4,
        BrowseTab::Starship => 5,
        BrowseTab::Review => 6,
        BrowseTab::Presets => 7,
    }
}

fn tab_from_index(index: usize) -> BrowseTab {
    match index {
        0 => BrowseTab::Theme,
        1 => BrowseTab::Waybar,
        2 => BrowseTab::Walker,
        3 => BrowseTab::Hyprlock,
        4 => BrowseTab::Unlock,
        5 => BrowseTab::Starship,
        6 => BrowseTab::Review,
        _ => BrowseTab::Presets,
    }
}

fn next_tab(tab: BrowseTab) -> BrowseTab {
    tab_from_index((tab_index(tab) + 1) % 8)
}

fn previous_tab(tab: BrowseTab) -> BrowseTab {
    tab_from_index((tab_index(tab) + 7) % 8)
}

fn tab_index_from_click(ranges: &[(u16, u16, usize)], column: u16) -> Option<usize> {
    ranges
        .iter()
        .find(|(start, end, _)| column >= *start && column <= *end)
        .map(|(_, _, idx)| *idx)
}

fn current_theme_value(items: &[OptionItem], state: &PickerState) -> Option<String> {
    let index = selected_item_index(state, items)?;
    Some(items[index].value.clone())
}

fn current_preset_name(items: &[PresetItem], state: &PickerState) -> Option<String> {
    let index = selected_item_index(state, items)?;
    Some(items[index].name.clone())
}

/// Points `state.list_state` at the real item index `item_index`, correctly
/// accounting for whichever domain `list_state` currently indexes into
/// (flattened header+item rows when grouping is active, `filtered_indices`
/// otherwise — see `selected_item_index`). If the item lives in a currently
/// collapsed group, that group is expanded first so the item is actually
/// reachable — e.g. loading a preset whose waybar/theme happens to live in
/// a group you haven't looked at yet should still show and select it.
fn select_real_item<T: ItemView>(state: &mut PickerState, items: &[T], item_index: usize) -> bool {
    if item_index >= items.len() {
        return false;
    }
    if let Some(group) = items[item_index].group() {
        state.collapsed_groups.remove(group);
    }
    if grouping_active(items, &state.search_query) {
        let rows = build_display_rows(items, &state.filtered_indices, &state.collapsed_groups);
        if let Some(pos) = rows
            .iter()
            .position(|row| matches!(row, DisplayRow::Item(idx) if *idx == item_index))
        {
            state.list_state.select(Some(pos));
            state.last_selected = Some(item_index);
            return true;
        }
        return false;
    }
    if let Some(pos) = state.filtered_indices.iter().position(|&idx| idx == item_index) {
        state.list_state.select(Some(pos));
        state.last_selected = Some(item_index);
        return true;
    }
    false
}

fn select_option_by_value(state: &mut PickerState, items: &[OptionItem], value: &str) -> bool {
    match items.iter().position(|item| item.value == value) {
        Some(item_index) => select_real_item(state, items, item_index),
        None => false,
    }
}

fn select_preset_by_name(state: &mut PickerState, items: &[PresetItem], name: &str) -> bool {
    match items.iter().position(|item| item.name == name) {
        Some(item_index) => select_real_item(state, items, item_index),
        None => false,
    }
}

fn preset_waybar_key(preset: &presets::PresetDefinition) -> Option<(String, String)> {
    match &preset.waybar {
        presets::PresetWaybarValue::None => Some(("none".to_string(), "none".to_string())),
        presets::PresetWaybarValue::Auto => Some(("theme".to_string(), "theme".to_string())),
        presets::PresetWaybarValue::Named(name) => Some(("named".to_string(), name.clone())),
    }
}

fn preset_walker_key(preset: &presets::PresetDefinition) -> Option<(String, String)> {
    match &preset.walker {
        presets::PresetWalkerValue::None => Some(("none".to_string(), "none".to_string())),
        presets::PresetWalkerValue::Auto => Some(("theme".to_string(), "theme".to_string())),
        presets::PresetWalkerValue::Named(name) => Some(("named".to_string(), name.clone())),
    }
}

fn preset_hyprlock_key(preset: &presets::PresetDefinition) -> Option<(String, String)> {
    match &preset.hyprlock {
        presets::PresetHyprlockValue::None => Some(("none".to_string(), "none".to_string())),
        presets::PresetHyprlockValue::Auto => Some(("theme".to_string(), "theme".to_string())),
        presets::PresetHyprlockValue::Named(name) => Some(("named".to_string(), name.clone())),
    }
}

fn preset_starship_key(preset: &presets::PresetDefinition) -> Option<(String, String)> {
    match &preset.starship {
        presets::PresetStarshipValue::None => Some(("none".to_string(), "none".to_string())),
        presets::PresetStarshipValue::Theme => Some(("theme".to_string(), "theme".to_string())),
        presets::PresetStarshipValue::Preset(name) => Some(("preset".to_string(), name.clone())),
        presets::PresetStarshipValue::Named(name) => Some(("named".to_string(), name.clone())),
    }
}

fn apply_preset_to_states(
    config: &ResolvedConfig,
    preset_items: &[PresetItem],
    preset_state: &mut PickerState,
    theme_items: &[OptionItem],
    theme_state: &mut PickerState,
    selected_theme: &mut String,
    theme_path: &mut PathBuf,
    waybar_items: &mut Vec<LabeledItem>,
    waybar_state: &mut PickerState,
    walker_items: &mut Vec<LabeledItem>,
    walker_state: &mut PickerState,
    hyprlock_items: &mut Vec<LabeledItem>,
    hyprlock_state: &mut PickerState,
    starship_items: &mut Vec<LabeledItem>,
    starship_state: &mut PickerState,
) -> Result<()> {
    let name = current_preset_name(preset_items, preset_state)
        .ok_or_else(|| anyhow!("no preset selected"))?;

    let preset = presets::load_preset_definition(config, &name)?;
    let normalized = normalize_theme_name(&preset.theme);
    let mut applied_theme = normalized.clone();
    if !select_option_by_value(theme_state, theme_items, &normalized) {
        if select_option_by_value(theme_state, theme_items, &preset.theme) {
            applied_theme = preset.theme.clone();
        } else {
            return Err(anyhow!("preset theme not found in theme list"));
        }
    }

    *selected_theme = applied_theme.clone();
    *theme_path = theme_ops::resolve_theme_path(config, &applied_theme)?;

    *waybar_items = build_waybar_items(config, theme_path)?;
    *walker_items = build_walker_items(config, theme_path)?;
    *hyprlock_items = build_hyprlock_items(config, theme_path)?;
    *starship_items = build_starship_items(config, theme_path)?;
    reset_picker_cache(waybar_state);
    reset_picker_cache(walker_state);
    reset_picker_cache(hyprlock_state);
    reset_picker_cache(starship_state);
    rebuild_filtered(waybar_state, waybar_items);
    rebuild_filtered(walker_state, walker_items);
    rebuild_filtered(hyprlock_state, hyprlock_items);
    rebuild_filtered(starship_state, starship_items);

    select_item_by_key(waybar_state, waybar_items, preset_waybar_key(&preset));
    select_item_by_key(walker_state, walker_items, preset_walker_key(&preset));
    select_item_by_key(hyprlock_state, hyprlock_items, preset_hyprlock_key(&preset));
    select_item_by_key(starship_state, starship_items, preset_starship_key(&preset));
    let waybar_len = nav_len(waybar_items, waybar_state);
    let walker_len = nav_len(walker_items, walker_state);
    let hyprlock_len = nav_len(hyprlock_items, hyprlock_state);
    let starship_len = nav_len(starship_items, starship_state);
    ensure_selected(&mut waybar_state.list_state, waybar_len);
    ensure_selected(&mut walker_state.list_state, walker_len);
    ensure_selected(&mut hyprlock_state.list_state, hyprlock_len);
    ensure_selected(&mut starship_state.list_state, starship_len);

    Ok(())
}

fn build_preset_entry_from_selection(
    theme: &str,
    waybar_selection: WaybarSelection,
    walker_selection: WalkerSelection,
    hyprlock_selection: HyprlockSelection,
    starship_selection: StarshipSelection,
) -> presets::PresetEntry {
    let waybar_entry = match waybar_selection {
        WaybarSelection::NoChange => presets::PresetWaybarEntry {
            mode: Some("none".to_string()),
            name: None,
        },
        WaybarSelection::None => presets::PresetWaybarEntry {
            mode: Some("none".to_string()),
            name: None,
        },
        WaybarSelection::Auto => presets::PresetWaybarEntry {
            mode: Some("auto".to_string()),
            name: None,
        },
        WaybarSelection::Named(name) => presets::PresetWaybarEntry {
            mode: Some("named".to_string()),
            name: Some(name),
        },
    };

    let walker_entry = match walker_selection {
        WalkerSelection::NoChange => presets::PresetWalkerEntry {
            mode: Some("none".to_string()),
            name: None,
        },
        WalkerSelection::None => presets::PresetWalkerEntry {
            mode: Some("none".to_string()),
            name: None,
        },
        WalkerSelection::Auto => presets::PresetWalkerEntry {
            mode: Some("auto".to_string()),
            name: None,
        },
        WalkerSelection::Named(name) => presets::PresetWalkerEntry {
            mode: Some("named".to_string()),
            name: Some(name),
        },
    };

    let hyprlock_entry = match hyprlock_selection {
        HyprlockSelection::NoChange => presets::PresetHyprlockEntry {
            mode: Some("none".to_string()),
            name: None,
        },
        HyprlockSelection::None => presets::PresetHyprlockEntry {
            mode: Some("none".to_string()),
            name: None,
        },
        HyprlockSelection::Auto => presets::PresetHyprlockEntry {
            mode: Some("auto".to_string()),
            name: None,
        },
        HyprlockSelection::Named(name) => presets::PresetHyprlockEntry {
            mode: Some("named".to_string()),
            name: Some(name),
        },
    };

    let starship_entry = match starship_selection {
        StarshipSelection::NoChange => presets::PresetStarshipEntry {
            mode: Some("none".to_string()),
            preset: None,
            name: None,
        },
        StarshipSelection::None => presets::PresetStarshipEntry {
            mode: Some("none".to_string()),
            preset: None,
            name: None,
        },
        StarshipSelection::Preset(preset) => presets::PresetStarshipEntry {
            mode: Some("preset".to_string()),
            preset: Some(preset),
            name: None,
        },
        StarshipSelection::Named(name) => presets::PresetStarshipEntry {
            mode: Some("named".to_string()),
            preset: None,
            name: Some(name),
        },
        StarshipSelection::Theme(_) => presets::PresetStarshipEntry {
            mode: Some("theme".to_string()),
            preset: None,
            name: None,
        },
    };

    presets::PresetEntry {
        theme: Some(theme.to_string()),
        waybar: Some(waybar_entry),
        walker: Some(walker_entry),
        hyprlock: Some(hyprlock_entry),
        starship: Some(starship_entry),
    }
}

fn current_waybar_label(items: &[LabeledItem], state: &PickerState) -> String {
    let index = match selected_item_index(state, items) {
        Some(index) => index,
        None => return "No options".to_string(),
    };
    let item = &items[index];
    match item.kind.as_str() {
        "theme" => "Theme waybar".to_string(),
        "none" => "None".to_string(),
        _ => item.label.clone(),
    }
}

fn current_starship_label(items: &[LabeledItem], state: &PickerState) -> String {
    let index = match selected_item_index(state, items) {
        Some(index) => index,
        None => return "No options".to_string(),
    };
    let item = &items[index];
    match item.kind.as_str() {
        "theme" => "Theme starship".to_string(),
        "none" => "None".to_string(),
        _ => item.label.clone(),
    }
}

fn current_waybar_selection(items: &[LabeledItem], state: &PickerState) -> WaybarSelection {
    let index = match selected_item_index(state, items) {
        Some(index) => index,
        None => return WaybarSelection::NoChange,
    };
    match items[index].kind.as_str() {
        "none" => WaybarSelection::None,
        "theme" => WaybarSelection::Auto,
        _ => WaybarSelection::Named(items[index].value.clone()),
    }
}

fn current_walker_label(items: &[LabeledItem], state: &PickerState) -> String {
    let index = match selected_item_index(state, items) {
        Some(index) => index,
        None => return "No options".to_string(),
    };
    let item = &items[index];
    match item.kind.as_str() {
        "theme" => "Theme walker".to_string(),
        "none" => "None".to_string(),
        _ => item.label.clone(),
    }
}

fn current_walker_selection(items: &[LabeledItem], state: &PickerState) -> WalkerSelection {
    let index = match selected_item_index(state, items) {
        Some(index) => index,
        None => return WalkerSelection::NoChange,
    };
    match items[index].kind.as_str() {
        "none" => WalkerSelection::None,
        "theme" => WalkerSelection::Auto,
        _ => WalkerSelection::Named(items[index].value.clone()),
    }
}

fn current_hyprlock_label(items: &[LabeledItem], state: &PickerState) -> String {
    let index = match selected_item_index(state, items) {
        Some(index) => index,
        None => return "No options".to_string(),
    };
    let item = &items[index];
    match item.kind.as_str() {
        "theme" => "Theme hyprlock".to_string(),
        "none" => "None".to_string(),
        _ => item.label.clone(),
    }
}

fn current_hyprlock_selection(items: &[LabeledItem], state: &PickerState) -> HyprlockSelection {
    let index = match selected_item_index(state, items) {
        Some(index) => index,
        None => return HyprlockSelection::NoChange,
    };
    match items[index].kind.as_str() {
        "none" => HyprlockSelection::None,
        "theme" => HyprlockSelection::Auto,
        _ => HyprlockSelection::Named(items[index].value.clone()),
    }
}

fn current_unlock_label(items: &[LabeledItem], state: &PickerState) -> String {
    let index = match selected_item_index(state, items) {
        Some(index) => index,
        None => return "No options".to_string(),
    };
    let item = &items[index];
    match item.kind.as_str() {
        "none" => "No change".to_string(),
        "default" => "Default".to_string(),
        _ => item.label.clone(),
    }
}

fn current_unlock_selection(items: &[LabeledItem], state: &PickerState) -> UnlockSelection {
    let index = match selected_item_index(state, items) {
        Some(index) => index,
        None => return UnlockSelection::NoChange,
    };
    match items[index].kind.as_str() {
        "none" => UnlockSelection::NoChange,
        "default" => UnlockSelection::Default,
        _ => UnlockSelection::Named(items[index].value.clone()),
    }
}

fn current_starship_selection(
    items: &[LabeledItem],
    state: &PickerState,
    theme_path: &Path,
) -> StarshipSelection {
    let index = match selected_item_index(state, items) {
        Some(index) => index,
        None => return StarshipSelection::NoChange,
    };
    match items[index].kind.as_str() {
        "none" => StarshipSelection::None,
        "theme" => StarshipSelection::Theme(theme_path.join("starship.toml")),
        "preset" => StarshipSelection::Preset(items[index].value.clone()),
        _ => StarshipSelection::Named(items[index].value.clone()),
    }
}

fn selected_item_key(items: &[LabeledItem], state: &PickerState) -> Option<(String, String)> {
    let index = selected_item_index(state, items)?;
    Some((items[index].kind.clone(), items[index].value.clone()))
}

fn select_item_by_key(
    state: &mut PickerState,
    items: &[LabeledItem],
    key: Option<(String, String)>,
) {
    if let Some((kind, value)) = key {
        if let Some(item_index) = items
            .iter()
            .position(|item| item.kind == kind && item.value == value)
        {
            select_real_item(state, items, item_index);
        }
    }
}

fn reset_picker_cache(state: &mut PickerState) {
    state.last_code_index = None;
    state.last_preview_index = None;
    state.last_preview = None;
    state.preview_dirty = false;
    state.last_preview_text = Text::default();
    state.last_image_area = None;
    state.code_scroll = 0;
    state.image_visible = false;
    state.force_clear = true;
}

fn filter_item_indices<T: ItemView>(items: &[T], query: &str) -> Vec<usize> {
    if query.trim().is_empty() {
        return (0..items.len()).collect();
    }
    let mut scored: Vec<(i64, usize, String)> = Vec::new();
    for (idx, item) in items.iter().enumerate() {
        let label = item.label();
        if let Some(score) = fuzzy_score(&label, query) {
            scored.push((score, idx, label));
        }
    }
    scored.sort_by(|a, b| b.0.cmp(&a.0).then_with(|| a.2.cmp(&b.2)));
    scored.into_iter().map(|(_, idx, _)| idx).collect()
}

fn fuzzy_score(label: &str, query: &str) -> Option<i64> {
    let query = query.trim();
    if query.is_empty() {
        return None;
    }
    let label_lower = label.to_lowercase();
    let query_lower = query.to_lowercase();
    let label_chars: Vec<char> = label_lower.chars().collect();
    let query_chars: Vec<char> = query_lower.chars().collect();
    let qlen = query_chars.len();

    let mut score = 0i64;
    let contains_pos = label_lower.find(&query_lower);
    if let Some(pos) = contains_pos {
        score += 20_000;
        score += (5000 - pos as i64).max(0);
        if pos == 0 {
            score += 8000;
        } else if is_word_boundary(&label_chars, pos) {
            score += 2000;
        }
    }

    let mut positions: Vec<usize> = Vec::with_capacity(query_chars.len());
    let mut q = 0;
    for (i, ch) in label_chars.iter().enumerate() {
        if *ch == query_chars[q] {
            positions.push(i);
            q += 1;
            if q == query_chars.len() {
                break;
            }
        }
    }
    if q != query_chars.len() {
        return if score > 0 { Some(score) } else { None };
    }

    score += 2000;
    if positions.first() == Some(&0) {
        score += 1500;
    } else if let Some(first) = positions.first().copied() {
        if is_word_boundary(&label_chars, first) {
            score += 500;
        }
    }
    for window in positions.windows(2) {
        let prev = window[0];
        let next = window[1];
        if next == prev + 1 {
            score += 400;
        } else {
            score -= (next - prev) as i64 * 2;
        }
    }
    if qlen <= 2 && contains_pos.is_none() {
        score -= 5000;
    }
    score += 500 - label_chars.len() as i64;
    Some(score)
}

fn is_word_boundary(chars: &[char], idx: usize) -> bool {
    if idx == 0 {
        return true;
    }
    !chars[idx.saturating_sub(1)].is_alphanumeric()
}

/// Resolves "whatever `state.list_state.selected()` currently means" to a
/// real item index. When grouping is active, `list_state` indexes into the
/// flattened header+item row list (so Up/Down can reach a header); landing
/// on a header has no real item, so it falls back to the last real item that
/// was selected (keeps the preview panes showing something sensible while
/// browsing past a header). When grouping isn't active, it indexes into
/// `filtered_indices` directly, exactly as before groups existed.
fn selected_item_index<T: ItemView>(state: &PickerState, items: &[T]) -> Option<usize> {
    let idx = if grouping_active(items, &state.search_query) {
        let rows = build_display_rows(items, &state.filtered_indices, &state.collapsed_groups);
        if rows.is_empty() {
            state.last_selected
        } else {
            let pos = selected_index(&state.list_state, rows.len());
            match rows.get(pos) {
                Some(DisplayRow::Item(idx)) => Some(*idx),
                _ => state.last_selected,
            }
        }
    } else if !state.filtered_indices.is_empty() {
        let selected = selected_index(&state.list_state, state.filtered_indices.len());
        state.filtered_indices.get(selected).copied()
    } else {
        state.last_selected
    };
    match idx {
        Some(idx) if idx < items.len() => Some(idx),
        _ => None,
    }
}

fn rebuild_filtered<T: ItemView>(state: &mut PickerState, items: &[T]) {
    let previous = selected_item_index(state, items);
    state.filtered_indices = filter_item_indices(items, &state.search_query);
    let query_changed = state.search_query != state.last_query;
    state.last_query = state.search_query.clone();

    if grouping_active(items, &state.search_query) {
        let rows = build_display_rows(items, &state.filtered_indices, &state.collapsed_groups);
        if let Some(item_index) = previous {
            if let Some(pos) = rows
                .iter()
                .position(|row| matches!(row, DisplayRow::Item(idx) if *idx == item_index))
            {
                state.list_state.select(Some(pos));
                state.last_selected = Some(item_index);
                return;
            }
        }
        ensure_selected(&mut state.list_state, rows.len());
        if let Some(DisplayRow::Item(idx)) =
            rows.get(selected_index(&state.list_state, rows.len()))
        {
            state.last_selected = Some(*idx);
        }
        return;
    }

    if query_changed && !state.search_query.trim().is_empty() {
        ensure_selected(&mut state.list_state, state.filtered_indices.len());
        if let Some(selected) = state.filtered_indices.first().copied() {
            state.list_state.select(Some(0));
            state.last_selected = Some(selected);
        }
        return;
    }
    if let Some(item_index) = previous {
        if let Some(pos) = state
            .filtered_indices
            .iter()
            .position(|&idx| idx == item_index)
        {
            state.list_state.select(Some(pos));
            state.last_selected = Some(item_index);
            return;
        }
    }
    ensure_selected(&mut state.list_state, state.filtered_indices.len());
    if let Some(selected) = state
        .filtered_indices
        .get(selected_index(
            &state.list_state,
            state.filtered_indices.len(),
        ))
        .copied()
    {
        state.last_selected = Some(selected);
    }
}

/// Collapse (or expand) whatever group is currently reachable: the header
/// itself if `list_state` is parked on one, otherwise the group of the
/// currently selected item. A no-op outside a group.
fn toggle_group_collapse<T: ItemView>(state: &mut PickerState, items: &[T], collapse: bool) {
    if !grouping_active(items, &state.search_query) {
        return;
    }
    let rows = build_display_rows(items, &state.filtered_indices, &state.collapsed_groups);
    if rows.is_empty() {
        return;
    }
    let pos = selected_index(&state.list_state, rows.len());
    let (group, was_header) = match rows.get(pos) {
        Some(DisplayRow::Header { name, .. }) => (Some(name.clone()), true),
        Some(DisplayRow::Item(idx)) => (items[*idx].group().map(|g| g.to_string()), false),
        None => (None, false),
    };
    let Some(group) = group else {
        return;
    };
    if collapse {
        state.collapsed_groups.insert(group.clone());
    } else {
        state.collapsed_groups.remove(&group);
    }
    rebuild_filtered(state, items);
    if was_header {
        // rebuild_filtered only knows how to restore a real item selection;
        // when a header was toggled, stay parked on that same header rather
        // than snapping to whatever real item it falls back to.
        let new_rows = build_display_rows(items, &state.filtered_indices, &state.collapsed_groups);
        if let Some(new_pos) = new_rows
            .iter()
            .position(|row| matches!(row, DisplayRow::Header { name, .. } if *name == group))
        {
            state.list_state.select(Some(new_pos));
        }
    }
}

fn ensure_selected(state: &mut ListState, len: usize) {
    if len == 0 {
        state.select(None);
        return;
    }
    let selected = state.selected().unwrap_or(0);
    let clamped = selected.min(len.saturating_sub(1));
    state.select(Some(clamped));
}

fn selected_index(state: &ListState, len: usize) -> usize {
    if len == 0 {
        return 0;
    }
    state.selected().unwrap_or(0).min(len.saturating_sub(1))
}

fn setup_terminal() -> Result<Terminal<CrosstermBackend<Stdout>>> {
    enable_raw_mode()?;
    let mut stdout = stdout();
    execute!(stdout, terminal::EnterAlternateScreen, EnableMouseCapture)?;
    let _ = execute!(
        stdout,
        PushKeyboardEnhancementFlags(
            KeyboardEnhancementFlags::DISAMBIGUATE_ESCAPE_CODES
                | KeyboardEnhancementFlags::REPORT_EVENT_TYPES
                | KeyboardEnhancementFlags::REPORT_ALTERNATE_KEYS
        )
    );
    let backend = CrosstermBackend::new(stdout);
    Terminal::new(backend).map_err(|err| anyhow!("failed to init terminal: {err}"))
}

fn cleanup_terminal(terminal: &mut Terminal<CrosstermBackend<Stdout>>) -> Result<()> {
    disable_raw_mode()?;
    execute!(
        terminal.backend_mut(),
        DisableMouseCapture,
        PopKeyboardEnhancementFlags,
        terminal::LeaveAlternateScreen
    )?;
    terminal.show_cursor()?;
    Ok(())
}

fn inner_rect(rect: Rect) -> Rect {
    let pad = 2;
    Rect {
        x: rect.x.saturating_add(pad),
        y: rect.y.saturating_add(pad),
        width: rect.width.saturating_sub(pad * 2),
        height: rect.height.saturating_sub(pad * 2),
    }
}

fn list_inner_rect(rect: Rect) -> Rect {
    let pad = 1;
    Rect {
        x: rect.x.saturating_add(pad),
        y: rect.y.saturating_add(pad),
        width: rect.width.saturating_sub(pad * 2),
        height: rect.height.saturating_sub(pad * 2),
    }
}

fn select_index_at_row(state: &mut ListState, rect: Rect, row: u16, len: usize) {
    if len == 0 || rect.height == 0 {
        return;
    }
    let offset = state.offset();
    let relative = row.saturating_sub(rect.y) as usize;
    let index = offset.saturating_add(relative);
    if index < len {
        state.select(Some(index));
    }
}

/// Handles a left click within a tab's picker: focuses the clicked pane, and
/// for the list pane, maps the clicked screen row to either a real item
/// (selects it) or, when groups are present, a header row (toggles that
/// group's collapsed state).
fn handle_list_mouse_click<T: ItemView>(
    state: &mut PickerState,
    items: &[T],
    position: Position,
    search_area: Rect,
    list_inner: Rect,
    code_inner: Rect,
) {
    if search_area.contains(position) {
        state.focus = FocusArea::List;
    } else if list_inner.contains(position) {
        state.focus = FocusArea::List;
        if grouping_active(items, &state.search_query) {
            let rows = build_display_rows(items, &state.filtered_indices, &state.collapsed_groups);
            if list_inner.height == 0 {
                return;
            }
            let offset = state.list_state.offset();
            let relative = position.y.saturating_sub(list_inner.y) as usize;
            let clicked = offset.saturating_add(relative);
            match rows.get(clicked) {
                Some(DisplayRow::Header { name, .. }) => {
                    let name = name.clone();
                    if state.collapsed_groups.contains(&name) {
                        state.collapsed_groups.remove(&name);
                    } else {
                        state.collapsed_groups.insert(name);
                    }
                    rebuild_filtered(state, items);
                }
                Some(DisplayRow::Item(_)) => {
                    state.list_state.select(Some(clicked));
                }
                None => {}
            }
        } else {
            select_index_at_row(
                &mut state.list_state,
                list_inner,
                position.y,
                state.filtered_indices.len(),
            );
        }
    } else if code_inner.contains(position) {
        state.focus = FocusArea::Code;
    }
}

fn next_index(current: Option<usize>, len: usize) -> usize {
    if len == 0 {
        return 0;
    }
    match current {
        Some(idx) => (idx + 1) % len,
        None => 0,
    }
}

fn previous_index(current: Option<usize>, len: usize) -> usize {
    if len == 0 {
        return 0;
    }
    match current {
        Some(idx) => {
            if idx == 0 {
                len - 1
            } else {
                idx - 1
            }
        }
        None => 0,
    }
}

fn list_waybar_themes(waybar_themes_dir: &Path) -> Result<Vec<DiscoveredTheme>> {
    discover_grouped_themes(
        waybar_themes_dir,
        |path| path.join("config.jsonc").is_file() && path.join("style.css").is_file(),
        |_name| false,
    )
}

fn list_walker_themes(walker_themes_dir: &Path) -> Result<Vec<DiscoveredTheme>> {
    // Walker themes require style.css, layout.xml is optional. "cc-auto" is
    // the auto-generated theme and is never a pickable entry.
    discover_grouped_themes(
        walker_themes_dir,
        |path| path.join("style.css").is_file(),
        |name| name == "cc-auto",
    )
}

fn list_hyprlock_themes(hyprlock_themes_dir: &Path) -> Result<Vec<String>> {
    if !hyprlock_themes_dir.is_dir() {
        return Ok(Vec::new());
    }
    let mut entries = Vec::new();
    for entry in fs::read_dir(hyprlock_themes_dir)? {
        let entry = entry?;
        let path = entry.path();
        if path.is_dir() && path.join("hyprlock.conf").is_file() {
            if let Some(name) = path.file_name().and_then(|n| n.to_str()) {
                entries.push(name.to_string());
            }
        }
    }
    entries.sort();
    Ok(entries)
}

fn unlock_theme_dir(config: &ResolvedConfig, name: &str) -> Option<PathBuf> {
    let mut roots = vec![config.theme_root_dir.clone()];
    if let Some(root) = crate::omarchy::detect_omarchy_root(config) {
        roots.push(root.join("themes"));
    }
    roots
        .into_iter()
        .map(|root| root.join(name))
        .find(|path| path.join("preview-unlock.png").is_file())
}

fn unlock_theme_preview(config: &ResolvedConfig, name: &str) -> Option<PathBuf> {
    unlock_theme_dir(config, name)
        .map(|path| path.join("preview-unlock.png"))
        .filter(|path| path.is_file())
}

fn omarchy_default_unlock_preview(config: &ResolvedConfig) -> Option<PathBuf> {
    crate::omarchy::detect_omarchy_root(config)
        .map(|root| root.join("default/plymouth/preview-unlock.png"))
        .filter(|path| path.is_file())
}

fn list_starship_presets() -> Vec<String> {
    if !command_exists("starship") {
        return Vec::new();
    }
    if let Ok(output) = Command::new("starship").args(["preset", "--list"]).output() {
        if output.status.success() {
            return parse_lines(&output.stdout);
        }
    }
    if let Ok(output) = Command::new("starship").args(["preset", "-l"]).output() {
        if output.status.success() {
            return parse_lines(&output.stdout);
        }
    }
    Vec::new()
}

fn list_starship_themes(dir: &Path) -> Result<Vec<String>> {
    if !dir.is_dir() {
        return Ok(Vec::new());
    }
    let mut themes = Vec::new();
    for entry in fs::read_dir(dir)? {
        let entry = entry?;
        let path = entry.path();
        if path.is_file() {
            if let Some(ext) = path.extension().and_then(|e| e.to_str()) {
                if ext.eq_ignore_ascii_case("toml") {
                    if let Some(stem) = path.file_stem().and_then(|s| s.to_str()) {
                        themes.push(stem.to_string());
                    }
                }
            }
        }
    }
    themes.sort();
    Ok(themes)
}

fn display_theme_name(name: &str) -> String {
    if name == "omarchy-default" {
        "Omarchy-Default".to_string()
    } else {
        name.to_string()
    }
}

fn pin_omarchy_default_first(names: &mut Vec<String>) {
    names.sort();
    if let Some(index) = names.iter().position(|name| name == "omarchy-default") {
        if index != 0 {
            let value = names.remove(index);
            names.insert(0, value);
        }
    }
}

fn convert_text(text: CoreText<'static>) -> Text<'static> {
    let lines = text.lines.into_iter().map(convert_line).collect::<Vec<_>>();
    Text {
        lines,
        style: convert_style(text.style),
        alignment: text.alignment.map(convert_alignment),
    }
}

fn convert_line(line: CoreLine<'static>) -> Line<'static> {
    let spans = line.spans.into_iter().map(convert_span).collect::<Vec<_>>();
    Line {
        spans,
        style: convert_style(line.style),
        alignment: line.alignment.map(convert_alignment),
    }
}

fn convert_span(span: CoreSpan<'static>) -> ratatui::text::Span<'static> {
    ratatui::text::Span {
        content: span.content,
        style: convert_style(span.style),
    }
}

fn convert_style(style: CoreStyle) -> Style {
    let mut out = Style::default();
    out.fg = style.fg.map(convert_color);
    out.bg = style.bg.map(convert_color);
    out.add_modifier = convert_modifier(style.add_modifier);
    out.sub_modifier = convert_modifier(style.sub_modifier);
    out
}

fn convert_modifier(modifier: CoreModifier) -> Modifier {
    Modifier::from_bits_truncate(modifier.bits())
}

fn convert_alignment(alignment: CoreAlignment) -> ratatui::layout::Alignment {
    match alignment {
        CoreAlignment::Left => ratatui::layout::Alignment::Left,
        CoreAlignment::Center => ratatui::layout::Alignment::Center,
        CoreAlignment::Right => ratatui::layout::Alignment::Right,
    }
}

fn convert_color(color: CoreColor) -> Color {
    match color {
        CoreColor::Reset => Color::Reset,
        CoreColor::Black => Color::Black,
        CoreColor::Red => Color::Red,
        CoreColor::Green => Color::Green,
        CoreColor::Yellow => Color::Yellow,
        CoreColor::Blue => Color::Blue,
        CoreColor::Magenta => Color::Magenta,
        CoreColor::Cyan => Color::Cyan,
        CoreColor::Gray => Color::Gray,
        CoreColor::DarkGray => Color::DarkGray,
        CoreColor::LightRed => Color::LightRed,
        CoreColor::LightGreen => Color::LightGreen,
        CoreColor::LightYellow => Color::LightYellow,
        CoreColor::LightBlue => Color::LightBlue,
        CoreColor::LightMagenta => Color::LightMagenta,
        CoreColor::LightCyan => Color::LightCyan,
        CoreColor::White => Color::White,
        CoreColor::Rgb(r, g, b) => Color::Rgb(r, g, b),
        CoreColor::Indexed(i) => Color::Indexed(i),
    }
}

fn parse_lines(output: &[u8]) -> Vec<String> {
    String::from_utf8_lossy(output)
        .lines()
        .map(|line| line.trim())
        .filter(|line| !line.is_empty())
        .map(|line| line.to_string())
        .collect()
}

fn command_exists(cmd: &str) -> bool {
    which::which(cmd).is_ok()
}

fn apply_key_matches(config: &ResolvedConfig, key: event::KeyEvent) -> bool {
    if let Some(spec) = config.tui_apply_key.as_deref() {
        if let Some(binding) = parse_apply_key(spec) {
            return key.code == binding.code && key.modifiers.contains(binding.modifiers);
        }
    }

    let default_keys = [
        ApplyKey::new(KeyCode::Enter, KeyModifiers::CONTROL),
        ApplyKey::new(KeyCode::Char('m'), KeyModifiers::CONTROL),
        ApplyKey::new(KeyCode::Char('j'), KeyModifiers::CONTROL),
    ];
    if default_keys
        .iter()
        .any(|binding| key.code == binding.code && key.modifiers.contains(binding.modifiers))
    {
        return true;
    }

    // Some terminals do not emit a distinct Ctrl+Enter event and only report Enter.
    key.code == KeyCode::Enter && key.modifiers.is_empty()
}

fn parse_apply_key(spec: &str) -> Option<ApplyKey> {
    let mut modifiers = KeyModifiers::empty();
    let mut code: Option<KeyCode> = None;
    for part in spec.split('+').map(|p| p.trim().to_ascii_lowercase()) {
        if part.is_empty() {
            continue;
        }
        match part.as_str() {
            "ctrl" | "control" => modifiers |= KeyModifiers::CONTROL,
            "alt" => modifiers |= KeyModifiers::ALT,
            "shift" => modifiers |= KeyModifiers::SHIFT,
            "enter" | "return" => code = Some(KeyCode::Enter),
            "esc" | "escape" => code = Some(KeyCode::Esc),
            "tab" => code = Some(KeyCode::Tab),
            _ => {
                if part.len() == 1 {
                    if let Some(ch) = part.chars().next() {
                        code = Some(KeyCode::Char(ch));
                    }
                } else {
                    return None;
                }
            }
        }
    }
    code.map(|code| ApplyKey { code, modifiers })
}

#[derive(Clone, Copy)]
struct ApplyKey {
    code: KeyCode,
    modifiers: KeyModifiers,
}

impl ApplyKey {
    fn new(code: KeyCode, modifiers: KeyModifiers) -> Self {
        Self { code, modifiers }
    }
}

fn term_contains(value: &str) -> bool {
    std::env::var("TERM")
        .unwrap_or_default()
        .to_lowercase()
        .contains(value)
}

fn term_program_contains(value: &str) -> bool {
    std::env::var("TERM_PROGRAM")
        .unwrap_or_default()
        .to_lowercase()
        .contains(value)
}

trait ItemView {
    fn label(&self) -> String;
    fn group(&self) -> Option<&str> {
        None
    }
    fn value(&self) -> &str {
        ""
    }
}

impl ItemView for OptionItem {
    fn label(&self) -> String {
        self.label.clone()
    }
    fn group(&self) -> Option<&str> {
        self.group.as_deref()
    }
    fn value(&self) -> &str {
        &self.value
    }
}

impl ItemView for LabeledItem {
    fn label(&self) -> String {
        self.label.clone()
    }
    fn value(&self) -> &str {
        &self.value
    }
    fn group(&self) -> Option<&str> {
        self.group.as_deref()
    }
}

impl ItemView for PresetItem {
    fn label(&self) -> String {
        self.label.clone()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    struct DummyItem {
        label: String,
    }

    impl ItemView for DummyItem {
        fn label(&self) -> String {
            self.label.clone()
        }
    }

    #[test]
    fn filter_items_empty_query_returns_all() {
        let items = vec![
            DummyItem {
                label: "alpha".to_string(),
            },
            DummyItem {
                label: "bravo".to_string(),
            },
            DummyItem {
                label: "charlie".to_string(),
            },
        ];
        let filtered = filter_item_indices(&items, "");
        assert_eq!(filtered, vec![0, 1, 2]);
    }

    #[test]
    fn filter_items_with_query_returns_matches() {
        let items = vec![
            DummyItem {
                label: "alpha".to_string(),
            },
            DummyItem {
                label: "bravo".to_string(),
            },
            DummyItem {
                label: "charlie".to_string(),
            },
        ];
        let filtered = filter_item_indices(&items, "br");
        assert_eq!(filtered, vec![1]);
    }

    #[test]
    fn rebuild_filtered_preserves_last_selected() {
        let items = vec![
            DummyItem {
                label: "alpha".to_string(),
            },
            DummyItem {
                label: "bravo".to_string(),
            },
        ];
        let mut state = PickerState::new();
        rebuild_filtered(&mut state, &items);
        state.list_state.select(Some(1));
        rebuild_filtered(&mut state, &items);
        assert_eq!(state.last_selected, Some(1));

        state.search_query = "zzz".to_string();
        rebuild_filtered(&mut state, &items);
        assert!(state.filtered_indices.is_empty());
        assert_eq!(state.last_selected, Some(1));
    }

    #[test]
    fn filter_items_falls_back_to_substring_match() {
        let items = vec![
            DummyItem {
                label: "dracula".to_string(),
            },
            DummyItem {
                label: "nord".to_string(),
            },
        ];
        let filtered = filter_item_indices(&items, "dra");
        assert_eq!(filtered, vec![0]);
    }

    #[test]
    fn filter_items_supports_subsequence_match() {
        let items = vec![
            DummyItem {
                label: "dracula".to_string(),
            },
            DummyItem {
                label: "nord".to_string(),
            },
        ];
        let filtered = filter_item_indices(&items, "drc");
        assert_eq!(filtered, vec![0]);
    }

    #[test]
    fn preset_keys_map_to_items() {
        let preset = presets::PresetDefinition {
            name: "Test".to_string(),
            theme: "noir".to_string(),
            waybar: presets::PresetWaybarValue::None,
            walker: presets::PresetWalkerValue::None,
            hyprlock: presets::PresetHyprlockValue::None,
            starship: presets::PresetStarshipValue::Theme,
        };
        assert_eq!(
            preset_waybar_key(&preset),
            Some(("none".to_string(), "none".to_string()))
        );
        assert_eq!(
            preset_walker_key(&preset),
            Some(("none".to_string(), "none".to_string()))
        );
        assert_eq!(
            preset_hyprlock_key(&preset),
            Some(("none".to_string(), "none".to_string()))
        );
        assert_eq!(
            preset_starship_key(&preset),
            Some(("theme".to_string(), "theme".to_string()))
        );
    }

    #[test]
    fn pin_omarchy_default_first_moves_default_to_top() {
        let mut names = vec![
            "zeta".to_string(),
            "omarchy-default".to_string(),
            "alpha".to_string(),
        ];
        pin_omarchy_default_first(&mut names);
        assert_eq!(
            names,
            vec![
                "omarchy-default".to_string(),
                "alpha".to_string(),
                "zeta".to_string()
            ]
        );
    }

    #[test]
    fn display_theme_name_formats_omarchy_default() {
        assert_eq!(display_theme_name("omarchy-default"), "Omarchy-Default");
        assert_eq!(display_theme_name("catppuccin"), "catppuccin");
    }

    #[test]
    fn current_unlock_selection_maps_items() {
        let items = vec![
            OptionItem::with_kind(
                "No Unlock change".to_string(),
                "none".to_string(),
                "none",
                None,
            ),
            OptionItem::with_kind(
                "Default".to_string(),
                "default".to_string(),
                "default",
                None,
            ),
            OptionItem::with_kind(
                "Tokyo Night".to_string(),
                "tokyo-night".to_string(),
                "named",
                None,
            ),
        ];
        let mut state = PickerState::new();
        rebuild_filtered(&mut state, &items);

        state.list_state.select(Some(0));
        assert!(matches!(
            current_unlock_selection(&items, &state),
            UnlockSelection::NoChange
        ));

        state.list_state.select(Some(1));
        assert!(matches!(
            current_unlock_selection(&items, &state),
            UnlockSelection::Default
        ));

        state.list_state.select(Some(2));
        match current_unlock_selection(&items, &state) {
            UnlockSelection::Named(name) => assert_eq!(name, "tokyo-night"),
            other => panic!("unexpected unlock selection: {other:?}"),
        }
    }

    #[test]
    fn clear_picker_preview_resets_image_state() {
        let backend = PreviewBackend {
            kind: PreviewBackendKind::None,
        };
        let mut state = PickerState::new();
        state.last_image_area = Some(Rect::new(1, 2, 10, 5));
        state.image_visible = true;
        state.preview_dirty = true;
        state.force_clear = false;

        clear_picker_preview(&backend, &mut state);

        assert!(!state.image_visible);
        assert!(!state.preview_dirty);
        assert!(state.force_clear);
    }

    #[test]
    fn decide_preview_action_logic() {
        assert_eq!(
            decide_preview_action(false, false, false),
            PreviewAction::None
        );
        assert_eq!(
            decide_preview_action(false, true, false),
            PreviewAction::Clear
        );
        assert_eq!(
            decide_preview_action(true, false, false),
            PreviewAction::Render
        );
        assert_eq!(
            decide_preview_action(true, true, false),
            PreviewAction::None
        );
        assert_eq!(
            decide_preview_action(true, true, true),
            PreviewAction::ClearAndRender
        );
        assert_eq!(
            decide_preview_action(false, false, true),
            PreviewAction::Clear
        );
    }

    #[test]
    fn preview_backend_detection_precedence() {
        assert_eq!(
            detect_preview_backend_kind(true, true, true, true),
            PreviewBackendKind::Kitty
        );
        assert_eq!(
            detect_preview_backend_kind(false, true, false, true),
            PreviewBackendKind::Sixel
        );
        assert_eq!(
            detect_preview_backend_kind(false, true, false, false),
            PreviewBackendKind::Chafa
        );
        assert_eq!(
            detect_preview_backend_kind(false, false, false, true),
            PreviewBackendKind::None
        );
    }

    struct GroupedDummyItem {
        label: String,
        group: Option<String>,
    }

    impl ItemView for GroupedDummyItem {
        fn label(&self) -> String {
            self.label.clone()
        }
        fn group(&self) -> Option<&str> {
            self.group.as_deref()
        }
        fn value(&self) -> &str {
            &self.label
        }
    }

    fn grouped_fixture() -> Vec<GroupedDummyItem> {
        // Pre-sorted the way build_waybar_items/build_walker_items sort
        // discovered entries: ungrouped first, then grouped alphabetically.
        vec![
            GroupedDummyItem {
                label: "none".to_string(),
                group: None,
            },
            GroupedDummyItem {
                label: "atif-pill".to_string(),
                group: Some("atif".to_string()),
            },
            GroupedDummyItem {
                label: "atif-dock".to_string(),
                group: Some("atif".to_string()),
            },
            GroupedDummyItem {
                label: "cc-squared".to_string(),
                group: Some("cc".to_string()),
            },
        ]
    }

    #[test]
    fn all_group_names_collects_every_distinct_group() {
        let items = grouped_fixture();
        let groups = all_group_names(&items);
        assert_eq!(groups.len(), 2);
        assert!(groups.contains("atif"));
        assert!(groups.contains("cc"));
    }

    #[test]
    fn initial_collapsed_groups_collapses_everything_without_an_active_value() {
        let items = grouped_fixture();
        let collapsed = initial_collapsed_groups(&items, None);
        assert_eq!(collapsed.len(), 2);
        assert!(collapsed.contains("atif"));
        assert!(collapsed.contains("cc"));
    }

    #[test]
    fn initial_collapsed_groups_leaves_the_active_items_group_expanded() {
        let items = grouped_fixture();
        let collapsed = initial_collapsed_groups(&items, Some("atif-pill"));
        assert_eq!(collapsed.len(), 1);
        assert!(collapsed.contains("cc"));
        assert!(!collapsed.contains("atif"));
    }

    #[test]
    fn initial_collapsed_groups_collapses_all_when_active_value_is_ungrouped() {
        let items = grouped_fixture();
        let collapsed = initial_collapsed_groups(&items, Some("none"));
        assert_eq!(collapsed.len(), 2);
    }

    #[test]
    fn filter_item_indices_is_unaffected_by_collapsed_groups() {
        // Hiding collapsed-group items is build_display_rows' job now, not
        // filter_item_indices' — this is what lets a collapsed group's
        // header still report an accurate item count.
        let items = grouped_fixture();
        let filtered = filter_item_indices(&items, "");
        assert_eq!(filtered, vec![0, 1, 2, 3]);
    }

    #[test]
    fn build_display_rows_inserts_header_per_group_and_skips_collapsed_items() {
        let items = grouped_fixture();
        let mut collapsed = std::collections::HashSet::new();
        collapsed.insert("atif".to_string());
        let filtered = filter_item_indices(&items, "");
        let rows = build_display_rows(&items, &filtered, &collapsed);

        // Expected shape: ungrouped item 0, "atif" header (still shown even
        // though collapsed, with no item rows under it), "cc" header, item 3.
        assert_eq!(rows.len(), 4);
        assert!(matches!(&rows[0], DisplayRow::Item(0)));
        assert!(matches!(&rows[1], DisplayRow::Header { name, count } if name == "atif" && *count == 2));
        assert!(matches!(&rows[2], DisplayRow::Header { name, .. } if name == "cc"));
        assert!(matches!(&rows[3], DisplayRow::Item(3)));
    }

    #[test]
    fn build_display_rows_shows_items_for_expanded_groups() {
        let items = grouped_fixture();
        let collapsed = std::collections::HashSet::new();
        let filtered = filter_item_indices(&items, "");
        let rows = build_display_rows(&items, &filtered, &collapsed);

        // Ungrouped item, "atif" header + its 2 items, "cc" header + its item.
        assert_eq!(rows.len(), 6);
        assert!(matches!(&rows[0], DisplayRow::Item(0)));
        assert!(matches!(&rows[1], DisplayRow::Header { name, .. } if name == "atif"));
        assert!(matches!(&rows[2], DisplayRow::Item(1)));
        assert!(matches!(&rows[3], DisplayRow::Item(2)));
        assert!(matches!(&rows[4], DisplayRow::Header { name, .. } if name == "cc"));
        assert!(matches!(&rows[5], DisplayRow::Item(3)));
    }

    #[test]
    fn toggle_group_collapse_works_when_parked_on_the_header_itself() {
        let items = grouped_fixture();
        let mut state = PickerState::new();
        rebuild_filtered(&mut state, &items);
        // Navigate to the "atif" header: ungrouped item (row 0), then header (row 1).
        state.list_state.select(Some(1));
        toggle_group_collapse(&mut state, &items, true);
        assert!(state.collapsed_groups.contains("atif"));

        toggle_group_collapse(&mut state, &items, false);
        assert!(!state.collapsed_groups.contains("atif"));
    }

    #[test]
    fn down_arrow_navigation_reaches_a_header_row() {
        // Regression test: Up/Down must be able to land on a header row at
        // all (via the same `nav_len` + `next_index` combo the event loop
        // uses), otherwise there's no way to select a group to expand it.
        let items = grouped_fixture();
        let mut state = PickerState::new();
        rebuild_filtered(&mut state, &items);
        assert!(matches!(state.list_state.selected(), Some(0)));

        let len = nav_len(&items, &state);
        let next = next_index(state.list_state.selected(), len);
        state.list_state.select(Some(next));

        let rows = build_display_rows(&items, &state.filtered_indices, &state.collapsed_groups);
        assert!(matches!(&rows[next], DisplayRow::Header { name, .. } if name == "atif"));
    }

    #[test]
    fn selected_item_index_falls_back_to_last_selected_when_parked_on_a_header() {
        let items = grouped_fixture();
        let mut state = PickerState::new();
        rebuild_filtered(&mut state, &items);
        assert_eq!(selected_item_index(&state, &items), Some(0));

        // Move onto the "atif" header row.
        state.list_state.select(Some(1));
        // No real item is selected while parked on a header, so this should
        // keep reporting the last real selection rather than None/garbage —
        // that's what keeps the preview panes showing something sensible.
        assert_eq!(selected_item_index(&state, &items), Some(0));
    }

    #[test]
    fn grouping_active_is_false_without_groups_or_during_search() {
        let flat_items = vec![
            DummyItem {
                label: "a".to_string(),
            },
            DummyItem {
                label: "b".to_string(),
            },
        ];
        assert!(!grouping_active(&flat_items, ""));

        let grouped_items = grouped_fixture();
        assert!(grouping_active(&grouped_items, ""));
        assert!(!grouping_active(&grouped_items, "atif"));
    }

    #[test]
    fn discover_grouped_themes_treats_subfolder_as_group() {
        let temp = tempfile::TempDir::new().unwrap();
        let root = temp.path();

        // Flat theme directly under root.
        let flat = root.join("flat-theme");
        std::fs::create_dir_all(&flat).unwrap();
        std::fs::write(flat.join("style.css"), "").unwrap();

        // Group folder containing two valid themes and one invalid entry.
        let group = root.join("my-group");
        std::fs::create_dir_all(group.join("nested-a")).unwrap();
        std::fs::write(group.join("nested-a").join("style.css"), "").unwrap();
        std::fs::create_dir_all(group.join("nested-b")).unwrap();
        std::fs::write(group.join("nested-b").join("style.css"), "").unwrap();
        std::fs::create_dir_all(group.join("not-a-theme")).unwrap();

        // Empty folder that doesn't qualify as a group (no valid children).
        std::fs::create_dir_all(root.join("empty-folder")).unwrap();

        let is_theme_dir = |p: &Path| p.join("style.css").is_file();
        let entries = discover_grouped_themes(root, is_theme_dir, |_| false).unwrap();

        let flat_entry = entries.iter().find(|e| e.name == "flat-theme").unwrap();
        assert_eq!(flat_entry.group, None);

        let nested_a = entries.iter().find(|e| e.name == "nested-a").unwrap();
        assert_eq!(nested_a.group.as_deref(), Some("my-group"));
        let nested_b = entries.iter().find(|e| e.name == "nested-b").unwrap();
        assert_eq!(nested_b.group.as_deref(), Some("my-group"));

        assert!(!entries.iter().any(|e| e.name == "not-a-theme"));
        assert!(!entries.iter().any(|e| e.name == "empty-folder"));
        assert_eq!(entries.len(), 3);
    }

    #[test]
    fn discover_grouped_themes_respects_skip_name() {
        let temp = tempfile::TempDir::new().unwrap();
        let root = temp.path();
        std::fs::create_dir_all(root.join("cc-auto")).unwrap();
        std::fs::write(root.join("cc-auto").join("style.css"), "").unwrap();
        std::fs::create_dir_all(root.join("real-theme")).unwrap();
        std::fs::write(root.join("real-theme").join("style.css"), "").unwrap();

        let is_theme_dir = |p: &Path| p.join("style.css").is_file();
        let entries = discover_grouped_themes(root, is_theme_dir, |name| name == "cc-auto").unwrap();

        assert_eq!(entries.len(), 1);
        assert_eq!(entries[0].name, "real-theme");
    }
}
