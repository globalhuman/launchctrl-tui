use std::{
    collections::{BTreeMap, BTreeSet},
    env, fs, io,
    path::{Path, PathBuf},
    process::Command,
    time::{Duration, Instant},
};

use anyhow::{Context, Result, anyhow};
use crossterm::{
    event::{self, Event, KeyCode, KeyEventKind},
    execute,
    terminal::{EnterAlternateScreen, LeaveAlternateScreen, disable_raw_mode, enable_raw_mode},
};
use plist::Value;
use ratatui::{
    Frame, Terminal,
    backend::CrosstermBackend,
    layout::{Constraint, Direction, Layout, Rect},
    style::{Color, Modifier, Style},
    text::{Line, Span},
    widgets::{Block, Borders, Cell, Clear, Paragraph, Row, Table, TableState, Wrap},
};
use walkdir::WalkDir;

fn main() -> Result<()> {
    let options = match parse_options(env::args().skip(1)) {
        Ok(options) => options,
        Err(error) if error.to_string() == "help requested" => {
            println!(
                "launchctrl-tui [--user] [--system] [--skip-sudo] [--version]\n\nKeys: ↑/↓ move, / text filter, f status filter, t type filter, Esc clear filters, r refresh, b load/bootstrap, u unload/bootout, s start/kickstart, x/K kill, q quit"
            );
            return Ok(());
        }
        Err(error) => return Err(error),
    };

    if options.show_version {
        println!("{}", version_string());
        return Ok(());
    }

    let terminal = init_terminal()?;
    let result = App::new(options)?.run(terminal);
    restore_terminal()?;
    result
}

fn parse_options(args: impl IntoIterator<Item = String>) -> Result<AppOptions> {
    let mut options = AppOptions::default();
    for arg in args {
        match arg.as_str() {
            "--user" => options.user_only = true,
            "--system" | "-s" => options.include_system = true,
            "--skip-sudo" => options.skip_sudo_sources = true,
            "--version" | "-V" => options.show_version = true,
            "--help" | "-h" => return Err(anyhow!("help requested")),
            other => return Err(anyhow!("unknown argument: {other}")),
        }
    }

    if options.user_only && options.include_system {
        return Err(anyhow!("--user and --system cannot be used together"));
    }

    Ok(options)
}

fn init_terminal() -> Result<Terminal<CrosstermBackend<io::Stdout>>> {
    enter_terminal_ui()?;
    Terminal::new(CrosstermBackend::new(io::stdout())).context("failed to initialize terminal")
}

fn enter_terminal_ui() -> Result<()> {
    enable_raw_mode()?;
    execute!(io::stdout(), EnterAlternateScreen)?;
    Ok(())
}

fn restore_terminal() -> Result<()> {
    disable_raw_mode()?;
    execute!(io::stdout(), LeaveAlternateScreen)?;
    Ok(())
}

#[derive(Clone, Debug)]
struct LaunchItem {
    label: String,
    path: PathBuf,
    domain: String,
    kind: ItemKind,
    user: String,
    launch: String,
    program: String,
    details: Vec<String>,
    loaded: bool,
    running: bool,
    disabled: bool,
    core: bool,
    legacy_disabled: bool,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
enum ItemKind {
    Agent,
    Daemon,
    EmondRule,
    LoginItem,
    BackgroundTask,
    CronJob,
    KernelExtension,
    SystemExtension,
    Periodic,
    LoginHook,
    LogoutHook,
}

impl ItemKind {
    fn as_str(self) -> &'static str {
        match self {
            Self::Agent => "agent",
            Self::Daemon => "daemon",
            Self::EmondRule => "emond",
            Self::LoginItem => "login",
            Self::BackgroundTask => "btm",
            Self::CronJob => "cron",
            Self::KernelExtension => "kext",
            Self::SystemExtension => "sysext",
            Self::Periodic => "periodic",
            Self::LoginHook => "loginhook",
            Self::LogoutHook => "logouthook",
        }
    }

    fn supports_launchctl_actions(self) -> bool {
        matches!(self, Self::Agent | Self::Daemon | Self::EmondRule)
    }
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
enum StatusFilter {
    All,
    Running,
    Loaded,
    Disabled,
    Off,
    Info,
}

impl StatusFilter {
    fn as_str(self) -> &'static str {
        match self {
            Self::All => "all",
            Self::Running => "running",
            Self::Loaded => "loaded",
            Self::Disabled => "disabled",
            Self::Off => "off",
            Self::Info => "info",
        }
    }

    fn next(self) -> Self {
        match self {
            Self::All => Self::Running,
            Self::Running => Self::Loaded,
            Self::Loaded => Self::Disabled,
            Self::Disabled => Self::Off,
            Self::Off => Self::Info,
            Self::Info => Self::All,
        }
    }

    fn matches(self, item: &LaunchItem) -> bool {
        match self {
            Self::All => true,
            Self::Running => item.running,
            Self::Loaded => item.loaded && !item.running,
            Self::Disabled => item.disabled || item.legacy_disabled,
            Self::Off => {
                item.kind.supports_launchctl_actions()
                    && !item.running
                    && !item.loaded
                    && !item.disabled
                    && !item.legacy_disabled
            }
            Self::Info => !item.kind.supports_launchctl_actions(),
        }
    }
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
enum DiscoverySource {
    UserLaunchAgents,
    LibraryLaunchAgents,
    LibraryLaunchDaemons,
    UserLaunchDaemons,
    EmondRules,
    SystemLaunchAgents,
    SystemLaunchDaemons,
    LoginHooks,
    LoginItems,
    BackgroundTasks,
    CurrentUserCrontab,
    SystemExtensions,
    KernelExtensions,
    PeriodicScripts,
}

impl DiscoverySource {
    fn startup_dir(self) -> Option<&'static str> {
        match self {
            Self::UserLaunchAgents => Some("~/Library/LaunchAgents"),
            Self::LibraryLaunchAgents => Some("/Library/LaunchAgents"),
            Self::LibraryLaunchDaemons => Some("/Library/LaunchDaemons"),
            Self::UserLaunchDaemons => Some("~/Library/LaunchDaemons"),
            Self::EmondRules => Some("/etc/emond.d/rules"),
            Self::SystemLaunchAgents => Some("/System/Library/LaunchAgents"),
            Self::SystemLaunchDaemons => Some("/System/Library/LaunchDaemons"),
            _ => None,
        }
    }
}

#[derive(Clone, Copy, Debug, Default)]
struct AppOptions {
    include_system: bool,
    user_only: bool,
    skip_sudo_sources: bool,
    show_version: bool,
}

impl AppOptions {
    fn discovery_sources(self) -> Vec<DiscoverySource> {
        if self.user_only {
            return vec![
                DiscoverySource::UserLaunchAgents,
                DiscoverySource::CurrentUserCrontab,
            ];
        }

        if self.skip_sudo_sources {
            return vec![
                DiscoverySource::UserLaunchAgents,
                DiscoverySource::CurrentUserCrontab,
            ];
        }

        let mut sources = vec![
            DiscoverySource::LibraryLaunchAgents,
            DiscoverySource::LibraryLaunchDaemons,
            DiscoverySource::UserLaunchAgents,
            DiscoverySource::UserLaunchDaemons,
            DiscoverySource::EmondRules,
            DiscoverySource::LoginHooks,
            DiscoverySource::LoginItems,
            DiscoverySource::CurrentUserCrontab,
            DiscoverySource::SystemExtensions,
        ];

        if self.include_system {
            sources.extend([
                DiscoverySource::SystemLaunchAgents,
                DiscoverySource::SystemLaunchDaemons,
                DiscoverySource::BackgroundTasks,
                DiscoverySource::KernelExtensions,
                DiscoverySource::PeriodicScripts,
            ]);
        }

        sources
    }

