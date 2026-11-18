//! `crafthub`: the command-line tool, the shared server, and the bridge AI clients launch.

use clap::{Parser, Subcommand};
use crafthub::clients::Snapshot;
use crafthub::config::Config;
use crafthub::control::{self, Probe};
use crafthub::hub::{self, AddOptions, App, Hub, Origin, StateKind};
use crafthub::server::{self, ServerSpec};
use crafthub::{catalog, clients, doctor, guide, logger, macapp, paths, shim, uninstall, update_check};
use serde_json::json;
use std::process::ExitCode;

#[derive(Parser)]
#[command(
    name = "crafthub",
    version,
    about = "Unofficial MCP hub for the ArtCraft app suite: one shared, windowless MCP server per app, for every AI client session."
)]
struct Cli {
    #[command(subcommand)]
    command: Command,
}

#[derive(Subcommand)]
enum Command {
    /// Show every app: installed or not, added or not, and what its server is doing
    #[command(alias = "status")]
    List {
        /// Only this app
        app: Option<String>,
        /// Print JSON
        #[arg(long)]
        json: bool,
    },
    /// Add and turn on every installed app that isn't in the hub yet
    Setup,
    /// Add one app: a newly installed one, or a custom build or fork with --cli
    Add {
        /// The app's id, e.g. `photocraft`; any name you like for a custom app
        id: String,
        /// The app's command-line tool, for a custom build or fork
        #[arg(long)]
        cli: Option<String>,
        /// Display name
        #[arg(long)]
        name: Option<String>,
        /// Start it the way this known app is started, instead of working it out from its --help
        #[arg(long)]
        like: Option<String>,
        /// Port for its server (default: the app's usual port, or the next free one)
        #[arg(long)]
        port: Option<u16>,
        /// The app's settings folder, where its recent libraries are listed (repeatable)
        #[arg(long = "settings")]
        settings: Vec<String>,
        /// Only add it, don't turn it on
        #[arg(long)]
        off: bool,
    },
    /// Turn an app off and forget its settings
    Remove { id: String },
    /// Turn an app on: register it with the AI client and start its server (`all` for every added app)
    On { app: String },
    /// Turn an app off: remove it from the AI client and stop its server (`all` for every added app)
    Off { app: String },
    /// Move an app's server to another port
    Port { id: String, port: u16 },
    /// Show or set the folders an app's engine may read and write
    Root {
        id: String,
        /// The folders (several for the engines that take more than one)
        dirs: Vec<String>,
        /// Lift the restriction: the engine may use any file you can
        #[arg(long)]
        anywhere: bool,
    },
    /// Test an app's server with a full MCP round trip (`all`, the default, for every app that is on)
    Check { app: Option<String> },
    /// Check everything between the AI client and an app's engine, and say what to do
    Doctor { id: String },
    /// Restart an app's server (`all` for every server that is running, as after an update)
    Restart { id: String },
    /// Close the library an app has open, so the desktop app can open it
    CloseLibrary { id: String },
    /// Show where the app catalog comes from, or fetch the current one
    Catalog {
        /// Download the current catalog from getartcraft.com
        #[arg(long)]
        update: bool,
    },
    /// Show the AI clients the hub can register apps with, or enable or disable one
    Clients {
        #[command(subcommand)]
        action: Option<ClientAction>,
        /// Print JSON
        #[arg(long)]
        json: bool,
    },
    /// For AI agents: how to set the hub up for yourself (ask your AI to run this)
    Guide,
    /// Print the MCP servers to paste into a client's settings by hand
    Snippet {
        /// One app (default: every app that is on)
        app: Option<String>,
        /// `json` for an mcpServers object, `toml` for [mcp_servers.<app>] tables
        #[arg(long, default_value = "json")]
        format: String,
    },
    /// Open the control panel window
    Panel,
    /// Show or switch how AI clients connect: `direct` (stdio directly to <app>-cli, no daemon) or `hub` (shared background server)
    Mode {
        /// `direct` or `hub`
        mode: Option<String>,
    },
    /// Fetch the command-line tools of the apps installed without one (macOS: the ArtCraft launcher
    /// installs only the desktop apps), and update the ones fetched for an older version
    Tools {
        /// Only this app
        app: Option<String>,
    },
    /// Check GitHub for a newer version of Craft MCP Hub
    UpdateCheck,
    /// Take the hub off this computer: turn every app off, take it out of the AI clients, and delete
    /// the app, the programs and ~/.crafthub (the ArtCraft apps stay)
    Uninstall {
        /// Don't ask first
        #[arg(long, short)]
        yes: bool,
    },
    /// Put Craft MCP Hub in the Applications folder, so it is in Launchpad and Spotlight (macOS)
    InstallApp {
        /// Also point the crafthub and crafthub-panel programs in this folder at the app's
        #[arg(long)]
        link: Option<std::path::PathBuf>,
    },
    /// Bridge an AI client session to an app's shared server (this is what AI clients run)
    Connect { id: String },
    /// Run an app's shared server (started automatically; you don't need to run it yourself)
    #[command(hide = true)]
    Serve { id: String },
}

