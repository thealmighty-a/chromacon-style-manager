use anyhow::{anyhow, Result};
use std::collections::HashSet;
use std::env;
use std::fs;
use std::path::{Path, PathBuf};
use walkdir::WalkDir;

use crate::config::ResolvedConfig;
use crate::hyprlock;
use crate::omarchy;
use crate::paths::{
    current_theme_dir, current_theme_name, normalize_theme_name, resolve_link_target,
    title_case_theme,
};
use crate::starship;
use crate::walker;
use crate::waybar;

#[derive(Debug, Clone)]
pub enum WaybarMode {
    None,
    Auto,
    Named,
}

#[derive(Debug, Clone)]
pub enum WalkerMode {
    None,
    Auto,
    Named,
}

#[derive(Debug, Clone)]
pub enum HyprlockMode {
    None,
    Auto,
    Named,
}

#[derive(Debug, Clone)]
pub enum StarshipMode {
    None,
    Preset { preset: String },
    Named { name: String },
    Theme { path: Option<PathBuf> },
}

pub struct CommandContext<'a> {
    pub config: &'a ResolvedConfig,
    pub quiet: bool,
    pub skip_apps: bool,
    pub skip_hook: bool,
    pub waybar_mode: WaybarMode,
    pub waybar_name: Option<String>,
    pub walker_mode: WalkerMode,
    pub walker_name: Option<String>,
    pub hyprlock_mode: HyprlockMode,
    pub hyprlock_name: Option<String>,
    pub starship_mode: StarshipMode,
    pub debug_awww: bool,
}

pub fn waybar_from_defaults(config: &ResolvedConfig) -> (WaybarMode, Option<String>) {
    match config.default_waybar_mode.as_deref() {
        Some("auto") => (WaybarMode::Auto, None),
        Some("named") => (WaybarMode::Named, config.default_waybar_name.clone()),
        _ => (WaybarMode::None, None),
    }
}

pub fn walker_from_defaults(config: &ResolvedConfig) -> (WalkerMode, Option<String>) {
    match config.default_walker_mode.as_deref() {
        Some("auto") => (WalkerMode::Auto, None),
        Some("named") => (WalkerMode::Named, config.default_walker_name.clone()),
        _ => (WalkerMode::None, None),
    }
}

pub fn starship_from_defaults(config: &ResolvedConfig) -> StarshipMode {
    match config.default_starship_mode.as_deref() {
        Some("preset") => {
            if let Some(preset) = &config.default_starship_preset {
                StarshipMode::Preset {
                    preset: preset.clone(),
                }
            } else {
                StarshipMode::None
            }
        }
        Some("named") => {
            if let Some(name) = &config.default_starship_name {
                StarshipMode::Named { name: name.clone() }
            } else {
                StarshipMode::None
            }
        }
        _ => StarshipMode::None,
    }
}

pub fn hyprlock_from_defaults(config: &ResolvedConfig) -> (HyprlockMode, Option<String>) {
    match config.default_hyprlock_mode.as_deref() {
        Some("auto") => (HyprlockMode::Auto, None),
        Some("named") => (HyprlockMode::Named, config.default_hyprlock_name.clone()),
        _ => (HyprlockMode::None, None),
    }
}

pub fn cmd_list(config: &ResolvedConfig) -> Result<()> {
    let entries = sorted_theme_entries_for_config(config)?;
    for name in entries {
        println!("{}", title_case_theme(&name));
    }
    Ok(())
}