    fn disabled_domains(self, uid: &str) -> Vec<String> {
        let mut domains = vec![format!("gui/{uid}")];
        if !self.user_only && !self.skip_sudo_sources {
            domains.push("system".to_string());
        }
        domains
    }

    fn permits_interactive_sudo(self) -> bool {
        self.include_system && !self.user_only && !self.skip_sudo_sources
    }

    fn permits_sudo_for_domain(self, domain: &str) -> bool {
        domain == "system" && self.permits_interactive_sudo()
    }
}

fn version_string() -> String {
    format!(
        "{} {} (tag: {}, branch: {}, commit: {})",
        env!("CARGO_PKG_NAME"),
        env!("CARGO_PKG_VERSION"),
        option_env!("LAUNCHCTRL_TUI_GIT_TAG").unwrap_or("unknown"),
        option_env!("LAUNCHCTRL_TUI_GIT_BRANCH").unwrap_or("unknown"),
        option_env!("LAUNCHCTRL_TUI_GIT_HASH").unwrap_or("unknown"),
    )
}

struct App {
    options: AppOptions,
    items: Vec<LaunchItem>,
    filtered: Vec<usize>,
    list_state: TableState,
    filter: String,
    status_filter: StatusFilter,
    type_filter: Option<String>,
    editing_filter: bool,
    message: String,
    last_refresh: Instant,
}

impl App {
    fn new(options: AppOptions) -> Result<Self> {
        let mut app = Self {
            options,
            items: Vec::new(),
            filtered: Vec::new(),
            list_state: TableState::default(),
            filter: String::new(),
            status_filter: StatusFilter::All,
            type_filter: None,
            editing_filter: false,
            message: String::new(),
            last_refresh: Instant::now(),
        };
        app.refresh()?;
        Ok(app)
    }

    fn run(mut self, mut terminal: Terminal<CrosstermBackend<io::Stdout>>) -> Result<()> {
        loop {
            terminal.draw(|frame| self.draw(frame))?;

            if event::poll(Duration::from_millis(250))? {
                let Event::Key(key) = event::read()? else {
                    continue;
                };
                if key.kind != KeyEventKind::Press {
                    continue;
                }

                if self.editing_filter {
                    match key.code {
                        KeyCode::Esc | KeyCode::Enter => self.editing_filter = false,
                        KeyCode::Backspace => {
                            self.filter.pop();
                            self.apply_filter();
                        }
                        KeyCode::Char(ch) => {
                            self.filter.push(ch);
                            self.apply_filter();
                        }
                        _ => {}
                    }
                    continue;
                }

                match key.code {
                    KeyCode::Char('q') => break,
                    KeyCode::Down | KeyCode::Char('j') => self.next(),
                    KeyCode::Up | KeyCode::Char('k') => self.previous(),
                    KeyCode::Char('/') => self.editing_filter = true,
                    KeyCode::Char('f') => {
                        self.status_filter = self.status_filter.next();
                        self.apply_filter();
                    }
                    KeyCode::Char('t') => {
                        self.cycle_type_filter();
                        self.apply_filter();
                    }
                    KeyCode::Esc => {
                        self.filter.clear();
                        self.status_filter = StatusFilter::All;
                        self.type_filter = None;
                        self.apply_filter();
                    }
                    KeyCode::Char('r') => self.refresh()?,
                    KeyCode::Char('b') => self.run_action(Action::Bootstrap)?,
                    KeyCode::Char('u') => self.run_action(Action::Bootout)?,
                    KeyCode::Char('s') => self.run_action(Action::Start)?,
                    KeyCode::Char('K') | KeyCode::Char('x') => self.run_action(Action::Kill)?,
                    _ => {}
                }
            }
        }
        Ok(())
    }

    fn refresh(&mut self) -> Result<()> {
        self.items = discover_launch_items(self.options)?;
        self.apply_filter();
        self.last_refresh = Instant::now();
        self.message = format!("Loaded {} startup items", self.items.len());
        Ok(())
    }

    fn apply_filter(&mut self) {
        let needle = self.filter.to_lowercase();
        self.filtered = self
            .items
            .iter()
            .enumerate()
            .filter(|(_, item)| {
                let text_matches = needle.is_empty()
                    || item.label.to_lowercase().contains(&needle)
                    || item.path.to_string_lossy().to_lowercase().contains(&needle)
                    || item.launch.to_lowercase().contains(&needle)
                    || item.type_label().contains(&needle);
                let type_matches = self
                    .type_filter
                    .as_ref()
                    .is_none_or(|type_filter| item.type_label() == *type_filter);
                text_matches && self.status_filter.matches(item) && type_matches
            })
            .map(|(index, _)| index)
            .collect();

        if self.filtered.is_empty() {
            self.list_state.select(None);
        } else {
            let selected = self
                .list_state
                .selected()
                .unwrap_or(0)
                .min(self.filtered.len() - 1);
            self.list_state.select(Some(selected));
        }
    }

    fn cycle_type_filter(&mut self) {
        let types: Vec<String> = self
            .items
            .iter()
            .map(LaunchItem::type_label)
            .collect::<BTreeSet<_>>()
            .into_iter()
            .collect();

        if types.is_empty() {
            self.type_filter = None;
            return;
        }

        self.type_filter = match self.type_filter.as_ref() {
            None => types.first().cloned(),
            Some(current) => types
                .iter()
                .position(|item_type| item_type == current)
                .and_then(|index| types.get(index + 1).cloned()),
        };
    }

    fn selected_item(&self) -> Option<&LaunchItem> {
        let selected = self.list_state.selected()?;
        self.filtered
            .get(selected)
            .and_then(|index| self.items.get(*index))
    }

    fn next(&mut self) {
        if self.filtered.is_empty() {
            return;
        }
        let selected = self.list_state.selected().unwrap_or(0);
        self.list_state
            .select(Some((selected + 1).min(self.filtered.len() - 1)));
    }

    fn previous(&mut self) {
        if self.filtered.is_empty() {
            return;
        }
        let selected = self.list_state.selected().unwrap_or(0);
        self.list_state.select(Some(selected.saturating_sub(1)));
    }

    fn run_action(&mut self, action: Action) -> Result<()> {
        let Some(item) = self.selected_item().cloned() else {
            return Ok(());
        };

        let result = action.run(&item, self.options);
        self.message = match result {
            Ok(output) => output,
            Err(error) => format!("{error:#}"),
        };
        self.refresh()?;
        Ok(())
    }

    fn draw(&mut self, frame: &mut Frame) {
        let chunks = Layout::default()
            .direction(Direction::Vertical)
            .constraints([
                Constraint::Length(3),
                Constraint::Min(8),
                Constraint::Length(11),
                Constraint::Length(3),
            ])
            .split(frame.area());

        self.draw_header(frame, chunks[0]);
        self.draw_list(frame, chunks[1]);
        self.draw_details(frame, chunks[2]);
        self.draw_footer(frame, chunks[3]);

        if self.editing_filter {
            let area = centered_rect(60, 3, frame.area());
            frame.render_widget(Clear, area);
            let input = Paragraph::new(self.filter.as_str())
                .block(Block::default().title("Filter").borders(Borders::ALL));
            frame.render_widget(input, area);
        }
    }

    fn draw_header(&self, frame: &mut Frame, area: Rect) {
        let title = if self.options.user_only {
            "launchctrl-tui — macOS startup items (user-only)"
        } else if self.options.skip_sudo_sources {
            "launchctrl-tui — macOS startup items (skip sudo sources)"
        } else if self.options.include_system {
            "launchctrl-tui — macOS startup items (including system-only sources)"
        } else {
            "launchctrl-tui — macOS startup items"
        };
        let header = Paragraph::new(Line::from(vec![
            Span::styled(title, Style::default().add_modifier(Modifier::BOLD)),
            Span::raw(format!(
                "  text: {}  status: {}  type: {}",
                empty_dash(&self.filter),
                self.status_filter.as_str(),
                self.type_filter.as_deref().unwrap_or("all")
            )),
        ]))
        .block(Block::default().borders(Borders::ALL));
        frame.render_widget(header, area);
    }