#[derive(Subcommand)]
enum ClientAction {
    /// Register the apps that are on with this client, and new ones from now on
    On { client: String },
    /// Take the hub's servers out of this client
    Off { client: String },
    /// Describe a client the hub doesn't know, so it can register apps with it too
    Add {
        /// A short id for the client, e.g. `windsurf`
        id: String,
        /// The client's MCP settings file, as a full path
        #[arg(long)]
        file: String,
        /// Display name
        #[arg(long)]
        name: Option<String>,
        /// The file's format: json or toml
        #[arg(long, default_value = "json")]
        format: String,
        /// The object or table holding the servers (default: mcpServers for json, mcp_servers for toml)
        #[arg(long)]
        key: Option<String>,
        /// Its JSON entries carry "type": "stdio"
        #[arg(long)]
        typed: bool,
        /// It reads its settings only when it starts
        #[arg(long)]
        restart: bool,
    },
    /// Forget a client that was added with `clients add`
    Remove { id: String },
}

fn main() -> ExitCode {
    let ok = match Cli::parse().command {
        Command::List { app, json } => list(app.as_deref(), json),
        Command::Setup => setup(),
        Command::Add { id, cli, name, like, port, settings, off } => add(&id, AddOptions { cli, name, like, port, settings }, off),
        Command::Remove { id } => done(hub::remove(&id), &format!("{id}: removed")),
        Command::On { app } => each(&app, "on", hub::turn_on),
        Command::Off { app } => each(&app, "off", hub::turn_off),
        Command::Port { id, port } => done(hub::set_port(&id, port), &format!("{id}: port {port}")),
        Command::Root { id, dirs, anywhere } => root(&id, dirs, anywhere),
        Command::Check { app } => check(app.as_deref().unwrap_or("all")),
        Command::Doctor { id } => {
            println!("{}", doctor::report(&id));
            true
        }
        Command::Restart { id } if id == "all" => restart_running(),
        Command::Restart { id } => done(hub::restart(&id), &format!("{id}: restarted")),
        Command::CloseLibrary { id } => done(hub::close_library(&id), &format!("{id}: library closed")),
        Command::Catalog { update } => show_catalog(update),
        Command::Clients { action: None, json } => list_clients(json),
        Command::Clients { action: Some(ClientAction::On { client }), .. } => set_client(&client, true),
        Command::Clients { action: Some(ClientAction::Off { client }), .. } => set_client(&client, false),
        Command::Clients { action: Some(ClientAction::Add { id, file, name, format, key, typed, restart }), .. } => {
            let options = hub::ClientOptions { name, file, format: Some(format), key, typed, restart };
            done(hub::add_client(&id, options), &format!("{id}: added. `crafthub clients on {id}` registers the apps that are on with it."))
        }
        Command::Clients { action: Some(ClientAction::Remove { id }), .. } => done(hub::remove_client(&id), &format!("{id}: removed.")),
        Command::Guide => {
            print!("{}", guide::text());
            true
        }
        Command::Snippet { app, format } => snippet(app, &format),
        Command::Panel => panel(),
        Command::Mode { mode } => set_or_show_mode(mode.as_deref()),
        Command::Tools { app } => tools(app.as_deref()),
        Command::Uninstall { yes } => uninstall_hub(yes),
        Command::UpdateCheck => update_check_now(),
        Command::InstallApp { link } => match macapp::install(link.as_deref()) {
            Ok(bundle) => {
                println!("Installed {} (in Launchpad and Spotlight as \"{}\").", bundle.display(), macapp::APP_NAME);
                true
            }
            Err(why) => {
                eprintln!("{why}");
                false
            }
        },
        Command::Connect { id } => return ExitCode::from(shim::run(&id) as u8),
        Command::Serve { id } => return ExitCode::from(serve(&id) as u8),
    };
    if ok {
        ExitCode::SUCCESS
    } else {
        ExitCode::FAILURE
    }
}

