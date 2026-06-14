# launchctrl-tui

<img width="943" height="603" alt="Screenshot 2026-05-04 at 1 29 55 PM" src="https://github.com/user-attachments/assets/c59869f3-07ac-4afe-ba55-aa40e77cbd14" />


A Rust terminal UI for inspecting and controlling macOS startup, login, and background items.

## Features

- Table view with `Status`, `Type`, `User`, `Name`, `Launch`, and `Path` columns
- Detail pane with launchd metadata and expanded Background Task Management fields
- Text filtering and status filtering
- View LaunchAgents, LaunchDaemons, cron jobs, login hooks, login items, system extensions, kernel extensions, periodic scripts, and modern background items
- Control launchd plist items with `launchctl`
- Best-effort actions for modern Background Task Management rows when they expose a safe launchd plist, app path, executable path, or bundle id
- User-only startup mode that avoids sudo and root/system-readable sources
- Optional startup mode that skips sources likely to require sudo/root-readable access

## Install Latest Release

Install with Homebrew:

```sh
brew tap CarterMcAlister/tools
brew install launchctrl-tui
```

You can also use the install script:

```sh
curl -fsSL https://raw.githubusercontent.com/CarterMcAlister/launchctrl-tui/main/scripts/install-latest.sh | bash -s -- --repo CarterMcAlister/launchctrl-tui
```

The script installs to `~/.local/bin` by default. Override with:

```sh
INSTALL_DIR=/usr/local/bin scripts/install-latest.sh --repo CarterMcAlister/launchctrl-tui
```

## Build From Source

This repo uses `mise` for Rust tooling. Trust the local config once:

```sh
mise trust
mise run build
mise run dev
```

Or run without changing trust state:

```sh
MISE_TRUSTED_CONFIG_PATHS="$PWD/mise.toml" mise exec rust@1.94.1 -- cargo build
```

Run the TUI without using mise tasks:

```sh
MISE_TRUSTED_CONFIG_PATHS="$PWD/mise.toml" mise exec rust@1.94.1 -- cargo run
```

## Flags

Run a user-only view that avoids sudo and root/system-readable sources:

```sh
launchctrl-tui --user
```

`--user` lists only current-user safe sources: `~/Library/LaunchAgents`, the current user crontab, and user-safe Background Task Management rows. It excludes LaunchDaemons, `/System`, `/etc`, `/var/db`, root-owned rows, system extensions, kernel extensions, and periodic scripts. It never invokes `sudo`.

Include Apple system launch items and system-only sources:

```sh
launchctrl-tui --system
```

`--system` is the only mode that may use interactive `sudo launchctl` for system-domain actions. If macOS sudo/PAM is configured for Touch ID, authentication is handled by macOS; `launchctrl-tui` does not implement Touch ID itself.

Skip sources that commonly need sudo/root-readable access during startup:

```sh
launchctrl-tui --skip-sudo
```

You can combine `--skip-sudo` with `--system` or `--user`; `--skip-sudo` takes precedence for sudo-sensitive sources and actions. It still shows current-user safe sources but does not query the system disabled map or retry actions with sudo.

## Keybindings

- `↑` / `↓` or `j` / `k`: move selection
- `/`: edit text filter
- `f`: cycle status filter: all → running → loaded → disabled → off → info
- `t`: cycle type filter through the currently discovered type labels, including `btm/<subcategory>` labels
- `Esc`: clear filters / leave filter input
- `r`: refresh
- `b`: load/bootstrap when supported
- `u`: unload/disable when supported
- `s`: start/restart/open when supported
- `x` or `K`: kill/quit when supported
- `q`: quit

## Sources Listed

Default sources:

- Login hooks and logout hooks from `com.apple.loginwindow`
- Legacy login items from `/var/db/com.apple.xpc.launchd/loginitems.*.plist`
- Modern Login Items / Allow in Background entries from `sfltool dumpbtm`
- Cron jobs from `crontab -l`
- LaunchAgents and LaunchDaemons from:
  - `/Library/LaunchAgents`
  - `/Library/LaunchDaemons`
  - `~/Library/LaunchAgents`
  - `~/Library/LaunchDaemons`
  - `/etc/emond.d/rules`