    fn draw_list(&mut self, frame: &mut Frame, area: Rect) {
        let rows: Vec<Row> = self
            .filtered
            .iter()
            .filter_map(|index| self.items.get(*index))
            .map(|item| {
                Row::new(vec![
                    Cell::from(status_span(item)),
                    Cell::from(item.type_label()),
                    Cell::from(item.user.clone()),
                    Cell::from(item.label.clone())
                        .style(Style::default().add_modifier(Modifier::BOLD)),
                    Cell::from(item.launch.clone()),
                ])
            })
            .collect();

        let header = Row::new(vec!["Status", "Type", "User", "Name", "Launch"])
            .style(
                Style::default()
                    .fg(Color::Yellow)
                    .add_modifier(Modifier::BOLD),
            )
            .bottom_margin(1);
        let table = Table::new(
            rows,
            [
                Constraint::Length(8),
                Constraint::Length(20),
                Constraint::Length(10),
                Constraint::Percentage(32),
                Constraint::Percentage(36),
            ],
        )
        .header(header)
        .block(Block::default().title("Items").borders(Borders::ALL))
        .highlight_symbol("▶")
        .row_highlight_style(Style::default().bg(Color::DarkGray));
        frame.render_stateful_widget(table, area, &mut self.list_state);
    }

    fn draw_details(&self, frame: &mut Frame, area: Rect) {
        let text = if let Some(item) = self.selected_item() {
            let mut lines = vec![
                Line::from(format!("Label : {}", item.label)),
                Line::from(format!("Type  : {}", item.type_label())),
                Line::from(format!("Domain: {}", empty_dash(&item.domain))),
                Line::from(format!("User  : {}", item.user)),
                Line::from(format!("Launch: {}", item.launch)),
                Line::from(format!("Path  : {}", item.path.display())),
                Line::from(format!("Program: {}", empty_dash(&item.program))),
            ];
            lines.extend(item.details.iter().map(|detail| Line::from(detail.clone())));
            lines
        } else {
            vec![Line::from("No launch item selected")]
        };

        let details = Paragraph::new(text)
            .wrap(Wrap { trim: true })
            .block(Block::default().title("Details").borders(Borders::ALL));
        frame.render_widget(details, area);
    }

    fn draw_footer(&self, frame: &mut Frame, area: Rect) {
        let keys = "↑/↓ move  / text  f status  t type  Esc clear  r refresh  b load  u unload  s start  x/K kill  q quit";
        let footer = Paragraph::new(vec![
            Line::from(keys),
            Line::from(format!(
                "{} · refreshed {:?} ago",
                self.message,
                self.last_refresh.elapsed().as_secs()
            )),
        ])
        .block(Block::default().borders(Borders::ALL));
        frame.render_widget(footer, area);
    }
}

#[derive(Clone, Copy)]
enum Action {
    Bootstrap,
    Bootout,
    Start,
    Kill,
}

impl Action {
    fn run(self, item: &LaunchItem, options: AppOptions) -> Result<String> {
        if item.kind == ItemKind::LoginItem && matches!(self, Self::Bootstrap | Self::Bootout) {
            let verb = match self {
                Self::Bootstrap => "enable",
                Self::Bootout => "disable",
                _ => unreachable!(),
            };
            let target = run_launchctl_enable_disable(verb, item, options)?;
            return Ok(format!("Ran launchctl {verb} {target}"));
        }

        if item.kind == ItemKind::BackgroundTask {
            return self.run_background_task_action(item, options);
        }

        if !item.kind.supports_launchctl_actions() {
            return Err(anyhow!(
                "{} is a {} item; launchctl load/unload/start/kill actions are not supported for this type",
                item.label,
                item.kind.as_str()
            ));
        }
        if item.core {
            return Err(anyhow!(
                "{} is under /System and is usually protected by SIP",
                item.label
            ));
        }
        if item.legacy_disabled {
            return Err(anyhow!(
                "{} uses legacy .disabled naming; rename it before loading",
                item.label
            ));
        }
        ensure_domain_action_privilege(&item.label, &item.domain, options)?;

        let (program, args): (&str, Vec<String>) = match self {
            Self::Bootstrap => (
                "launchctl",
                vec![
                    "bootstrap".into(),
                    item.domain.clone(),
                    item.path.display().to_string(),
                ],
            ),
            Self::Bootout => (
                "launchctl",
                vec![
                    "bootout".into(),
                    item.domain.clone(),
                    item.path.display().to_string(),
                ],
            ),
            Self::Start => (
                "launchctl",
                vec!["kickstart".into(), "-k".into(), item.target()],
            ),
            Self::Kill => (
                "launchctl",
                vec!["kill".into(), "TERM".into(), item.target()],
            ),
        };

        if program == "launchctl" {
            run_launchctl_command(&args, options.permits_sudo_for_domain(&item.domain))?;
        } else {
            run_command(program, &args)?;
        }
        Ok(format!("Ran launchctl {}", args.join(" ")))
    }

    fn run_background_task_action(self, item: &LaunchItem, options: AppOptions) -> Result<String> {
        if item
            .path
            .extension()
            .is_some_and(|extension| extension == "plist")
            && item.path.exists()
        {
            let uid = current_gui_uid()?;
            let kind = launch_kind(&item.path);
            let domain = launch_domain(kind, &uid);
            let label = label_from_path(&item.path).unwrap_or_else(|| item.label.clone());
            let target = format!("{domain}/{label}");
            if matches!(self, Self::Bootstrap | Self::Bootout) {
                let verb = match self {
                    Self::Bootstrap => "enable",
                    Self::Bootout => "disable",
                    _ => unreachable!(),
                };
                let mut candidates = launchctl_label_candidates(item);
                push_candidate(&mut candidates, &label);
                let target = run_launchctl_enable_disable_in_domain(
                    verb, item, &domain, candidates, options,
                )?;
                return Ok(format!("Ran launchctl {verb} {target}"));
            }
            let args = match self {
                Self::Start => vec!["kickstart".into(), "-k".into(), target],
                Self::Kill => vec!["kill".into(), "TERM".into(), target],
                Self::Bootstrap | Self::Bootout => unreachable!(),
            };
            if domain == "system" {
                ensure_domain_action_privilege(&item.label, &domain, options)?;
            }
            run_launchctl_command(&args, options.permits_sudo_for_domain(&domain))?;
            return Ok(format!("Ran launchctl {}", args.join(" ")));
        }

        match self {
            Self::Start
                if item
                    .path
                    .extension()
                    .is_some_and(|extension| extension == "app") =>
            {
                let args = vec![item.path.display().to_string()];
                run_command("open", &args)?;
                Ok(format!("Opened {}", item.path.display()))
            }
            Self::Kill => {
                if let Some(executable) = detail_value(item, "Executable") {
                    let args = vec!["-TERM".into(), "-f".into(), executable.to_string()];
                    run_command("pkill", &args)?;
                    Ok(format!("Sent TERM to processes matching {executable}"))
                } else if let Some(bundle) = detail_value(item, "Bundle ID") {
                    let script = format!("tell application id \"{bundle}\" to quit");
                    let args = vec!["-e".into(), script];
                    run_command("osascript", &args)?;
                    Ok(format!("Asked application {bundle} to quit"))
                } else {
                    Err(anyhow!(
                        "{} is a Background Task Management item without a launchd plist, executable path, or bundle id to target",
                        item.label
                    ))
                }
            }
            Self::Bootstrap | Self::Bootout => {
                let verb = match self {
                    Self::Bootstrap => "enable",
                    Self::Bootout => "disable",
                    _ => unreachable!(),
                };
                let target = run_launchctl_enable_disable(verb, item, options)?;
                Ok(format!("Ran launchctl {verb} {target}"))
            }
            Self::Start => Err(anyhow!(
                "{} is a Background Task Management item without an app path or launchd plist to start",
                item.label
            )),
        }
    }
}