fn done(outcome: Result<(), String>, message: &str) -> bool {
    match outcome {
        Ok(()) => {
            println!("{message}");
            true
        }
        Err(why) => {
            eprintln!("{why}");
            false
        }
    }
}

/// The ids `which` stands for: one app, or with `all` every app that has been added.
fn targets(which: &str) -> Vec<String> {
    if which == "all" {
        Hub::load().apps().into_iter().filter(|a| a.configured && a.available()).map(|a| a.id).collect()
    } else {
        vec![which.to_string()]
    }
}

fn each(which: &str, word: &str, action: fn(&str) -> Result<(), String>) -> bool {
    let ids = targets(which);
    if ids.is_empty() {
        println!("No apps have been added yet: `crafthub setup` adds every installed one.");
    }
    each_of(ids, word, action)
}

/// Restarts every server that is running. After an update they are still the previous version;
/// an app that is on with no server running needs nothing, its next session starts the new one.
fn restart_running() -> bool {
    let running = |app: &App| app.configured && matches!(control::status(app.port), Probe::Up(_));
    let ids: Vec<String> = Hub::load().apps().into_iter().filter(running).map(|app| app.id).collect();
    if ids.is_empty() {
        println!("No server is running.");
    }
    each_of(ids, "restarted", hub::restart)
}

fn each_of(ids: Vec<String>, word: &str, action: fn(&str) -> Result<(), String>) -> bool {
    let mut ok = true;
    for id in ids {
        match action(&id) {
            Ok(()) => println!("{id:<16} {word}"),
            Err(why) => {
                println!("{id:<16} FAILED: {why}");
                ok = false;
            }
        }
    }
    ok
}

fn origin(app: &App) -> &'static str {
    match app.origin {
        Origin::Official => "official",
        Origin::Community => "community",
        Origin::Custom => "custom",
    }
}

fn list(only: Option<&str>, as_json: bool) -> bool {
    let hub = Hub::load();
    let apps: Vec<App> = hub.apps().into_iter().filter(|a| only.is_none_or(|id| a.id == id)).collect();
    if apps.is_empty() {
        eprintln!("No such app. `crafthub list` shows every app the hub knows.");
        return false;
    }
    let registered = Snapshot::take(&hub.config);
    let rows: Vec<(App, hub::AppState)> = apps.into_iter().map(|app| (app.clone(), hub::state(&app, &registered))).collect();
    if as_json {
        let items: Vec<_> = rows
            .iter()
            .map(|(app, state)| {
                json!({
                    "id": app.id, "name": app.name, "origin": origin(app), "installed": app.installed(), "added": app.configured,
                    "port": app.port, "state": state.label, "detail": state.detail, "library": state.library,
                    "missingIn": state.partial, "files": app.roots(), "engine": app.engine_command(), "server": state.status,
                })
            })
            .collect();
        println!("{}", serde_json::to_string_pretty(&items).unwrap_or_default());
        return true;
    }
    let client_names: Vec<&str> = registered.clients().map(|client| client.name).collect();
    let mode_label = if hub.config.direct { "direct stdio (no background server)" } else { "shared hub server" };
    println!("Mode: {mode_label}");
    if client_names.is_empty() {
        println!("AI clients: none enabled (`crafthub clients` lists them)\n");
    } else {
        println!("AI clients: {}\n", client_names.join(", "));
    }
    println!("{:<16} {:<6} {:<14} DETAIL", "APP", "PORT", "STATE");
    for (app, state) in &rows {
        let port = if app.port == 0 || state.kind == StateKind::NotInstalled { "-".to_string() } else { app.port.to_string() };
        let mut detail = state.detail.clone();
        if let Some(library) = &state.library {
            detail = format!("{detail} · library: {library}");
        }
        if state.stale {
            detail = format!("{detail} · restart to apply settings");
        }
        if let Some(missing) = &state.partial {
            detail = format!("{detail} · {missing}: `crafthub on {}` registers it there", app.id);
        }
        if app.origin != Origin::Official {
            detail = format!("{detail} [{}]", origin(app));
        }
        println!("{:<16} {:<6} {:<14} {}", app.id, port, state.label.to_lowercase(), detail.trim_start_matches(" · ").trim());
    }
    let new = rows.iter().filter(|(_, s)| s.kind == StateKind::New).count();
    if new > 0 {
        println!("\n{new} installed app(s) not added yet: `crafthub setup` adds and turns them on.");
    }
    // What the last check found; `list` itself never goes online.
    if let Some(newer) = update_check::known_newer() {
        println!("\nCraft MCP Hub {} is available (this is {}): {}", newer.version, crafthub::VERSION, newer.url);
    }
    true
}