pub fn cmd_set(ctx: &CommandContext<'_>, theme_name: &str) -> Result<()> {
    let normalized = normalize_theme_name(theme_name);
    let theme_path = resolve_theme_path(ctx.config, &normalized)?;

    if is_broken_symlink(&theme_path)? {
        return Err(anyhow!(
            "theme symlink is broken: {}",
            theme_path.to_string_lossy()
        ));
    }
    if !theme_path.is_dir() && !is_symlink(&theme_path)? {
        if normalized != theme_name {
            return Err(anyhow!(
                "theme not found: {normalized} (from '{theme_name}')"
            ));
        }
        return Err(anyhow!("theme not found: {normalized}"));
    }

    omarchy::ensure_awww_daemon(ctx.config, ctx.quiet);

    let theme_source = resolve_link_target(&theme_path)?;
    let staging_dir = prepare_staging_dir(&theme_source, &ctx.config.current_theme_link)?;
    omarchy::run_omarchy_optional("theme", "set-templates", &[], ctx.quiet)?;
    replace_theme_dir(&staging_dir, &ctx.config.current_theme_link)?;
    write_theme_name(&ctx.config.current_theme_link, &normalized)?;

    let current_theme_dir = current_theme_dir(&ctx.config.current_theme_link)?;

    let mut waybar_restart = None;
    if !ctx.skip_apps {
        waybar_restart = waybar::prepare_waybar(ctx, &theme_source)?;
        walker::prepare_walker(ctx, &theme_source)?;
        hyprlock::prepare_hyprlock(ctx, &theme_source)?;
        starship::apply_starship(ctx, &theme_source)?;
    }

    if !ctx.skip_apps {
        if ctx.config.awww_transition && omarchy::command_exists("awww") {
            omarchy::stop_swaybg();
            cycle_background(ctx, &current_theme_dir)?;
            let _ = omarchy::run_awww_transition(ctx.config, ctx.quiet, ctx.debug_awww);
        } else {
            omarchy::run_omarchy_required("theme", "bg-next", &[], ctx.quiet)?;
        }
        omarchy::reload_components(ctx.quiet, waybar_restart, ctx.config.waybar_restart_logs)?;
        omarchy::apply_theme_setters(ctx.quiet)?;
    }

    if !ctx.skip_hook {
        let _ = omarchy::run_theme_hook(&normalized, ctx.quiet);
    }

    Ok(())
}

pub fn cmd_next(ctx: &CommandContext<'_>) -> Result<()> {
    let entries = sorted_theme_entries_for_config(ctx.config)?;
    if entries.is_empty() {
        return Err(anyhow!("no themes available"));
    }

    let current_name = current_theme_name(&ctx.config.current_theme_link)?;

    let next = next_theme(&entries, current_name.as_deref());
    cmd_set(ctx, &next)
}

pub fn cmd_current(config: &ResolvedConfig) -> Result<()> {
    let name = current_theme_name(&config.current_theme_link)?.ok_or_else(|| {
        anyhow!(
            "current theme not set: {}",
            config.current_theme_link.to_string_lossy()
        )
    })?;
    println!("{}", title_case_theme(&name));
    Ok(())
}

pub fn cmd_bg_next(config: &ResolvedConfig, debug_awww: bool) -> Result<()> {
    let theme_path = current_theme_dir(&config.current_theme_link)?;

    let ctx = CommandContext {
        config,
        quiet: false,
        skip_apps: false,
        skip_hook: false,
        waybar_mode: WaybarMode::None,
        waybar_name: None,
        walker_mode: WalkerMode::None,
        walker_name: None,
        hyprlock_mode: HyprlockMode::None,
        hyprlock_name: None,
        starship_mode: StarshipMode::None,
        debug_awww,
    };

    if config.awww_transition && omarchy::command_exists("awww") {
        omarchy::ensure_awww_daemon(config, false);
        omarchy::stop_swaybg();
        cycle_background(&ctx, &theme_path)?;
        let _ = omarchy::run_awww_transition(config, false, debug_awww);
    } else {
        omarchy::run_omarchy_required("theme", "bg-next", &[], false)?;
    }
    Ok(())
}

pub fn cmd_version() {
    println!("{}", env!("THEME_MANAGER_VERSION"));
}

pub fn cmd_browse_stub(_ctx: &CommandContext<'_>) -> Result<()> {
    Err(anyhow!("browse is not implemented in this command path"))
}

pub fn list_theme_entries(theme_root: &Path) -> Result<Vec<String>> {
    if !theme_root.is_dir() {
        return Err(anyhow!(
            "themes directory not found: {}",
            theme_root.to_string_lossy()
        ));
    }
    let mut entries = Vec::new();
    for entry in fs::read_dir(theme_root)? {
        let entry = entry?;
        let path = entry.path();
        if path.is_dir() || is_symlink(&path)? {
            if let Some(name) = path.file_name().and_then(|n| n.to_str()) {
                entries.push(name.to_string());
            }
        }
    }
    Ok(entries)
}

/// Flat list of theme names (bare names, no group info) — for callers that
/// just need every available theme regardless of how it's organized on disk,
/// e.g. the `list` and `next` CLI commands. Delegates to the same one-level
/// group-aware discovery `list_theme_entries_with_groups` uses, so a theme
/// nested under an organizational group folder still shows up here.
pub fn list_theme_entries_for_config(config: &ResolvedConfig) -> Result<Vec<String>> {
    Ok(list_theme_entries_with_groups(config)?
        .into_iter()
        .map(|entry| entry.name)
        .collect())
}