fn run_launchctl_enable_disable(
    verb: &str,
    item: &LaunchItem,
    options: AppOptions,
) -> Result<String> {
    let uid = current_gui_uid()?;
    let domain = format!("gui/{uid}");
    run_launchctl_enable_disable_in_domain(
        verb,
        item,
        &domain,
        launchctl_label_candidates(item),
        options,
    )
}

fn run_launchctl_enable_disable_in_domain(
    verb: &str,
    item: &LaunchItem,
    domain: &str,
    mut candidates: Vec<String>,
    options: AppOptions,
) -> Result<String> {
    if domain == "system" {
        ensure_domain_action_privilege(&item.label, domain, options)?;
    }

    candidates.dedup();

    if candidates.is_empty() {
        return Err(anyhow!(
            "{} does not expose a launchctl label candidate to {verb}",
            item.label
        ));
    }

    let is_group = detail_value(item, "Embedded Identifiers").is_some();
    let mut successes = Vec::new();
    let mut failures = Vec::new();
    for candidate in candidates {
        let target = format!("{domain}/{candidate}");
        let args = vec![verb.to_string(), target.clone()];
        match run_launchctl_command(&args, options.permits_sudo_for_domain(domain)) {
            Ok(()) => {
                successes.push(target.clone());
                if !is_group {
                    return Ok(target);
                }
            }
            Err(error) => failures.push(format!("{target}: {error:#}")),
        }
    }

    if !successes.is_empty() {
        return Ok(successes.join(", "));
    }

    Err(anyhow!(
        "could not run launchctl {verb} for {}; tried:\n{}",
        item.label,
        failures.join("\n")
    ))
}

fn ensure_domain_action_privilege(label: &str, domain: &str, options: AppOptions) -> Result<()> {
    if domain != "system" || current_uid().is_ok_and(|uid| uid == "0") {
        return Ok(());
    }

    if options.permits_sudo_for_domain(domain) {
        return Ok(());
    }

    Err(anyhow!(
        "{} targets the system launchctl domain; run with --system or root privileges to control it",
        label
    ))
}

fn launchctl_label_candidates(item: &LaunchItem) -> Vec<String> {
    let mut candidates = Vec::new();
    push_candidate(&mut candidates, item.label.as_str());
    push_candidate(
        &mut candidates,
        detail_value(item, "Identifier").unwrap_or_default(),
    );
    push_candidate(
        &mut candidates,
        detail_value(item, "Bundle ID").unwrap_or_default(),
    );

    let path_label = label_from_path(&item.path).unwrap_or_default();
    push_candidate(&mut candidates, &path_label);

    push_candidate(
        &mut candidates,
        detail_value(item, "Parent").unwrap_or_default(),
    );

    expand_launchctl_candidates(candidates)
}

fn expand_launchctl_candidates(mut candidates: Vec<String>) -> Vec<String> {
    let initial = candidates.clone();
    for candidate in initial {
        if let Some(stripped) = strip_btm_identifier_prefix(&candidate) {
            push_candidate(&mut candidates, stripped);
        }
        if let Some(stripped) = candidate.strip_prefix("version.") {
            push_candidate(&mut candidates, stripped);
        }
        push_candidate(&mut candidates, &format!("version.{candidate}"));
    }
    candidates
}

fn push_candidate(candidates: &mut Vec<String>, candidate: &str) {
    let candidate = candidate.trim();
    if candidate.is_empty() || candidate == "n/a" || candidate == "Unknown Developer" {
        return;
    }
    let candidate = candidate.trim_start_matches("file://");
    if !candidates.iter().any(|existing| existing == candidate) {
        candidates.push(candidate.to_string());
    }
}

fn strip_btm_identifier_prefix(identifier: &str) -> Option<&str> {
    let (prefix, rest) = identifier.split_once('.')?;
    if prefix.chars().all(|ch| ch.is_ascii_digit()) && !rest.is_empty() {
        Some(rest)
    } else {
        None
    }
}

fn run_launchctl_command(args: &[String], allow_sudo: bool) -> Result<()> {
    match run_command("launchctl", args) {
        Ok(()) => Ok(()),
        Err(error) if allow_sudo && current_uid().is_ok_and(|uid| uid != "0") => {
            run_interactive_sudo_launchctl(args).map_err(|sudo_error| {
                anyhow!(
                    "launchctl {} failed: {error:#}\nsudo launchctl {} also failed: {sudo_error:#}",
                    args.join(" "),
                    args.join(" ")
                )
            })
        }
        Err(error) => Err(error),
    }
}

fn run_interactive_sudo_launchctl(args: &[String]) -> Result<()> {
    restore_terminal().context("failed to leave TUI before sudo")?;
    let status = Command::new("sudo")
        .arg("launchctl")
        .args(args)
        .status()
        .with_context(|| format!("failed to run sudo launchctl {}", args.join(" ")));
    let resume_result = enter_terminal_ui().context("failed to restore TUI after sudo");

    match (status, resume_result) {
        (Err(error), Ok(())) => Err(error),
        (Ok(_), Err(error)) => Err(error),
        (Err(command_error), Err(resume_error)) => Err(anyhow!(
            "{command_error:#}; additionally failed to restore TUI after sudo: {resume_error:#}"
        )),
        (Ok(status), Ok(())) if status.success() => Ok(()),
        (Ok(status), Ok(())) => Err(anyhow!(
            "sudo launchctl {} exited with {status}",
            args.join(" ")
        )),
    }
}

fn run_command(program: &str, args: &[String]) -> Result<()> {
    let output = Command::new(program)
        .args(args)
        .output()
        .with_context(|| format!("failed to run {program} {}", args.join(" ")))?;

    if !output.status.success() {
        let stderr = String::from_utf8_lossy(&output.stderr).trim().to_string();
        let stdout = String::from_utf8_lossy(&output.stdout).trim().to_string();
        let detail = if !stderr.is_empty() { stderr } else { stdout };
        return Err(anyhow!("{program} {} failed: {}", args.join(" "), detail));
    }

    Ok(())
}

fn btm_subcategory(category: &str) -> &'static str {
    match category {
        "startup/open-at-login app" => "open-at-login",
        "startup/login item" => "login",
        "launchd background service" => "launchd",
        "background helper" => "helper",
        "background item group" => "group",
        "background item" => "background",
        _ => "background",
    }
}

fn status_span(item: &LaunchItem) -> Span<'static> {
    if item.running {
        Span::styled(
            "RUN",
            Style::default()
                .fg(Color::Green)
                .add_modifier(Modifier::BOLD),
        )
    } else if item.loaded {
        Span::styled("LOAD", Style::default().fg(Color::Cyan))
    } else if item.disabled || item.legacy_disabled {
        Span::styled("DIS", Style::default().fg(Color::Yellow))
    } else if item.kind.supports_launchctl_actions() {
        Span::styled("OFF", Style::default().fg(Color::DarkGray))
    } else {
        Span::styled("INFO", Style::default().fg(Color::Magenta))
    }
}

fn detail_value<'a>(item: &'a LaunchItem, key: &str) -> Option<&'a str> {
    let prefix = format!("{key}: ");
    item.details
        .iter()
        .find_map(|detail| detail.strip_prefix(&prefix))
}

impl LaunchItem {
    fn target(&self) -> String {
        format!("{}/{}", self.domain, self.label)
    }