fn update_check_now() -> bool {
    if update_check::disabled() {
        println!("The update check is turned off (CRAFTHUB_NO_UPDATE_CHECK).");
        return true;
    }
    match update_check::newer(true) {
        Ok(Some(newer)) => {
            println!("Craft MCP Hub {} is available (this is {}).", newer.version, crafthub::VERSION);
            println!("What's new: {}", newer.url);
            println!("To update, run the install command again:\n  {}", update_check::how_to_update());
            true
        }
        Ok(None) => {
            println!("Craft MCP Hub {} is the latest version.", crafthub::VERSION);
            true
        }
        Err(why) => {
            eprintln!("Could not check for a newer version: {why}");
            false
        }
    }
}

fn setup() -> bool {
    let enabled = clients::enabled(&Config::load());
    if enabled.is_empty() {
        eprintln!("{}", hub::no_client_hint());
        return false;
    }
    let results = hub::setup();
    if results.is_empty() {
        println!("Nothing new: every installed app is already in the hub. (`crafthub list` shows them, `crafthub on all` turns them on.)");
        return true;
    }
    let mut ok = true;
    for (app, outcome) in &results {
        let port = Hub::load().app(&app.id).map_or(app.port, |a| a.port);
        match outcome {
            Ok(()) => println!("{:<16} on, port {port}", app.id),
            Err(why) => {
                println!("{:<16} FAILED: {why}", app.id);
                ok = false;
            }
        }
    }
    let names: Vec<&str> = enabled.iter().map(|client| client.name).collect();
    println!("\nRegistered in {}. Open a new session there to use them.", names.join(", "));
    ok
}

/// `crafthub root`: show or set the folders an app's engine may read and write.
fn root(id: &str, dirs: Vec<String>, anywhere: bool) -> bool {
    if dirs.is_empty() && !anywhere {
        let Some(app) = Hub::load().app(id) else {
            eprintln!("`{id}` is not an app the hub knows.");
            return false;
        };
        match app.roots() {
            None => println!("{id}: this engine can't be restricted to a folder (its command-line tool has no option for it)."),
            Some(roots) if roots.is_empty() => println!("{id}: no restriction, it may use any file you can."),
            Some(roots) => println!("{id}: may read and write {}", roots.join(", ")),
        }
        return true;
    }
    if anywhere && !dirs.is_empty() {
        eprintln!("Give folders or --anywhere, not both.");
        return false;
    }
    let message =
        if anywhere { format!("{id}: no restriction, it may use any file you can.") } else { format!("{id}: may read and write {}", dirs.join(", ")) };
    done(hub::set_roots(id, &dirs), &message)
}

/// `crafthub snippet`: the servers as text, for a client whose settings the hub can't edit.
fn snippet(app: Option<String>, format: &str) -> bool {
    let apps: Vec<String> = match app {
        Some(app) => vec![app],
        None => {
            let hub = Hub::load();
            let registered = Snapshot::take(&hub.config);
            let on: Vec<String> = hub.apps().iter().filter(|app| hub::state(app, &registered).is_on()).map(|app| app.id.clone()).collect();
            // Nothing is on yet: every app that has been added, so there is something to paste.
            if on.is_empty() {
                hub.apps().into_iter().filter(|app| app.configured && app.available()).map(|app| app.id).collect()
            } else {
                on
            }
        }
    };
    match clients::snippet(&apps, format) {
        Ok(text) => {
            print!("{text}");
            true
        }
        Err(why) => {
            eprintln!("{why}");
            false
        }
    }
}

