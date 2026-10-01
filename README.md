# ChromaCon Style Manager

## Overview

**ChromaCon Style Manager** is a TUI and CLI tool for switching themes (plus Waybar, Walker, Hyprlock, and Starship) on **ChromaCon**, a personal Hyprland desktop setup. It's a fork of [theme-manager-plus](https://github.com/OldJobobo/theme-manager-plus) by OldJobobo, adapted to drive ChromaCon's own `cc-*` helper scripts and `~/.config/cc/...` theme layout instead of Omarchy's. Most of the original design (the TUI, the apply pipeline, preset bundles) comes from that project — full credit to OldJobobo for it.

It is **not a replacement** for ChromaCon's own theming scripts (`cc-theme-set`, `cc-hook`, etc.).  
Think of it as a **direct, expanded interface** for driving that existing theme flow — script-compatible, hook-compatible.

### What's different from upstream

- Talks to ChromaCon's `cc-*` helpers and `~/.config/cc/themes` instead of requiring Omarchy to be installed
- Binary/command is `chromacon-style-manager`, not `chromacon-style-manager`
- Theme, Waybar, and Walker tabs support one level of folder-based grouping (put related themes in a subfolder and collapse/expand it in the TUI with Left/Right)
- The rest of this README is largely inherited from upstream and still describes the general Omarchy-shaped flow; treat command names and some paths below as illustrative rather than exact for a ChromaCon install

### What it does

- Materializes `~/.config/omarchy/current/theme` and writes `theme.name` so Omarchy apps know which theme is active
- Runs Omarchy’s own theme scripts to keep behavior identical to the menu
- Reloads common components (Waybar, terminals, notifications, etc.)
- Optionally applies:
  - A Waybar theme
  - A Walker theme
  - A Hyprlock theme
  - A Starship preset or user theme
- Supports **presets** (theme + Waybar + Walker + Hyprlock + Starship bundles)

---

## Quick Start

Build from source and install the binary to `~/.local/bin`:

```sh
cargo build --release --manifest-path rust/Cargo.toml
cp rust/target/release/chromacon-style-manager ~/.local/bin/
chromacon-style-manager
```