    fn type_label(&self) -> String {
        if self.kind != ItemKind::BackgroundTask {
            return self.kind.as_str().to_string();
        }

        let subcategory = self
            .details
            .iter()
            .find_map(|detail| detail.strip_prefix("Category: "))
            .map(btm_subcategory)
            .unwrap_or("background");
        format!("btm/{subcategory}")
    }

    fn needs_sudo_source(&self) -> bool {
        let path = self.path.to_string_lossy();
        self.user == "root"
            || path.starts_with("/Library/LaunchDaemons")
            || path.starts_with("/System/")
            || path.starts_with("/var/db/")
            || path.starts_with("/etc/")
    }
}

fn discover_launch_items(options: AppOptions) -> Result<Vec<LaunchItem>> {
    let uid = current_gui_uid()?;
    let sources = options.discovery_sources();
    let dirs: Vec<PathBuf> = sources
        .iter()
        .filter_map(|source| source.startup_dir())
        .map(|dir| expand_tilde(&dir))
        .collect();

    let disabled_by_domain = load_disabled_maps(&uid, options);
    let mut items = Vec::new();

    if sources.contains(&DiscoverySource::LoginHooks) {
        items.extend(discover_login_hooks());
        items.extend(discover_login_items());
    }
    if sources.contains(&DiscoverySource::BackgroundTasks) {
        items.extend(discover_background_tasks(
            options.user_only || options.skip_sudo_sources,
            &disabled_by_domain,
        ));
    }
    if sources.contains(&DiscoverySource::CurrentUserCrontab) {
        items.extend(discover_cron_jobs());
    }
    if sources.contains(&DiscoverySource::SystemExtensions) {
        items.extend(discover_system_extensions());
    }
    if sources.contains(&DiscoverySource::KernelExtensions) {
        items.extend(discover_kernel_extensions());
    }
    if sources.contains(&DiscoverySource::PeriodicScripts) {
        items.extend(discover_periodic_scripts());
    }

    for dir in dirs {
        if !dir.exists() {
            continue;
        }
        for entry in WalkDir::new(&dir)
            .follow_links(false)
            .into_iter()
            .filter_map(Result::ok)
            .filter(|entry| entry.file_type().is_file())
        {
            let path = entry.path();
            if !is_plistish(path) {
                continue;
            }
            match parse_launch_item(path, &uid, &disabled_by_domain) {
                Ok(item) => items.push(item),
                Err(error) => items.push(fallback_item(path, &uid, error.to_string())),
            }
        }
    }

    items.sort_by(|left, right| {
        left.type_label()
            .cmp(&right.type_label())
            .then_with(|| left.label.cmp(&right.label))
    });
    Ok(items)
}

fn parse_launch_item(
    path: &Path,
    uid: &str,
    disabled_by_domain: &BTreeMap<String, Vec<String>>,
) -> Result<LaunchItem> {
    let plist_path = if path.extension().is_some_and(|ext| ext == "disabled") {
        path.with_extension("")
    } else {
        path.to_path_buf()
    };
    let legacy_disabled = path.extension().is_some_and(|ext| ext == "disabled");
    let value =
        Value::from_file(path).with_context(|| format!("failed to parse {}", path.display()))?;
    let dict = value
        .as_dictionary()
        .ok_or_else(|| anyhow!("plist root is not a dictionary"))?;

    let label = dict
        .get("Label")
        .and_then(Value::as_string)
        .map(str::to_string)
        .or_else(|| label_from_path(path))
        .ok_or_else(|| anyhow!("missing Label"))?;

    let kind = launch_kind(path);
    let domain = launch_domain(kind, uid);
    let disabled = legacy_disabled
        || disabled_by_domain
            .get(&domain)
            .is_some_and(|labels| labels.iter().any(|entry| entry == &label))
        || dict
            .get("Disabled")
            .and_then(Value::as_boolean)
            .unwrap_or(false)
        || dict.get("Enabled").and_then(Value::as_boolean) == Some(false);
    let print = launchctl_print(&domain, &label);
    let loaded = print.as_ref().is_ok_and(|output| output.status.success());
    let running = print
        .as_ref()
        .ok()
        .map(|output| {
            let text = String::from_utf8_lossy(&output.stdout).to_lowercase();
            text.contains("state = running") || text.contains("\npid = ")
        })
        .unwrap_or(false);

    Ok(LaunchItem {
        label,
        path: if legacy_disabled {
            plist_path
        } else {
            path.to_path_buf()
        },
        domain,
        kind,
        user: script_user(path, dict),
        launch: launch_summary(dict, disabled, legacy_disabled),
        program: program_summary(dict),
        details: Vec::new(),
        loaded,
        running,
        disabled,
        core: path.starts_with("/System"),
        legacy_disabled,
    })
}

fn fallback_item(path: &Path, uid: &str, error: String) -> LaunchItem {
    let kind = launch_kind(path);
    LaunchItem {
        label: label_from_path(path).unwrap_or_else(|| path.display().to_string()),
        path: path.to_path_buf(),
        domain: launch_domain(kind, uid),
        kind,
        user: "unknown".into(),
        launch: format!("unparseable: {error}"),
        program: String::new(),
        details: Vec::new(),
        loaded: false,
        running: false,
        disabled: false,
        core: path.starts_with("/System"),
        legacy_disabled: path.extension().is_some_and(|ext| ext == "disabled"),
    }
}

fn info_item(
    label: impl Into<String>,
    kind: ItemKind,
    user: impl Into<String>,
    launch: impl Into<String>,
    path: impl Into<PathBuf>,
    program: impl Into<String>,
    loaded: bool,
    running: bool,
    disabled: bool,
) -> LaunchItem {
    info_item_with_details(
        label,
        kind,
        user,
        launch,
        path,
        program,
        Vec::new(),
        loaded,
        running,
        disabled,
    )
}

fn info_item_with_details(
    label: impl Into<String>,
    kind: ItemKind,
    user: impl Into<String>,
    launch: impl Into<String>,
    path: impl Into<PathBuf>,
    program: impl Into<String>,
    details: Vec<String>,
    loaded: bool,
    running: bool,
    disabled: bool,
) -> LaunchItem {
    LaunchItem {
        label: label.into(),
        path: path.into(),
        domain: String::new(),
        kind,
        user: user.into(),
        launch: launch.into(),
        program: program.into(),
        details,
        loaded,
        running,
        disabled,
        core: false,
        legacy_disabled: false,
    }
}

fn discover_login_hooks() -> Vec<LaunchItem> {
    let mut items = Vec::new();
    for (key, kind) in [
        ("LoginHook", ItemKind::LoginHook),
        ("LogoutHook", ItemKind::LogoutHook),
    ] {
        let output = Command::new("defaults")
            .args(["read", "com.apple.loginwindow", key])
            .output();
        let Ok(output) = output else {
            continue;
        };
        if !output.status.success() {
            continue;
        }
        let hook = String::from_utf8_lossy(&output.stdout).trim().to_string();
        if hook.is_empty() {
            continue;
        }
        items.push(info_item(
            hook.clone(),
            kind,
            "root",
            key,
            "/var/root/Library/Preferences/com.apple.loginwindow",
            hook,
            true,
            false,
            false,
        ));
    }
    items
}