- System extensions from `systemextensionsctl list`

With `--user`:

- `~/Library/LaunchAgents`
- Current user cron jobs from `crontab -l`
- User-safe Background Task Management rows

With `--system`:

- `/System/Library/LaunchAgents`
- `/System/Library/LaunchDaemons`
- Kernel extensions from `kmutil showloaded`
- Periodic scripts from `/etc/periodic`

With `--skip-sudo`, root/system-oriented sources are skipped where possible, including `/Library/LaunchDaemons`, `/System`, `/var/db`, `/etc`, and root-owned BTM records detected from their path/user.

## Type Labels

BTM rows are shown as `btm/<subcategory>` when possible:

- `btm/open-at-login`: app-style Open at Login item
- `btm/login`: login item
- `btm/launchd`: BTM record backed by a launchd plist/service
- `btm/helper`: background helper
- `btm/group`: background item group/developer grouping
- `btm/background`: generic background item

Other types include `agent`, `daemon`, `emond`, `login`, `cron`, `kext`, `sysext`, `periodic`, `loginhook`, and `logouthook`.

## Status Filters

- `running`: currently running items
- `loaded`: loaded but not currently running
- `disabled`: disabled items
- `off`: launchd-capable items that are not loaded/running/disabled
- `info`: view-only informational rows

## Actions

LaunchAgents, LaunchDaemons, and emond launchd plist items support:

- `b`: `launchctl bootstrap <domain> <plist>`
- `u`: `launchctl bootout <domain> <plist>`
- `s`: `launchctl kickstart -k <domain>/<label>`
- `x` / `K`: `launchctl kill TERM <domain>/<label>`

Legacy login items support:

- `b`: `launchctl enable gui/<uid>/<candidate-label>`
- `u`: `launchctl disable gui/<uid>/<candidate-label>`

The TUI tries several candidate labels for login/background items, including the displayed name, BTM identifier, bundle id, parent identifier, embedded BTM child identifiers, path-derived plist label, numeric-prefix-stripped BTM identifiers like `2.com.example.App` → `com.example.App`, and `version.*` variants. If running under `sudo`, the TUI uses `SUDO_UID` for `gui/<uid>` targets so login/background actions still target the console user. For BTM group rows, `b` / `u` attempts all embedded child identifiers instead of stopping after the first success.

Modern Background Task Management rows keep `Launch` as the category only and split disposition details into `Startup/Login Toggle`, `Background Permission`, `User Notification`, and optional `Launchctl Override` fields. Rows support actions only when there is a safe target:

- Rows with launchd plist URLs use their plist domain (`system` or `gui/<uid>`); `b` / `u` use persistent `launchctl enable/disable`, while `s` / `x` use `kickstart` / `kill`
- Rows without plist URLs try `launchctl enable/disable gui/<uid>/<candidate-label>` for `b` / `u`
- App rows can be started with `open <app>`
- Rows with executable paths can be terminated with `pkill -TERM -f <executable>`
- Rows with bundle IDs can be asked to quit with AppleScript

macOS does not expose a universal per-item CLI load/unload command for every Background Task Management record, so unsupported combinations show an explanatory message.

System-domain actions require `--system` or root privileges. In `--system`, failed system-domain `launchctl` actions may retry with interactive `sudo launchctl` rather than `sudo -n`, so macOS can present Touch ID when sudo/PAM is configured. Full Disk Access or SIP may still block some `/System` items.

## GitHub Actions

`.github/workflows/build.yml` checks formatting, runs `cargo check`, runs tests, and builds release binaries for:

- `aarch64-apple-darwin`
- `x86_64-apple-darwin`

Pushing a tag like `v0.1.0` uploads `.tar.gz` binaries and `.sha256` checksums to the GitHub release.