fn sorted_theme_entries_for_config(config: &ResolvedConfig) -> Result<Vec<String>> {
    let mut entries = list_theme_entries_for_config(config)?;
    entries.sort();
    Ok(entries)
}

pub fn resolve_theme_path(config: &ResolvedConfig, normalized: &str) -> Result<PathBuf> {
    for root in theme_roots(config) {
        let candidate = root.join(normalized);
        if candidate.is_dir() || is_symlink(&candidate)? {
            return Ok(candidate);
        }
        // Not found directly under this root: the theme may live one level
        // down inside an organizational group folder (e.g. themes/<group>/<name>).
        if let Ok(group_dirs) = fs::read_dir(&root) {
            for group_dir in group_dirs.flatten() {
                let group_path = group_dir.path();
                if !group_path.is_dir() {
                    continue;
                }
                let nested = group_path.join(normalized);
                if nested.is_dir() || is_symlink(&nested).unwrap_or(false) {
                    return Ok(nested);
                }
            }
        }
    }
    Err(anyhow!("theme not found: {normalized}"))
}

/// Resolve a bare theme name to its directory under `themes_dir`, falling back
/// to a one-level-nested group folder (`themes_dir/<group>/<name>`) when the
/// theme isn't directly under `themes_dir`. Used by apply-time code that only
/// has a stored name (CLI flags, presets.toml) and needs the real directory
/// regardless of whether the theme has since been organized into a group.
pub fn resolve_named_theme_dir(themes_dir: &Path, name: &str) -> PathBuf {
    let direct = themes_dir.join(name);
    if direct.is_dir() {
        return direct;
    }
    if let Ok(entries) = fs::read_dir(themes_dir) {
        for entry in entries.flatten() {
            let path = entry.path();
            if path.is_dir() {
                let candidate = path.join(name);
                if candidate.is_dir() {
                    return candidate;
                }
            }
        }
    }
    direct
}

/// A theme/entry discovered under a themes directory, optionally nested one
/// level inside an organizational group folder.
#[derive(Debug, Clone)]
pub struct GroupedEntry {
    pub name: String,
    pub group: Option<String>,
    pub path: PathBuf,
}