fn discover_background_tasks(
    skip_sudo_sources: bool,
    disabled_by_domain: &BTreeMap<String, Vec<String>>,
) -> Vec<LaunchItem> {
    let output = Command::new("sfltool").arg("dumpbtm").output();
    let Ok(output) = output else {
        return Vec::new();
    };
    if !output.status.success() {
        return Vec::new();
    }

    let text = String::from_utf8_lossy(&output.stdout);
    let user = env::var("USER").unwrap_or_else(|_| "user".into());
    let mut items = Vec::new();
    let mut record = BtmRecord::default();
    let mut in_record = false;

    for line in text.lines() {
        let trimmed = line.trim();
        if trimmed.starts_with('#') && trimmed.ends_with(':') {
            if in_record {
                if let Some(item) = record.to_item(&user, disabled_by_domain) {
                    if !skip_sudo_sources || !item.needs_sudo_source() {
                        items.push(item);
                    }
                }
                record = BtmRecord::default();
            }
            in_record = true;
            continue;
        }

        if !in_record {
            continue;
        }

        if let Some((key, value)) = trimmed.split_once(':') {
            let value = clean_btm_value(value);
            let key = key.trim();
            match key {
                "Name" => record.name = value,
                "Developer Name" => record.developer = value,
                "Team Identifier" => record.team = value,
                "Type" => record.kind = value,
                "Disposition" => record.disposition = value,
                "Identifier" => record.identifier = value,
                "URL" => record.url = value,
                "Executable Path" => record.executable = value,
                "Bundle Identifier" => record.bundle = value,
                "Parent Identifier" => record.parent = value,
                _ if key.starts_with('#') => {
                    if let Some(value) = value {
                        record.embedded_identifiers.push(value);
                    }
                }
                _ => {}
            }
        }
    }

    if in_record {
        if let Some(item) = record.to_item(&user, disabled_by_domain) {
            if !skip_sudo_sources || !item.needs_sudo_source() {
                items.push(item);
            }
        }
    }

    items
}

#[derive(Default)]
struct BtmRecord {
    name: Option<String>,
    developer: Option<String>,
    team: Option<String>,
    kind: Option<String>,
    disposition: Option<String>,
    identifier: Option<String>,
    url: Option<String>,
    executable: Option<String>,
    bundle: Option<String>,
    parent: Option<String>,
    embedded_identifiers: Vec<String>,
}

impl BtmRecord {
    fn launchctl_label_candidates(&self, label: &str, path: &Path) -> Vec<String> {
        let mut candidates = Vec::new();
        push_candidate(&mut candidates, label);
        push_candidate(
            &mut candidates,
            self.identifier.as_deref().unwrap_or_default(),
        );
        push_candidate(&mut candidates, self.bundle.as_deref().unwrap_or_default());
        push_candidate(&mut candidates, self.parent.as_deref().unwrap_or_default());
        for embedded_identifier in &self.embedded_identifiers {
            push_candidate(&mut candidates, embedded_identifier);
        }
        let path_label = label_from_path(path).unwrap_or_default();
        push_candidate(&mut candidates, &path_label);
        expand_launchctl_candidates(candidates)
    }

    fn category(&self) -> &'static str {
        let kind = self.kind.as_deref().unwrap_or_default().to_lowercase();
        let url = self.url.as_deref().unwrap_or_default().to_lowercase();

        if kind.contains("app") && url.ends_with(".app/") {
            "startup/open-at-login app"
        } else if kind.contains("login") {
            "startup/login item"
        } else if kind.contains("legacy daemon")
            || kind.contains("legacy agent")
            || url.ends_with(".plist")
        {
            "launchd background service"
        } else if kind.contains("developer") {
            "background item group"
        } else if kind.contains("daemon") || kind.contains("agent") || kind.contains("extension") {
            "background helper"
        } else {
            "background item"
        }
    }

    fn to_item(
        &self,
        user: &str,
        disabled_by_domain: &BTreeMap<String, Vec<String>>,
    ) -> Option<LaunchItem> {
        let identifier = self.identifier.as_deref().filter(|value| !value.is_empty());
        let name = self.name.as_deref().filter(|value| !value.is_empty());
        let label = name.or(identifier)?.to_string();
        let disposition = self.disposition.clone().unwrap_or_else(|| "unknown".into());
        let disposition_flags = btm_disposition_flags(&disposition);
        let category = self.category();
        let path = self
            .url
            .as_deref()
            .filter(|value| !value.is_empty())
            .map(url_to_path)
            .unwrap_or_else(|| PathBuf::from("n/a"));
        let domain = btm_launchctl_domain(&path);
        let disabled_services = disabled_by_domain
            .get(&domain)
            .map(Vec::as_slice)
            .unwrap_or(&[]);
        let disabled_by_launchctl = self
            .launchctl_label_candidates(&label, &path)
            .iter()
            .any(|candidate| disabled_services.iter().any(|service| service == candidate));
        let btm_enabled = disposition_flags.iter().any(|flag| flag == "enabled");
        let btm_disabled = disposition_flags.iter().any(|flag| flag == "disabled");
        let loaded = btm_enabled && !disabled_by_launchctl;
        let disabled = btm_disabled || disabled_by_launchctl;
        let mut details = Vec::new();
        push_detail(&mut details, "Category", Some(category));
        push_detail(
            &mut details,
            "Startup/Login Toggle",
            Some(if btm_enabled {
                "enabled"
            } else if btm_disabled {
                "disabled"
            } else {
                "unknown"
            }),
        );
        push_detail(
            &mut details,
            "Background Permission",
            Some(if disposition_flags.iter().any(|flag| flag == "allowed") {
                "allowed"
            } else if disposition_flags.iter().any(|flag| flag == "disallowed") {
                "disallowed"
            } else {
                "unknown"
            }),
        );
        push_detail(
            &mut details,
            "User Notification",
            Some(if disposition_flags.iter().any(|flag| flag == "notified") {
                "notified"
            } else if disposition_flags.iter().any(|flag| flag == "not notified") {
                "not notified"
            } else {
                "unknown"
            }),
        );
        if disabled_by_launchctl {
            push_detail(&mut details, "Launchctl Override", Some("disabled"));
            push_detail(&mut details, "Launchctl Domain", Some(domain.as_str()));
        }
        push_detail(&mut details, "Raw Disposition", Some(disposition.as_str()));
        push_detail(&mut details, "BTM Type", self.kind.as_deref());
        push_detail(&mut details, "Identifier", identifier);
        push_detail(&mut details, "Bundle ID", self.bundle.as_deref());
        push_detail(&mut details, "Developer", self.developer.as_deref());
        push_detail(&mut details, "Team ID", self.team.as_deref());
        push_detail(&mut details, "Parent", self.parent.as_deref());
        if !self.embedded_identifiers.is_empty() {
            details.push(format!(
                "Embedded Identifiers: {}",
                self.embedded_identifiers.join(", ")
            ));
        }
        push_detail(&mut details, "Executable", self.executable.as_deref());

        let program = self
            .executable
            .clone()
            .or_else(|| self.bundle.clone())
            .or_else(|| self.identifier.clone())
            .unwrap_or_default();

        Some(info_item_with_details(
            label,
            ItemKind::BackgroundTask,
            user.to_string(),
            category,
            path,
            program,
            details,
            loaded,
            false,
            disabled,
        ))
    }
}

fn btm_launchctl_domain(path: &Path) -> String {
    let path = path.to_string_lossy();
    if path.contains("/LaunchDaemons/") {
        "system".to_string()
    } else {
        current_gui_uid()
            .map(|uid| format!("gui/{uid}"))
            .unwrap_or_else(|_| "gui/0".to_string())
    }
}

fn btm_disposition_flags(disposition: &str) -> Vec<String> {
    disposition
        .split_once('[')
        .and_then(|(_, rest)| rest.split_once(']'))
        .map(|(flags, _)| {
            flags
                .split(',')
                .map(str::trim)
                .filter(|flag| !flag.is_empty())
                .map(str::to_string)
                .collect()
        })
        .unwrap_or_else(|| vec![disposition.to_string()])
}

fn clean_btm_value(value: &str) -> Option<String> {
    let value = value.trim();
    if value.is_empty() || value == "(null)" {
        None
    } else {
        Some(value.to_string())
    }
}

fn push_detail(details: &mut Vec<String>, key: &str, value: Option<&str>) {
    if let Some(value) = value.filter(|value| !value.is_empty()) {
        details.push(format!("{key}: {value}"));
    }
}