(This fork doesn't use upstream's `install.sh`/`uninstall.sh` curl installers — those point at OldJobobo's original repo and would install the unmodified upstream tool, not this one.)

---

## Requirements

- Omarchy installed on this machine
- Omarchy scripts available in `PATH`
  - or configured via `OMARCHY_BIN_DIR`
- Optional:
  - `starship` (for Starship presets or themes)
  - `kitty` (for Kitty graphics previews)
  - `chafa` (for text previews and Foot Sixel image previews)
  - `awww` (for wallpaper transitions; daemon is **not** auto-started)

---

## Installation

See Quick Start above — build from source with `cargo build --release` and copy the binary into place. There's no curl-pipe installer for this fork.

---

## Common Commands

- `chromacon-style-manager` — open the full-screen browser (default)
- `chromacon-style-manager list` — list available themes
- `chromacon-style-manager set <Theme>` — switch to a theme
- `chromacon-style-manager set <Theme> -w` — switch theme and apply Waybar
- `chromacon-style-manager set <Theme> -k` — switch theme and apply bundled Walker theme
- `chromacon-style-manager set <Theme> --hyprlock` — switch theme and apply bundled Hyprlock theme
- `chromacon-style-manager browse` — interactive selector (theme + Waybar + Walker + Hyprlock + Unlock + Starship)
- `chromacon-style-manager waybar <mode>` — apply Waybar only
- `chromacon-style-manager walker <mode>` — apply Walker only
- `chromacon-style-manager hyprlock <mode>` — apply Hyprlock only
- `chromacon-style-manager unlock list|set|reset` — list or apply Omarchy 3.7 boot unlock themes
- `chromacon-style-manager starship <mode>` — apply Starship only
- `chromacon-style-manager preset save|load|list|remove`
- `chromacon-style-manager version`

---

## Command Reference (Short)

### `set <theme> [-w|--waybar [name]] [-k|--walker [name]] [--hyprlock [name]] [-q|--quiet]`

Switch themes.

- `-w` (no name): use the theme’s `waybar-theme/` if present
- `-w <name>`: use `~/.config/waybar/themes/<name>/`
- `-k` (no name): use the theme’s `walker-theme/` if present
- `-k <name>`: use `~/.config/walker/themes/<name>/`
- `--hyprlock` (no name): use the theme’s `hyprlock-theme/` if present
- `--hyprlock <name>`: use `~/.config/hypr/themes/hyprlock/<name>/`
- `-q`: suppress external command output

---

### `browse`

Full-screen selector with previews.

- Tabs: **Theme**, **Waybar**, **Walker**, **Hyprlock**, **Unlock**, **Starship**, **Presets**, **Review**
- Apply with **Ctrl+Enter** by default
- Includes a **“No theme change”** option
- Component tabs include **“No Waybar change”**, **“No Walker change”**, **“No Hyprlock change”**, and **“No Starship change”** (leave current config as-is)
- Supports search and preset saving

---

### `next` / `current` / `bg-next`

- `next`: cycle to the next theme
- `current`: print current theme name
- `bg-next`: cycle background via Omarchy

---

### `install <git-url>` / `update` / `remove [theme]`

**Experimental**

- `install`: clone and activate a theme
- `update`: pull updates for git-based themes
- `remove`: delete a theme directory

---

### `preset save|load|list|remove`

Presets store a **theme + Waybar + Walker + Hyprlock + Starship** bundle.

Save example:
```sh
chromacon-style-manager preset save "Daily Driver" \
  --theme noir \
  --waybar auto \
  --walker auto \
  --hyprlock auto \
  --starship preset:bracketed-segmented
```

Load example:
```sh
chromacon-style-manager preset load "Daily Driver" -w
# or override Walker too:
chromacon-style-manager preset load "Daily Driver" -w -k omarchy-default
# or override Hyprlock:
chromacon-style-manager preset load "Daily Driver" --hyprlock omarchy-default
```

**Precedence:**  
CLI flags > preset values > config defaults

---

### `waybar <mode>`

Apply Waybar without changing the theme.

Modes:
- `auto`
- `none`
- `<name>` (shared Waybar theme)

---

### `starship <mode>`

Apply Starship without changing the theme.

Modes:
- `none`
- `theme`
- `preset:<name>`
- `named:<name>`
- `<name>` (named theme if it exists, otherwise preset)

---

### `walker <mode>`

Apply Walker without changing the theme.

Modes:
- `auto`
- `none`
- `<name>` (shared Walker theme)

---

### `hyprlock <mode>`

Apply Hyprlock without changing the theme.

Modes:
- `auto`
- `none`
- `<name>` (shared Hyprlock theme)

---

### `unlock list|set|reset`

Manage Omarchy 3.7 boot unlock themes through Omarchy’s Plymouth commands.

- `unlock list`: list themes that provide `preview-unlock.png`, plus `Default`
- `unlock set <theme>`: apply a theme’s `unlock.png` using `omarchy plymouth set-by-theme`
- `unlock set Default`: same as `unlock reset`
- `unlock reset`: restore the shipped Omarchy Plymouth/SDDM unlock theme
- Applying or resetting may prompt for sudo because Omarchy rebuilds boot/login assets.

---

### `print-config`

Print resolved configuration values.

---

### `version`

Print CLI version.

---

## Browse Mode Details

### Previews

- `preview.png` (preferred)
- `theme.png`
- First image in `backgrounds/`

All checks are case-insensitive.

### Keybindings

- Apply: `Ctrl+Enter` (default)
- Save preset: `Ctrl+S`
- Clear search: `Ctrl+U`
- Collapse/expand group: `Left`/`Right` (Theme, Waybar, Walker tabs — only shown when a group exists; put related themes in a subfolder to form a group)

### Ghostty users

Change apply key:
```toml
[tui]
apply_key = "ctrl+m"
```

Or unbind in Ghostty:
```ini
keybind = ctrl+enter=unbound
```

Restart Ghostty after changes.

---

## Waybar Integration

Two supported layouts:

**Per-theme**
```
theme/
└── waybar-theme/
    ├── config.jsonc
    └── style.css
```

**Shared**
```
~/.config/waybar/themes/<name>/
```

Behavior:
- Files are symlinked into `~/.config/waybar/` by default
- Set `WAYBAR_APPLY_MODE="copy"` to copy instead
- Waybar is restarted after apply
- If Omarchy default Waybar files are found, `omarchy-default` is auto-linked into `~/.config/waybar/themes/`

---

## Walker Integration

Supported sources:
- Theme-specific: `walker-theme/` (requires `style.css`, optional `layout.xml`)
- Shared themes: `~/.config/walker/themes/<name>/`

Behavior:
- Named Walker mode updates `~/.config/walker/config.toml` (`theme = "..."`)
- Auto mode builds `cc-auto` under `~/.config/walker/themes/`
- Walker is restarted after apply
- If Omarchy default Walker files are found, `omarchy-default` is auto-linked into `~/.config/walker/themes/`

---

## Starship Integration

Supported sources:
- Starship presets
- User themes: `~/.config/starship-themes/*.toml`
- Theme-specific: `starship.toml`

Behavior:
- Active config is written to `~/.config/starship.toml`
- Presets appear automatically in browse mode
- Example themes live in `extras/starship-themes/`
- If Omarchy default Starship files are found, `omarchy-default.toml` is auto-linked into `~/.config/starship-themes/`

---

## Hyprlock Integration

Supported layouts:
- Theme-specific: `hyprlock-theme/hyprlock.conf`
- Shared: `~/.config/hypr/themes/hyprlock/<name>/hyprlock.conf`

Behavior:
- Applied to `~/.config/omarchy/current/theme/hyprlock.conf` by symlink by default (`copy` via config/env)
- Expects `~/.config/hypr/hyprlock.conf` to source `~/.config/omarchy/current/theme/hyprlock.conf`
- `No Hyprlock change` leaves current Hyprlock config untouched
- Host `~/.config/hypr/hyprlock.conf` handling is automatic:
  - Style-only Hyprlock themes keep/restore the Omarchy wrapper layout.
  - Full-layout Hyprlock themes use a minimal source-only host config to avoid duplicate widgets.
  - If host config is custom and does not source current theme, it is preserved and a warning is printed.
- If Omarchy default Hyprlock files are found, `omarchy-default` is auto-linked into `~/.config/hypr/themes/hyprlock/` and shown in TUI.

---

## Omarchy Compatibility

ChromaCon Style Manager **calls Omarchy’s own scripts** to stay compatible.

Scripts invoked include:
- `omarchy` grouped commands when available (Omarchy 3.7+)
- `omarchy-theme-bg-next`
- `omarchy-restart-terminal`
- `omarchy-restart-waybar`
- `omarchy-restart-walker`
- `omarchy-restart-swayosd`
- `omarchy-restart-hyprctl`
- `omarchy-restart-btop`
- `omarchy-restart-opencode`
- `omarchy-restart-mako`
- `omarchy-restart-helix`
- `omarchy-theme-set-*`
- `omarchy-plymouth-set-by-theme`
- `omarchy-plymouth-reset`
- `omarchy-hook theme-set`

### Order of operations (simplified)

1. Materialize theme and write `theme.name`
2. Apply Waybar / Walker / Hyprlock / Starship (if selected)
3. Update background
4. Reload components
5. Run Omarchy app setters
6. Trigger Omarchy theme hook

Supports Omarchy templates via:
- `$OMARCHY_PATH/default/themed`
- `~/.config/omarchy/themed` (user overrides)

### Omarchy Default Source Resolution

`omarchy-default` component discovery is unified across CLI and TUI.

Root detection order:
1. `OMARCHY_PATH`
2. configured `omarchy_bin_dir` parent
3. `~/.local/share/omarchy`

Module default precedence:
- Waybar: `default/waybar/themes/omarchy-default` -> `default/waybar`
- Walker: `default/walker/themes/omarchy-default` -> `default/walker`
- Hyprlock: `default/hyprlock/themes/omarchy-default` -> `default/hyprlock` -> `themes/omarchy-default` -> `config/hypr` -> user config fallbacks under `~/.config/omarchy/`
- Starship: `default/starship/themes/omarchy-default.toml` -> `default/starship.toml` -> `default/starship/starship.toml`

Validation rules:
- Waybar requires both `config.jsonc` and `style.css`
- Walker requires `style.css`
- Hyprlock requires `hyprlock.conf`
- Starship requires a `.toml` file

Troubleshooting non-standard Omarchy layouts:
- If your Omarchy root is not `~/.local/share/omarchy`, set `OMARCHY_PATH` explicitly.
- If helper commands are installed in a custom bin location, set `OMARCHY_BIN_DIR`.
- Use `chromacon-style-manager print-config` to verify resolved paths before applying themes.
- If `omarchy-default` is missing from tabs, confirm required module files exist at one of the supported paths above.

---

## Configuration

Configuration precedence:
1. CLI flags
2. Environment variables
3. `./.chromacon-style-manager.toml`
4. `~/.config/chromacon-style-manager/config.toml`
5. Defaults

Example (`awww` transitions):
```toml
[behavior]
awww_transition = true
awww_transition_type = "grow"
awww_transition_duration = 2.4
awww_transition_fps = 60
```

Presets are stored in:
```
~/.config/chromacon-style-manager/presets.toml
```

---

## Troubleshooting

- **Theme not found** → check spelling or `THEME_ROOT_DIR`
- **Omarchy scripts missing** → ensure they are in `PATH`
- **Waybar not changing** → verify `waybar-theme/` contents
- **Missing previews** → check `preview.png`, `theme.png`, or `backgrounds/`
- **GTK / browser warnings** → usually harmless; use `-q`

---

## Development Notes

- Rust CLI entry: `rust/src/main.rs`
- Rust tests: `rust/tests/`

Run tests:
```sh
cd rust
cargo test
```

---

## FAQ

**Why not replace Omarchy’s theming?**  
Because Omarchy owns the system; this tool just drives it.

**Why symlink Waybar files?**  
To preserve Omarchy’s expected paths and imports.

**Can I use custom theme paths?**  
Yes—configure `THEME_ROOT_DIR`.

**Does browse require fzf?**  
No. The Rust TUI replaces it entirely.