/// `crafthub clients`: the AI clients the hub knows, which are on this computer, which are enabled.
fn list_clients(as_json: bool) -> bool {
    let config = Config::load();
    let enabled = clients::enabled(&config);
    let is_enabled = |id: &str| enabled.iter().any(|e| e.id == id);
    if as_json {
        let items: Vec<_> = clients::all_in(&config)
            .iter()
            .map(|client| {
                json!({
                    "id": client.id, "name": client.name, "builtIn": clients::is_built_in(client.id), "found": client.detected(),
                    "enabled": is_enabled(client.id), "settings": client.settings_file(), "needsRestart": client.needs_restart,
                })
            })
            .collect();
        println!("{}", serde_json::to_string_pretty(&items).unwrap_or_default());
        return true;
    }
    println!("{:<16} {:<16} {:<10} {:<8} SETTINGS", "CLIENT", "NAME", "FOUND", "ENABLED");
    for client in clients::all_in(&config) {
        let found = if client.detected() { "yes" } else { "-" };
        println!(
            "{:<16} {:<16} {:<10} {:<8} {}",
            client.id,
            client.name,
            found,
            if is_enabled(client.id) { "yes" } else { "-" },
            client.settings_file().display()
        );
    }
    println!("\n`crafthub clients on <client>` registers the apps that are on with it; `off` takes them out again.");
    println!("A client that isn't listed: `crafthub clients add <id> --file <its MCP settings file>` (see `crafthub guide`).");
    true
}

fn set_client(id: &str, on: bool) -> bool {
    let name = clients::get(id).map_or(id, |client| client.name);
    if !on {
        return done(hub::disable_client(id), &format!("{name}: disabled, the hub's servers were taken out of it."));
    }
    match hub::enable_client(id) {
        Ok(apps) => {
            let restart = if clients::get(id).is_some_and(|client| client.needs_restart) { format!(" Restart {name} to load them.") } else { String::new() };
            if apps.is_empty() {
                println!("{name}: enabled. No app is on yet: `crafthub on <app>` registers one.");
            } else {
                println!("{name}: enabled, registered {}.{restart}", apps.join(", "));
            }
            true
        }
        Err(why) => {
            eprintln!("{why}");
            false
        }
    }
}

fn add(id: &str, options: AddOptions, stay_off: bool) -> bool {
    let app = match hub::add(id, options) {
        Ok(app) => app,
        Err(why) => {
            eprintln!("{why}");
            return false;
        }
    };
    println!("{}: added on port {}", app.id, app.port);
    println!("  engine: {}", crafthub::proc::display_command(&app.engine_command().unwrap_or_default()));
    if app.profile.library {
        println!("  libraries: opened by the AI client (library_list, library_open); recent ones read from {}", app.profile.settings.join(", "));
    }
    if stay_off {
        return true;
    }
    done(hub::turn_on(id), &format!("{id}: on. Open a new AI client session to use it."))
}

fn check(which: &str) -> bool {
    let ids: Vec<String> = if which == "all" {
        let hub = Hub::load();
        let registered = Snapshot::take(&hub.config);
        hub.apps().into_iter().filter(|app| hub::state(app, &registered).is_on()).map(|a| a.id).collect()
    } else {
        vec![which.to_string()]
    };
    if ids.is_empty() {
        println!("No app is on.");
    }
    let mut ok = true;
    for id in ids {
        match hub::check(&id) {
            Ok(result) => println!("{id:<16} OK      {} tools in {} ms ({})", result.tools, result.millis, result.server),
            Err(why) => {
                println!("{id:<16} FAILED  {why}");
                ok = false;
            }
        }
    }
    ok
}

fn show_catalog(update: bool) -> bool {
    if update {
        match catalog::update() {
            Ok(count) => println!("Downloaded the catalog: {count} apps."),
            Err(why) => {
                eprintln!("{why}");
                return false;
            }
        }
    }
    let catalog = catalog::load();
    println!("{} apps, from {}", catalog.apps.len(), catalog.source);
    for app in &catalog.apps {
        println!("  {:<16} {:<16} {}", app.id, app.name, app.tagline);
    }
    true
}