fn url_to_path(value: &str) -> PathBuf {
    let path = value.strip_prefix("file://").unwrap_or(value);
    PathBuf::from(percent_decode(path))
}

fn percent_decode(value: &str) -> String {
    let bytes = value.as_bytes();
    let mut output = Vec::with_capacity(bytes.len());
    let mut index = 0;
    while index < bytes.len() {
        if bytes[index] == b'%' && index + 2 < bytes.len() {
            if let Ok(hex) = std::str::from_utf8(&bytes[index + 1..index + 3]) {
                if let Ok(byte) = u8::from_str_radix(hex, 16) {
                    output.push(byte);
                    index += 3;
                    continue;
                }
            }
        }
        output.push(bytes[index]);
        index += 1;
    }
    String::from_utf8_lossy(&output).into_owned()
}

fn discover_cron_jobs() -> Vec<LaunchItem> {
    let output = Command::new("crontab").arg("-l").output();
    let Ok(output) = output else {
        return Vec::new();
    };
    if !output.status.success() {
        return Vec::new();
    }

    let user = env::var("USER").unwrap_or_else(|_| "user".into());
    String::from_utf8_lossy(&output.stdout)
        .lines()
        .map(str::trim)
        .filter(|line| !line.is_empty() && !line.starts_with('#'))
        .filter_map(|line| {
            let fields: Vec<&str> = line.split_whitespace().collect();
            let command = if fields.first().is_some_and(|field| field.starts_with('@')) {
                fields.get(1..)?.join(" ")
            } else {
                fields.get(5..)?.join(" ")
            };
            if command.is_empty() {
                return None;
            }
            Some(info_item(
                command.clone(),
                ItemKind::CronJob,
                user.clone(),
                "enabled",
                command.clone(),
                command,
                true,
                false,
                false,
            ))
        })
        .collect()
}

fn discover_login_items() -> Vec<LaunchItem> {
    let disabled = disabled_login_items();
    let user = env::var("USER").unwrap_or_else(|_| "user".into());
    let mut items = Vec::new();
    let dir = Path::new("/var/db/com.apple.xpc.launchd");
    if !dir.exists() {
        return items;
    }

    for entry in WalkDir::new(dir)
        .max_depth(1)
        .into_iter()
        .filter_map(Result::ok)
        .filter(|entry| entry.file_type().is_file())
    {
        let path = entry.path();
        let Some(name) = path.file_name().and_then(|name| name.to_str()) else {
            continue;
        };
        if !name.starts_with("loginitems.") || !name.ends_with(".plist") {
            continue;
        }
        let Ok(value) = Value::from_file(path) else {
            continue;
        };
        collect_login_item_keys(&value, path, &user, &disabled, &mut items);
    }
    items
}

fn disabled_login_items() -> Vec<String> {
    let mut labels = Vec::new();
    let dir = Path::new("/var/db/com.apple.xpc.launchd");
    let Ok(entries) = fs::read_dir(dir) else {
        return labels;
    };
    for entry in entries.filter_map(Result::ok) {
        let path = entry.path();
        let Some(name) = path.file_name().and_then(|name| name.to_str()) else {
            continue;
        };
        if !name.starts_with("disabled.") {
            continue;
        }
        let Ok(value) = Value::from_file(&path) else {
            continue;
        };
        if let Some(dict) = value.as_dictionary() {
            for (key, value) in dict {
                if value.as_boolean() == Some(true) {
                    labels.push(key.clone());
                }
            }
        }
    }
    labels
}

fn collect_login_item_keys(
    value: &Value,
    path: &Path,
    user: &str,
    disabled: &[String],
    items: &mut Vec<LaunchItem>,
) {
    match value {
        Value::Dictionary(dict) => {
            for (key, value) in dict {
                if !matches!(value, Value::Dictionary(_) | Value::Array(_)) && !key.starts_with('$')
                {
                    let is_disabled = disabled.iter().any(|entry| entry == key);
                    items.push(info_item(
                        key.clone(),
                        ItemKind::LoginItem,
                        user.to_string(),
                        if is_disabled { "disabled" } else { "LoginItem" },
                        path.to_path_buf(),
                        key.clone(),
                        !is_disabled,
                        false,
                        is_disabled,
                    ));
                }
                collect_login_item_keys(value, path, user, disabled, items);
            }
        }
        Value::Array(values) => {
            for value in values {
                collect_login_item_keys(value, path, user, disabled, items);
            }
        }
        _ => {}
    }
}

fn discover_kernel_extensions() -> Vec<LaunchItem> {
    let output = Command::new("kmutil")
        .args([
            "showloaded",
            "--no-kernel-components",
            "--list-only",
            "--sort",
            "--show",
            "loaded",
        ])
        .output();
    let Ok(output) = output else {
        return Vec::new();
    };
    if !output.status.success() {
        return Vec::new();
    }

    String::from_utf8_lossy(&output.stdout)
        .lines()
        .filter(|line| !line.contains("com.apple."))
        .filter_map(|line| {
            let fields: Vec<&str> = line.split_whitespace().collect();
            let loaded_field = fields.get(2).copied().unwrap_or("0");
            let label = fields.get(6).copied().or_else(|| fields.last().copied())?;
            let loaded = loaded_field != "0";
            Some(info_item(
                label.to_string(),
                ItemKind::KernelExtension,
                "root",
                if loaded { "Always" } else { "disabled" },
                "n/a",
                line.to_string(),
                loaded,
                loaded,
                !loaded,
            ))
        })
        .collect()
}

fn discover_system_extensions() -> Vec<LaunchItem> {
    let output = Command::new("systemextensionsctl").arg("list").output();
    let Ok(output) = output else {
        return Vec::new();
    };
    if !output.status.success() {
        return Vec::new();
    }

    String::from_utf8_lossy(&output.stdout)
        .lines()
        .map(str::trim)
        .filter(|line| !line.is_empty() && !line.starts_with("---") && !line.starts_with("enabled"))
        .filter_map(|line| {
            let fields: Vec<&str> = line.split_whitespace().collect();
            let label = fields
                .iter()
                .find(|field| field.contains('.') && !field.starts_with('('))
                .copied()
                .or_else(|| fields.last().copied())?;
            let enabled = line.contains('*') || line.contains("enabled");
            Some(info_item(
                label.to_string(),
                ItemKind::SystemExtension,
                env::var("USER").unwrap_or_else(|_| "user".into()),
                if enabled { "enabled" } else { "disabled" },
                "n/a",
                line.to_string(),
                enabled,
                false,
                !enabled,
            ))
        })
        .collect()
}

fn discover_periodic_scripts() -> Vec<LaunchItem> {
    let mut items = Vec::new();
    let root = Path::new("/etc/periodic");
    if !root.exists() {
        return items;
    }
    for entry in WalkDir::new(root)
        .into_iter()
        .filter_map(Result::ok)
        .filter(|entry| entry.file_type().is_file())
    {
        let path = entry.path();
        let mode = if path.to_string_lossy().contains("/weekly/") {
            "weekly"
        } else if path.to_string_lossy().contains("/monthly/") {
            "monthly"
        } else {
            "daily"
        };
        items.push(info_item(
            path.display().to_string(),
            ItemKind::Periodic,
            env::var("USER").unwrap_or_else(|_| "user".into()),
            mode,
            path.to_path_buf(),
            path.display().to_string(),
            true,
            false,
            false,
        ));
    }
    items
}

fn load_disabled_maps(uid: &str, options: AppOptions) -> BTreeMap<String, Vec<String>> {
    let mut maps = BTreeMap::new();
    for domain in options.disabled_domains(uid) {
        let output = Command::new("launchctl")
            .args(["print-disabled", &domain])
            .output();
        let Ok(output) = output else {
            continue;
        };
        let text = String::from_utf8_lossy(&output.stdout);
        let labels = text
            .lines()
            .filter_map(parse_disabled_service_line)
            .collect();
        maps.insert(domain, labels);
    }
    maps
}