/// Like `list_theme_entries_for_config`, but also resolves one level of group
/// subfolders (a folder that isn't itself a theme, containing theme folders).
/// A directory counts as a theme if it has `colors.toml` directly inside it.
pub fn list_theme_entries_with_groups(config: &ResolvedConfig) -> Result<Vec<GroupedEntry>> {
    let mut out = Vec::new();
    let mut seen = HashSet::new();
    for root in theme_roots(config) {
        if !root.is_dir() {
            continue;
        }
        for entry in fs::read_dir(&root)? {
            let entry = entry?;
            let path = entry.path();
            if !path.is_dir() {
                continue;
            }
            if is_theme_dir(&path) {
                if let Some(name) = path.file_name().and_then(|n| n.to_str()) {
                    if seen.insert(name.to_string()) {
                        out.push(GroupedEntry {
                            name: name.to_string(),
                            group: None,
                            path: path.clone(),
                        });
                    }
                }
                continue;
            }
            let Some(group_name) = path.file_name().and_then(|n| n.to_str()) else {
                continue;
            };
            if let Ok(children) = fs::read_dir(&path) {
                for child in children.flatten() {
                    let child_path = child.path();
                    if child_path.is_dir() && is_theme_dir(&child_path) {
                        if let Some(name) = child_path.file_name().and_then(|n| n.to_str()) {
                            if seen.insert(name.to_string()) {
                                out.push(GroupedEntry {
                                    name: name.to_string(),
                                    group: Some(group_name.to_string()),
                                    path: child_path.clone(),
                                });
                            }
                        }
                    }
                }
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

/// A directory counts as a theme if it has at least one regular file
/// directly inside it (not just subdirectories). A pure organizational group
/// folder (e.g. themes/<group>/) holds only further theme folders and no
/// loose files of its own, so this cleanly tells the two apart without
/// requiring every theme to ship a specific marker file (some don't, e.g.
/// ones with no colors.toml).
fn is_theme_dir(path: &Path) -> bool {
    fs::read_dir(path)
        .map(|entries| entries.flatten().any(|entry| entry.path().is_file()))
        .unwrap_or(false)
}

fn theme_roots(config: &ResolvedConfig) -> Vec<PathBuf> {
    let mut roots = Vec::new();
    roots.push(config.theme_root_dir.clone());

    let mut omarchy_path = env::var("OMARCHY_PATH").ok().map(PathBuf::from);
    if omarchy_path.is_none() {
        if let Some(bin_dir) = &config.omarchy_bin_dir {
            if let Some(parent) = bin_dir.parent() {
                omarchy_path = Some(parent.to_path_buf());
            }
        }
    }
    if omarchy_path.is_none() {
        if let Ok(home) = env::var("HOME") {
            omarchy_path = Some(PathBuf::from(home).join(".local/share/omarchy"));
        }
    }
    if let Some(omarchy_path) = omarchy_path {
        let omarchy_themes = omarchy_path.join("themes");
        if omarchy_themes != config.theme_root_dir {
            roots.push(omarchy_themes);
        }
    }

    roots
}

fn next_theme(entries: &[String], current: Option<&str>) -> String {
    if let Some(current) = current {
        if let Some(idx) = entries.iter().position(|name| name == current) {
            let next_idx = (idx + 1) % entries.len();
            return entries[next_idx].clone();
        }
    }
    entries[0].clone()
}

fn replace_theme_dir(staging_dir: &Path, current_dir: &Path) -> Result<()> {
    if let Ok(meta) = fs::symlink_metadata(current_dir) {
        if meta.file_type().is_dir() {
            fs::remove_dir_all(current_dir)?;
        } else {
            fs::remove_file(current_dir)?;
        }
    }
    fs::rename(staging_dir, current_dir)?;
    Ok(())
}

fn ensure_parent_dir(path: &Path) -> Result<()> {
    if let Some(parent) = path.parent() {
        fs::create_dir_all(parent)?;
    }
    Ok(())
}

fn is_symlink(path: &Path) -> Result<bool> {
    match fs::symlink_metadata(path) {
        Ok(meta) => Ok(meta.file_type().is_symlink()),
        Err(err) if err.kind() == std::io::ErrorKind::NotFound => Ok(false),
        Err(err) => Err(err.into()),
    }
}

fn cycle_background(ctx: &CommandContext<'_>, theme_path: &Path) -> Result<()> {
    let mut background_dirs = Vec::new();
    let theme_backgrounds = theme_path.join("backgrounds");
    if theme_backgrounds.is_dir() {
        background_dirs.push(theme_backgrounds);
    }
    if let Some(theme_name) = current_theme_name(&ctx.config.current_theme_link)? {
        if let Some(omarchy_dir) = ctx
            .config
            .current_theme_link
            .parent()
            .and_then(|p| p.parent())
        {
            let user_backgrounds = omarchy_dir.join("backgrounds").join(theme_name);
            if user_backgrounds.is_dir() {
                background_dirs.push(user_backgrounds);
            }
        }
    }
    if background_dirs.is_empty() {
        return Ok(());
    }

    let mut images: Vec<PathBuf> = Vec::new();
    for dir in &background_dirs {
        for entry in fs::read_dir(dir)? {
            let path = entry?.path();
            if path.is_file()
                && path
                    .extension()
                    .and_then(|ext| ext.to_str())
                    .map(|ext| {
                        matches!(
                            ext.to_ascii_lowercase().as_str(),
                            "png" | "jpg" | "jpeg" | "webp"
                        )
                    })
                    .unwrap_or(false)
            {
                images.push(path);
            }
        }
    }
    images.sort();
    images.dedup();
    if images.is_empty() {
        return Ok(());
    }

    let current_link = &ctx.config.current_background_link;
    let current_target = if current_link.exists() {
        if current_link.is_symlink() {
            Some(resolve_link_target(current_link)?)
        } else {
            Some(current_link.to_path_buf())
        }
    } else {
        None
    };

    let next_index = current_target
        .as_ref()
        .and_then(|target| images.iter().position(|img| img == target))
        .map(|idx| (idx + 1) % images.len())
        .unwrap_or(0);

    let next_image = &images[next_index];
    if let Some(parent) = current_link.parent() {
        fs::create_dir_all(parent)?;
    }
    if let Ok(meta) = fs::symlink_metadata(current_link) {
        if meta.file_type().is_dir() {
            fs::remove_dir_all(current_link)?;
        } else {
            fs::remove_file(current_link)?;
        }
    }
    #[cfg(unix)]
    {
        std::os::unix::fs::symlink(next_image, current_link)?;
    }
    Ok(())
}

fn write_theme_name(current_link: &Path, theme_name: &str) -> Result<()> {
    let Some(parent) = current_link.parent() else {
        return Ok(());
    };
    fs::create_dir_all(parent)?;
    fs::write(parent.join("theme.name"), theme_name)?;
    Ok(())
}

fn is_broken_symlink(path: &Path) -> Result<bool> {
    if !is_symlink(path)? {
        return Ok(false);
    }
    Ok(fs::metadata(path).is_err())
}

fn prepare_staging_dir(theme_source: &Path, current_link: &Path) -> Result<PathBuf> {
    ensure_parent_dir(current_link)?;
    let current_parent = current_link
        .parent()
        .ok_or_else(|| anyhow!("failed to resolve current theme parent"))?;
    let staging_dir = current_parent.join("next-theme");

    if let Ok(meta) = fs::symlink_metadata(&staging_dir) {
        if meta.file_type().is_dir() {
            fs::remove_dir_all(&staging_dir)?;
        } else {
            fs::remove_file(&staging_dir)?;
        }
    }
    fs::create_dir_all(&staging_dir)?;
    copy_theme_dir(theme_source, &staging_dir)?;
    Ok(staging_dir)
}

fn copy_theme_dir(source: &Path, dest: &Path) -> Result<()> {
    for entry in WalkDir::new(source).follow_links(false) {
        let entry = entry?;
        let entry_path = entry.path();
        let rel = entry_path.strip_prefix(source)?;
        if rel.as_os_str().is_empty() {
            continue;
        }
        let target_path = dest.join(rel);
        let file_type = entry.file_type();
        if file_type.is_dir() {
            fs::create_dir_all(&target_path)?;
            continue;
        }
        if file_type.is_symlink() {
            let link_target = fs::read_link(entry_path)?;
            if let Some(parent) = target_path.parent() {
                fs::create_dir_all(parent)?;
            }
            #[cfg(unix)]
            std::os::unix::fs::symlink(link_target, &target_path)?;
            continue;
        }
        if let Some(parent) = target_path.parent() {
            fs::create_dir_all(parent)?;
        }
        fs::copy(entry_path, &target_path)?;
    }
    Ok(())
}

#[cfg(test)]
mod grouping_tests {
    use super::*;
    use tempfile::TempDir;

    fn make_theme(dir: &Path) {
        fs::create_dir_all(dir).unwrap();
        fs::write(dir.join("colors.toml"), "").unwrap();
    }

    #[test]
    fn resolve_named_theme_dir_finds_flat_theme() {
        let temp = TempDir::new().unwrap();
        let themes_dir = temp.path().join("themes");
        make_theme(&themes_dir.join("flat-one"));

        let resolved = resolve_named_theme_dir(&themes_dir, "flat-one");
        assert_eq!(resolved, themes_dir.join("flat-one"));
    }

    #[test]
    fn resolve_named_theme_dir_finds_grouped_theme() {
        let temp = TempDir::new().unwrap();
        let themes_dir = temp.path().join("themes");
        make_theme(&themes_dir.join("my-group").join("nested-one"));

        let resolved = resolve_named_theme_dir(&themes_dir, "nested-one");
        assert_eq!(resolved, themes_dir.join("my-group").join("nested-one"));
    }

    #[test]
    fn resolve_named_theme_dir_falls_back_to_direct_join_when_missing() {
        let temp = TempDir::new().unwrap();
        let themes_dir = temp.path().join("themes");
        fs::create_dir_all(&themes_dir).unwrap();

        let resolved = resolve_named_theme_dir(&themes_dir, "does-not-exist");
        assert_eq!(resolved, themes_dir.join("does-not-exist"));
    }

    #[test]
    fn is_theme_dir_requires_a_direct_file() {
        let temp = TempDir::new().unwrap();
        let theme_dir = temp.path().join("theme");
        fs::create_dir_all(&theme_dir).unwrap();
        assert!(!is_theme_dir(&theme_dir));
        // A theme without colors.toml (just some other direct file) still counts.
        fs::write(theme_dir.join("hyprland.conf"), "").unwrap();
        assert!(is_theme_dir(&theme_dir));
    }

    #[test]
    fn is_theme_dir_rejects_pure_container_folder() {
        let temp = TempDir::new().unwrap();
        let group_dir = temp.path().join("group");
        fs::create_dir_all(group_dir.join("nested-theme")).unwrap();
        fs::write(group_dir.join("nested-theme").join("colors.toml"), "").unwrap();
        // The group folder itself has no direct files, only a subdirectory.
        assert!(!is_theme_dir(&group_dir));
    }
}