fn uninstall_hub(yes: bool) -> bool {
    use std::io::{BufRead, IsTerminal, Write};
    let plan = uninstall::plan();
    if plan.is_empty() {
        println!("Craft MCP Hub is not installed here: there is nothing to remove.");
        return true;
    }
    println!("Uninstalling Craft MCP Hub will:");
    for step in &plan {
        println!("  - {step}");
    }
    println!("The ArtCraft apps themselves are not touched.");
    if !yes {
        if !std::io::stdin().is_terminal() {
            eprintln!("Run `crafthub uninstall --yes` to uninstall without being asked.");
            return false;
        }
        print!("Uninstall? [y/N] ");
        let _ = std::io::stdout().flush();
        let mut answer = String::new();
        let _ = std::io::stdin().lock().read_line(&mut answer);
        if !matches!(answer.trim(), "y" | "Y" | "yes" | "Yes") {
            println!("Nothing was changed.");
            return true;
        }
    }
    let mut ok = true;
    for (what, outcome) in uninstall::run() {
        match outcome {
            Ok(()) => println!("{what}"),
            Err(why) => {
                ok = false;
                eprintln!("{why}");
            }
        }
    }
    match uninstall::left_over() {
        Some(todo) => println!("\nAlmost done. {todo}"),
        None if ok => println!("\nCraft MCP Hub is uninstalled."),
        None => eprintln!("\nUninstalled, except for what is listed above."),
    }
    ok
}

/// Fetches the command-line tools the hub can fetch, and says what happened to each.
fn tools(only: Option<&str>) -> bool {
    let outcomes = match hub::fetch_tools(only) {
        Ok(outcomes) => outcomes,
        Err(why) => {
            eprintln!("{why}");
            return false;
        }
    };
    if outcomes.is_empty() {
        println!("Nothing to fetch: every installed app has its command-line tool.");
        return true;
    }
    let mut ok = true;
    for (name, outcome) in outcomes {
        match outcome {
            Ok(what) => println!("{name}: {what}"),
            Err(why) => {
                ok = false;
                eprintln!("{name}: {why}");
            }
        }
    }
    ok
}

fn panel() -> bool {
    // On macOS the app, when installed, so the panel has its name and icon in the Dock.
    let exe = paths::panel_exe();
    if cfg!(target_os = "macos") && macapp::open(&exe) {
        return true;
    }
    if !exe.is_file() {
        eprintln!("The control panel ({}) is not installed next to crafthub.", exe.display());
        return false;
    }
    match std::process::Command::new(&exe).spawn() {
        Ok(_) => true,
        Err(e) => {
            eprintln!("Could not open the control panel: {e}");
            false
        }
    }
}

fn set_or_show_mode(mode: Option<&str>) -> bool {
    match mode {
        None => {
            let direct = Config::load().direct;
            if direct {
                println!("Current mode: direct (AI clients launch `<app>-cli mcp` directly on stdio; no background server).");
            } else {
                println!("Current mode: hub (AI clients connect to a shared background server per app).");
            }
            println!("Switch with `crafthub mode direct` or `crafthub mode hub`.");
            true
        }
        Some("direct") => match hub::set_mode(true) {
            Ok(msg) => {
                println!("{msg}");
                true
            }
            Err(why) => {
                eprintln!("{why}");
                false
            }
        },
        Some("hub") => match hub::set_mode(false) {
            Ok(msg) => {
                println!("{msg}");
                true
            }
            Err(why) => {
                eprintln!("{why}");
                false
            }
        },
        Some(other) => {
            eprintln!("Unknown mode `{other}`: use `direct` or `hub`.");
            false
        }
    }
}

fn serve(id: &str) -> i32 {
    let log = logger::Logger::new(id);
    // A tool the hub fetched that has gone missing is fetched again before anything starts. One that
    // is only out of date is updated by the server in the background (`update` below).
    if Hub::load().app(id).is_some_and(|app| !app.installed() && app.fetchable) {
        match hub::update_tool(id) {
            Ok(_) => log.write("Fetched the engine's program"),
            Err(why) => log.write(&format!("Could not fetch the engine's program: {why}")),
        }
    }
    let Some(app) = Hub::load().app(id) else {
        log.write(&format!("`{id}` is not an app the hub knows"));
        return 2;
    };
    let Some(engine) = app.engine_command() else {
        log.write(&format!("{} is not installed: its command-line tool was not found", app.name));
        return 2;
    };
    let updated_id = app.id.clone();
    let update: server::ToolUpdate = std::sync::Arc::new(move || hub::update_tool(&updated_id));
    server::run(ServerSpec {
        app: app.id.clone(),
        port: app.port,
        engine,
        library_tools: app.profile.library,
        recent: app.recent_files(),
        update: Some(update),
        update_timing: Default::default(),
    })
}