fn parse_disabled_service_line(line: &str) -> Option<String> {
    let (label, state) = line.split_once("=>")?;
    let state = state.trim().trim_end_matches(',').to_lowercase();
    if matches!(state.as_str(), "true" | "disabled") {
        Some(label.trim().trim_matches('"').to_string())
    } else {
        None
    }
}

fn launchctl_print(domain: &str, label: &str) -> io::Result<std::process::Output> {
    Command::new("launchctl")
        .args(["print", &format!("{domain}/{label}")])
        .output()
}

fn launch_summary(dict: &plist::Dictionary, disabled: bool, legacy_disabled: bool) -> String {
    if legacy_disabled {
        return "disabled (legacy)".into();
    }
    if disabled {
        return "disabled".into();
    }

    let mut triggers = Vec::new();
    if dict.contains_key("OnDemand") {
        triggers.push("OnDemand");
    }
    if dict
        .get("RunAtLoad")
        .and_then(Value::as_boolean)
        .unwrap_or(false)
    {
        triggers.push("OnStartup");
    }
    if dict.contains_key("KeepAlive") {
        triggers.push("Always");
    }
    if dict
        .get("StartOnMount")
        .and_then(Value::as_boolean)
        .unwrap_or(false)
    {
        triggers.push("OnFilesystemMount");
    }
    if dict.contains_key("StartInterval") || dict.contains_key("StartCalendarInterval") {
        triggers.push("Periodically");
    }
    if dict.contains_key("MachServices") {
        triggers.push("MachService");
    }
    if dict.contains_key("WatchPaths") || dict.contains_key("QueueDirectories") {
        triggers.push("WatchPaths");
    }

    if triggers.is_empty() {
        "Unknown".into()
    } else {
        triggers.join(",")
    }
}

fn program_summary(dict: &plist::Dictionary) -> String {
    if let Some(program) = dict.get("Program").and_then(Value::as_string) {
        return program.to_string();
    }
    dict.get("ProgramArguments")
        .and_then(Value::as_array)
        .and_then(|args| args.first())
        .and_then(Value::as_string)
        .unwrap_or("")
        .to_string()
}

fn script_user(path: &Path, dict: &plist::Dictionary) -> String {
    if path.to_string_lossy().contains("LaunchAgents") {
        return env::var("USER").unwrap_or_else(|_| "user".into());
    }
    dict.get("UserName")
        .and_then(Value::as_string)
        .unwrap_or("root")
        .to_string()
}

fn launch_kind(path: &Path) -> ItemKind {
    let text = path.to_string_lossy();
    if text.contains("LaunchAgents") {
        ItemKind::Agent
    } else if text.contains("LaunchDaemons") {
        ItemKind::Daemon
    } else {
        ItemKind::EmondRule
    }
}

fn launch_domain(kind: ItemKind, uid: &str) -> String {
    match kind {
        ItemKind::Agent => format!("gui/{uid}"),
        ItemKind::Daemon | ItemKind::EmondRule => "system".into(),
        _ => String::new(),
    }
}

fn label_from_path(path: &Path) -> Option<String> {
    let file_name = path.file_name()?.to_string_lossy();
    Some(
        file_name
            .trim_end_matches(".disabled")
            .trim_end_matches(".plist")
            .to_string(),
    )
}

fn is_plistish(path: &Path) -> bool {
    let name = path
        .file_name()
        .and_then(|name| name.to_str())
        .unwrap_or("");
    name.ends_with(".plist") || name.ends_with(".plist.disabled")
}

fn current_gui_uid() -> Result<String> {
    env::var("SUDO_UID")
        .ok()
        .filter(|uid| !uid.is_empty())
        .map(Ok)
        .unwrap_or_else(current_uid)
}

fn current_uid() -> Result<String> {
    let output = Command::new("id").arg("-u").output()?;
    if !output.status.success() {
        return Err(anyhow!("id -u failed"));
    }
    Ok(String::from_utf8_lossy(&output.stdout).trim().to_string())
}

fn expand_tilde(path: &&str) -> PathBuf {
    if let Some(rest) = path.strip_prefix("~/") {
        if let Some(home) = env::var_os("HOME") {
            return PathBuf::from(home).join(rest);
        }
    }
    PathBuf::from(path)
}

fn empty_dash(value: &str) -> &str {
    if value.is_empty() { "-" } else { value }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn parse(args: &[&str]) -> Result<AppOptions> {
        parse_options(args.iter().map(|arg| arg.to_string()))
    }

    #[test]
    fn parses_user_mode() {
        let options = parse(&["--user"]).unwrap();

        assert!(options.user_only);
        assert!(!options.include_system);
    }

    #[test]
    fn rejects_user_and_system_together() {
        let error = parse(&["--user", "--system"]).unwrap_err().to_string();

        assert!(error.contains("cannot be used together"));
    }

    #[test]
    fn user_source_policy_is_user_safe_only() {
        let options = parse(&["--user"]).unwrap();

        assert_eq!(
            options.discovery_sources(),
            vec![
                DiscoverySource::UserLaunchAgents,
                DiscoverySource::CurrentUserCrontab,
            ]
        );
    }

    #[test]
    fn skip_sudo_source_policy_avoids_prompting_background_tasks() {
        let options = parse(&["--skip-sudo"]).unwrap();
        assert!(
            !options
                .discovery_sources()
                .contains(&DiscoverySource::BackgroundTasks)
        );
    }

    #[test]
    fn default_source_policy_avoids_prompting_background_tasks() {
        let options = parse(&[]).unwrap();
        assert!(
            !options
                .discovery_sources()
                .contains(&DiscoverySource::BackgroundTasks)
        );
    }

    #[test]
    fn system_source_policy_includes_background_tasks() {
        let options = parse(&["--system"]).unwrap();
        assert!(
            options
                .discovery_sources()
                .contains(&DiscoverySource::BackgroundTasks)
        );
    }

    #[test]
    fn parses_version_mode() {
        let options = parse(&["--version"]).unwrap();
        assert!(options.show_version);
    }

    #[test]
    fn version_output_includes_build_metadata_labels() {
        let version = version_string();
        assert!(version.contains(env!("CARGO_PKG_VERSION")));
        assert!(version.contains("tag: "));
        assert!(version.contains("branch: "));
        assert!(version.contains("commit: "));
    }

    #[test]
    fn skip_sudo_queries_only_user_disabled_map() {
        let options = parse(&["--skip-sudo"]).unwrap();

        assert_eq!(options.disabled_domains("501"), vec!["gui/501".to_string()]);
    }

    #[test]
    fn only_system_mode_permits_interactive_sudo() {
        assert!(!parse(&[]).unwrap().permits_interactive_sudo());
        assert!(!parse(&["--user"]).unwrap().permits_interactive_sudo());
        assert!(!parse(&["--skip-sudo"]).unwrap().permits_interactive_sudo());
        assert!(parse(&["--system"]).unwrap().permits_interactive_sudo());
    }
}

fn centered_rect(width_percent: u16, height: u16, area: Rect) -> Rect {
    let vertical = Layout::default()
        .direction(Direction::Vertical)
        .constraints([
            Constraint::Percentage((100 - height.min(100)) / 2),
            Constraint::Length(height),
            Constraint::Min(0),
        ])
        .split(area);
    Layout::default()
        .direction(Direction::Horizontal)
        .constraints([
            Constraint::Percentage((100 - width_percent) / 2),
            Constraint::Percentage(width_percent),
            Constraint::Min(0),
        ])
        .split(vertical[1])[1]
}
