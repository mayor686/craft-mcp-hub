//! `crafthub-panel`: the control panel window.
//!
//! The window never waits on anything: a worker thread refreshes what the apps are doing every
//! couple of seconds and runs the actions the buttons ask for, one at a time.

#![cfg_attr(windows, windows_subsystem = "windows")]

use crafthub::clients::Snapshot;
use crafthub::hub::{self, AddOptions, App, AppState, Hub, Origin, StateKind};
use crafthub::update_check::{self, Newer};
use crafthub::{catalog, clients, doctor, uninstall};
use eframe::egui::{self, Color32, RichText};
use std::collections::HashMap;
use std::path::PathBuf;
use std::sync::mpsc::{self, Receiver, RecvTimeoutError, Sender};
use std::sync::{Arc, Mutex, MutexGuard};
use std::thread;
use std::time::{Duration, Instant};

const GREEN: Color32 = Color32::from_rgb(61, 214, 140);
const YELLOW: Color32 = Color32::from_rgb(245, 185, 53);
const RED: Color32 = Color32::from_rgb(240, 67, 90);
const MUTED: Color32 = Color32::from_rgb(138, 140, 148);

// The table's measures. A row's fixed columns keep these widths; the name and what the engine
// works on share the rest, so the table always fills the window.
const ROW_PAD: f32 = 10.0;
const GAP: f32 = 14.0;
const PORT_WIDTH: f32 = 70.0;
const STATUS_WIDTH: f32 = 170.0;
const ACTIONS_WIDTH: f32 = 226.0;
const NAME_MIN_WIDTH: f32 = 170.0;
const EXTRA_MIN_WIDTH: f32 = 200.0;
const WIDE_ROW_HEIGHT: f32 = 56.0;
const COMPACT_ROW_HEIGHT: f32 = 74.0;
/// In a table narrower than this, a row takes two lines.
const WIDE_MIN_WIDTH: f32 = 4.0 + 2.0 * ROW_PAD + NAME_MIN_WIDTH + EXTRA_MIN_WIDTH + PORT_WIDTH + STATUS_WIDTH + ACTIONS_WIDTH + 4.0 * GAP;
/// The table is never narrower than this: a window that is, scrolls sideways.
const COMPACT_MIN_WIDTH: f32 = 4.0 + 2.0 * ROW_PAD + 150.0 + STATUS_WIDTH + ACTIONS_WIDTH + 2.0 * GAP;

/// A part of a row with a set width, laid out its own way.
fn cell(ui: &mut egui::Ui, width: f32, height: f32, layout: egui::Layout, add: impl FnOnce(&mut egui::Ui)) {
    let width = width.max(0.0);
    ui.allocate_ui_with_layout(egui::vec2(width, height), layout, |ui| {
        ui.set_width(width);
        add(ui);
    });
}

/// A label that takes what its text needs, up to `max_width`, and is cut with an ellipsis beyond.
fn label_up_to(ui: &mut egui::Ui, max_width: f32, text: RichText) -> egui::Response {
    ui.scope(|ui| {
        ui.set_max_width(max_width.max(30.0));
        ui.label(text)
    })
    .inner
}

/// The heading over what an app's engine works on, if it works on something the hub manages.
fn extra_header(app: &App) -> Option<&'static str> {
    if app.profile.library {
        Some("LIBRARY")
    } else if app.confinable() {
        Some("FILES IT MAY USE")
    } else {
        None
    }
}

#[derive(Clone)]
struct Row {
    app: App,
    state: AppState,
}

struct CheckResult {
    ok: bool,
    text: String,
    at: Instant,
}

struct Diagnosis {
    id: String,
    name: String,
    /// `None` while the checks run.
    text: Option<String>,
}

/// An AI client the hub can register apps with, for the row of checkboxes.
#[derive(Clone)]
struct ClientRow {
    id: &'static str,
    name: &'static str,
    found: bool,
    enabled: bool,
    needs_restart: bool,
}

#[derive(Default)]
struct Shared {
    rows: Vec<Row>,
    clients: Vec<ClientRow>,
    direct: bool,
    message: String,
    /// What the worker is doing right now; buttons are disabled meanwhile.
    busy: Option<String>,
    checks: HashMap<String, CheckResult>,
    diagnosis: Option<Diagnosis>,
    error: Option<String>,
    /// What uninstalling did, once it is done: the panel then only says so and closes.
    uninstalled: Option<String>,
    /// A newer Craft MCP Hub on GitHub, once the check has found one.
    newer: Option<Newer>,
}

enum Job {
    Add(String),
    AddCustom { id: String, options: AddOptions },
    Remove(String),
    TurnOn(String),
    TurnOff(String),
    Restart(String),
    SetPort(String, u16),
    SetRoots(String, Vec<String>),
    CloseLibrary(String),
    Diagnose(String),
    SetUpNew,
    AllOn,
    AllOff,
    CheckAll,
    UpdateCatalog,
    SetClient(&'static str, bool),
    SetMode(bool),
    Uninstall,
}

fn lock<T>(mutex: &Mutex<T>) -> MutexGuard<'_, T> {
    mutex.lock().unwrap_or_else(|poisoned| poisoned.into_inner())
}

// ---------------------------------------------------------------- worker

fn refresh(shared: &Mutex<Shared>) {
    let hub = Hub::load();
    let registered = Snapshot::take(&hub.config);
    // Clients rewrite their settings often: when none can be read this instant, keep what we have.
    if registered.unreadable() {
        return;
    }
    let rows: Vec<Row> = hub.apps().into_iter().map(|app| Row { state: hub::state(&app, &registered), app }).collect();
    let enabled: Vec<&str> = registered.clients().map(|client| client.id).collect();
    let client_rows: Vec<ClientRow> = clients::all()
        .iter()
        .map(|client| ClientRow {
            id: client.id,
            name: client.name,
            found: client.detected(),
            enabled: enabled.contains(&client.id),
            needs_restart: client.needs_restart,
        })
        .collect();
    let mut shared = lock(shared);
    shared.rows = rows;
    shared.clients = client_rows;
    shared.direct = hub.config.direct;
}

fn worker(shared: Arc<Mutex<Shared>>, jobs: Receiver<Job>, ctx: egui::Context) {
    loop {
        refresh(&shared);
        ctx.request_repaint();
        match jobs.recv_timeout(Duration::from_secs(2)) {
            Ok(job) => run(job, &shared, &ctx),
            Err(RecvTimeoutError::Timeout) => {}
            Err(RecvTimeoutError::Disconnected) => break,
        }
    }
}

fn run(job: Job, shared: &Mutex<Shared>, ctx: &egui::Context) {
    let busy = |text: String| {
        lock(shared).busy = Some(text);
        ctx.request_repaint();
    };
    let name = |id: &str| lock(shared).rows.iter().find(|r| r.app.id == id).map_or(id.to_string(), |r| r.app.name.clone());
    // Runs one action on several apps; returns what went wrong, app by app.
    let on_each = |ids: Vec<String>, verb: &str, action: &dyn Fn(&str) -> Result<(), String>| {
        let mut errors = Vec::new();
        for id in &ids {
            busy(format!("{verb} {}…", name(id)));
            if let Err(why) = action(id) {
                errors.push(format!("{}: {why}", name(id)));
            }
        }
        if errors.is_empty() {
            Ok(ids.len())
        } else {
            Err(errors.join("\n\n"))
        }
    };
    let ids_where = |keep: &dyn Fn(&Row) -> bool| -> Vec<String> { lock(shared).rows.iter().filter(|r| keep(r)).map(|r| r.app.id.clone()).collect() };
    let session_hint = "Open a new AI client session to use it.";

    if let Job::Uninstall = job {
        busy("Uninstalling Craft MCP Hub…".to_string());
        let mut report: Vec<String> = uninstall::run()
            .into_iter()
            .map(|(what, outcome)| match outcome {
                Ok(()) => format!("✔ {what}"),
                Err(why) => format!("✘ {why}"),
            })
            .collect();
        report.extend(uninstall::left_over());
        let mut state = lock(shared);
        state.busy = None;
        state.uninstalled = Some(report.join("\n"));
        drop(state);
        ctx.request_repaint();
        return;
    }

    let outcome: Result<String, String> = match job {
        Job::Add(id) | Job::TurnOn(id) => {
            busy(format!("Turning on {}…", name(&id)));
            hub::turn_on(&id).map(|()| format!("{} is on. {session_hint}", name(&id)))
        }
        Job::AddCustom { id, options } => {
            busy(format!("Adding {id}…"));
            hub::add(&id, options).and_then(|app| hub::turn_on(&app.id).map(|()| format!("{} is on, port {}. {session_hint}", app.name, app.port)))
        }
        Job::Remove(id) => {
            busy(format!("Removing {}…", name(&id)));
            hub::remove(&id).map(|()| format!("{id} removed."))
        }
        Job::TurnOff(id) => {
            busy(format!("Turning off {}…", name(&id)));
            hub::turn_off(&id).map(|()| format!("{} is off.", name(&id)))
        }
        Job::Restart(id) => {
            busy(format!("Restarting {}…", name(&id)));
            hub::restart(&id).map(|()| format!("{} restarted.", name(&id)))
        }
        Job::SetPort(id, port) => {
            busy(format!("Moving {} to port {port}…", name(&id)));
            hub::set_port(&id, port).map(|()| format!("{} is on port {port}.", name(&id)))
        }
        Job::SetRoots(id, dirs) => {
            busy(format!("Changing the files {} may use…", name(&id)));
            hub::set_roots(&id, &dirs).map(|()| match dirs.as_slice() {
                [] => format!("{} may now use any file you can.", name(&id)),
                dirs => format!("{} may now read and write only {}.", name(&id), dirs.join(", ")),
            })
        }
        Job::CloseLibrary(id) => {
            busy(format!("Closing the library of {}…", name(&id)));
            hub::close_library(&id).map(|()| format!("{}: library closed, the desktop app can open it now.", name(&id)))
        }
        Job::Diagnose(id) => {
            lock(shared).diagnosis = Some(Diagnosis { id: id.clone(), name: name(&id), text: None });
            busy(format!("Checking {}…", name(&id)));
            let text = doctor::report(&id);
            if let Some(diagnosis) = lock(shared).diagnosis.as_mut().filter(|d| d.id == id) {
                diagnosis.text = Some(text);
            }
            Ok(String::new())
        }
        Job::SetUpNew => on_each(ids_where(&|r| r.state.kind == StateKind::New), "Turning on", &|id| hub::turn_on(id))
            .map(|n| format!("{n} new app(s) set up. {session_hint}")),
        Job::AllOn => on_each(ids_where(&|r| r.app.configured && r.app.installed()), "Turning on", &|id| hub::turn_on(id)).map(|n| format!("{n} app(s) on.")),
        Job::AllOff => on_each(ids_where(&|r| r.state.is_on()), "Turning off", &|id| hub::turn_off(id)).map(|n| format!("{n} app(s) off.")),
        Job::CheckAll => {
            let ids = ids_where(&|r| r.state.is_on());
            let mut failed = 0;
            for id in &ids {
                busy(format!("Checking {}…", name(id)));
                let result = match hub::check(id) {
                    Ok(ok) => CheckResult { ok: true, text: format!("{} tools in {} ms", ok.tools, ok.millis), at: Instant::now() },
                    Err(why) => {
                        failed += 1;
                        CheckResult { ok: false, text: why, at: Instant::now() }
                    }
                };
                lock(shared).checks.insert(id.clone(), result);
            }
            Ok(match (ids.len(), failed) {
                (0, _) => "No app is on.".to_string(),
                (n, 0) => format!("All {n} apps that are on answer."),
                (_, failed) => format!("{failed} app(s) have a problem: open their Diagnose."),
            })
        }
        Job::UpdateCatalog => {
            busy("Fetching the app catalog…".to_string());
            catalog::update().map(|n| format!("Catalog updated: {n} apps."))
        }
        Job::Uninstall => unreachable!("handled above"),
        Job::SetClient(id, on) => {
            let client = clients::get(id);
            let client_name = client.map_or(id, |c| c.name);
            if on {
                busy(format!("Registering the apps with {client_name}…"));
                hub::enable_client(id).map(|apps| {
                    let restart = if client.is_some_and(|c| c.needs_restart) { format!(" Restart {client_name} to load them.") } else { String::new() };
                    match apps.len() {
                        0 => format!("{client_name} enabled. Turn an app on to register it there."),
                        n => format!("{client_name} enabled: {n} app(s) registered.{restart}"),
                    }
                })
            } else {
                busy(format!("Taking the apps out of {client_name}…"));
                hub::disable_client(id).map(|()| format!("{client_name} disabled: the hub's servers were taken out of it."))
            }
        }
        Job::SetMode(direct) => {
            busy(if direct { "Switching to direct stdio mode…".to_string() } else { "Switching to shared hub mode…".to_string() });
            hub::set_mode(direct)
        }
    };

    refresh(shared);
    let mut state = lock(shared);
    state.busy = None;
    match outcome {
        Ok(message) if message.is_empty() => {}
        Ok(message) => state.message = message,
        Err(why) => {
            state.message = "Something went wrong.".to_string();
            state.error = Some(why);
        }
    }
    drop(state);
    ctx.request_repaint();
}

// ---------------------------------------------------------------- window

#[derive(Default)]
struct CustomForm {
    id: String,
    name: String,
    cli: String,
    like: String,
    settings: String,
}

/// The window for choosing the folders an app's engine may use.
struct RootEdit {
    id: String,
    name: String,
    /// The folders being typed, one per line.
    folders: String,
    /// Whether the engine takes more than one folder.
    several: bool,
    /// Whether the engine needs a folder to have any file access.
    required: bool,
}

struct Panel {
    shared: Arc<Mutex<Shared>>,
    jobs: Sender<Job>,
    /// Port fields being edited, by app.
    ports: HashMap<String, u16>,
    custom: Option<CustomForm>,
    /// The file-folder window: app id, app name, the folder being typed.
    root_edit: Option<RootEdit>,
    /// The uninstall confirmation, with what uninstalling would do.
    uninstall: Option<Vec<String>>,
    /// The new-version notice was put away until the panel is next opened.
    newer_dismissed: bool,
    /// For taking a picture of the panel: where to save it, and how far along we are.
    screenshot: Option<PathBuf>,
    frames: u32,
    shot_requested: bool,
}

impl Panel {
    fn new(cc: &eframe::CreationContext<'_>) -> Panel {
        cc.egui_ctx.set_visuals(egui::Visuals::dark());
        cc.egui_ctx.style_mut(|style| {
            style.spacing.item_spacing = egui::vec2(8.0, 6.0);
            style.spacing.button_padding = egui::vec2(10.0, 5.0);
            // Scroll bars that take their own space, so they never cover a row's buttons.
            style.spacing.scroll = egui::style::ScrollStyle::solid();
        });
        let shared = Arc::new(Mutex::new(Shared::default()));
        let (jobs, queue) = mpsc::channel();
        let (for_worker, ctx) = (shared.clone(), cc.egui_ctx.clone());
        thread::spawn(move || worker(for_worker, queue, ctx));
        // Used to take the pictures in the README: save one of the window and close, optionally
        // with an app's diagnosis open.
        let screenshot = std::env::var_os("CRAFTHUB_PANEL_SCREENSHOT").map(PathBuf::from);
        if let Some(id) = std::env::var("CRAFTHUB_PANEL_DIAGNOSE").ok().filter(|_| screenshot.is_some()) {
            let _ = jobs.send(Job::Diagnose(id));
        }
        Panel {
            shared,
            jobs,
            ports: HashMap::new(),
            custom: None,
            root_edit: None,
            uninstall: None,
            newer_dismissed: false,
            screenshot,
            frames: 0,
            shot_requested: false,
        }
    }

    /// Says that a newer version is out, with a way to its release page on GitHub.
    fn newer_notice(&mut self, ui: &mut egui::Ui, newer: &Newer) {
        egui::Frame::new().fill(Color32::from_rgb(28, 52, 92)).corner_radius(6.0).inner_margin(egui::Margin::symmetric(10, 8)).show(ui, |ui| {
            ui.horizontal_wrapped(|ui| {
                ui.label(RichText::new(format!("Craft MCP Hub {} is available", newer.version)).strong().color(Color32::WHITE));
                ui.label(RichText::new(format!("(this is {})", crafthub::VERSION)).color(Color32::from_gray(200)));
                if ui.button("See what's new on GitHub").on_hover_text(&newer.url).clicked() {
                    ui.ctx().open_url(egui::OpenUrl::new_tab(&newer.url));
                }
                if ui.button("Copy the update command").on_hover_text(update_check::how_to_update()).clicked() {
                    ui.ctx().copy_text(update_check::how_to_update().to_string());
                }
                if ui.button("Later").on_hover_text("Hide this until the panel is opened again").clicked() {
                    self.newer_dismissed = true;
                }
            });
        });
        ui.add_space(4.0);
    }

    fn send(&self, job: Job) {
        let _ = self.jobs.send(job);
    }

    /// One app. In a wide window it is one line of columns that share the width, with the actions at
    /// the right edge; in a narrow one it takes two lines, so everything stays in view.
    #[allow(clippy::too_many_arguments)]
    fn app_row(&mut self, ui: &mut egui::Ui, index: usize, width: f32, compact: bool, row: &Row, busy: bool, check: Option<(bool, String)>) {
        let (app, state) = (&row.app, &row.state);
        let height = if compact { COMPACT_ROW_HEIGHT } else { WIDE_ROW_HEIGHT };
        let (rect, _) = ui.allocate_exact_size(egui::vec2(width, height), egui::Sense::hover());
        if index % 2 == 1 {
            ui.painter().rect_filled(rect, egui::CornerRadius::same(4), ui.visuals().faint_bg_color);
        }
        let bar = egui::Rect::from_min_max(egui::pos2(rect.left(), rect.top() + 7.0), egui::pos2(rect.left() + 4.0, rect.bottom() - 7.0));
        ui.painter().rect_filled(bar, egui::CornerRadius::same(2), Color32::from_rgb(app.color[0], app.color[1], app.color[2]));

        let content =
            egui::Rect::from_min_max(egui::pos2(rect.left() + 4.0 + ROW_PAD, rect.top() + 6.0), egui::pos2(rect.right() - ROW_PAD, rect.bottom() - 6.0));
        let top_down = egui::Layout::top_down(egui::Align::LEFT);
        let inline = egui::Layout::left_to_right(egui::Align::Center);
        let from_right = egui::Layout::right_to_left(egui::Align::Center);
        let mut row_ui = ui.new_child(egui::UiBuilder::new().max_rect(content).layout(top_down));
        let ui = &mut row_ui;
        // A label that doesn't fit its column is cut with an ellipsis instead of pushing the others.
        ui.style_mut().wrap_mode = Some(egui::TextWrapMode::Truncate);
        ui.spacing_mut().item_spacing = egui::vec2(GAP, 4.0);

        if compact {
            let name_width = content.width() - STATUS_WIDTH - ACTIONS_WIDTH - 2.0 * GAP;
            cell(ui, content.width(), 28.0, inline, |ui| {
                cell(ui, name_width, 28.0, inline, |ui| self.name(ui, app, state));
                cell(ui, STATUS_WIDTH, 28.0, inline, |ui| {
                    self.status_head(ui, state);
                    self.status_detail(ui, state, check.clone());
                });
                let rest = ui.available_width();
                cell(ui, rest, 28.0, from_right, |ui| self.actions(ui, app, state, busy));
            });
            cell(ui, content.width(), 26.0, inline, |ui| {
                let port_width = PORT_WIDTH + 44.0;
                cell(ui, content.width() - port_width - GAP, 26.0, inline, |ui| match extra_header(app) {
                    Some(header) => {
                        ui.spacing_mut().item_spacing.x = 8.0;
                        ui.label(RichText::new(header).small().color(MUTED));
                        self.extra_value(ui, app, state, busy);
                    }
                    None => self.tagline(ui, app),
                });
                let rest = ui.available_width();
                cell(ui, rest, 26.0, from_right, |ui| {
                    ui.spacing_mut().item_spacing.x = 8.0;
                    self.port_field(ui, app, busy);
                    ui.label(RichText::new("PORT").small().color(MUTED));
                });
            });
        } else {
            // The name and what the engine works on share whatever the fixed columns leave.
            let flexible = content.width() - PORT_WIDTH - STATUS_WIDTH - ACTIONS_WIDTH - 4.0 * GAP;
            let name_width = (flexible * 0.45).clamp(NAME_MIN_WIDTH, 340.0);
            let cell_height = content.height();
            cell(ui, content.width(), cell_height, inline, |ui| {
                cell(ui, name_width, cell_height, top_down, |ui| {
                    ui.horizontal(|ui| self.name(ui, app, state));
                    self.tagline(ui, app);
                });
                cell(ui, flexible - name_width, cell_height, top_down, |ui| {
                    if let Some(header) = extra_header(app) {
                        ui.label(RichText::new(header).small().color(MUTED));
                        ui.horizontal(|ui| self.extra_value(ui, app, state, busy));
                    }
                });
                cell(ui, PORT_WIDTH, cell_height, top_down, |ui| {
                    ui.label(RichText::new("PORT").small().color(MUTED));
                    self.port_field(ui, app, busy);
                });
                cell(ui, STATUS_WIDTH, cell_height, top_down, |ui| {
                    ui.horizontal(|ui| self.status_head(ui, state));
                    self.status_detail(ui, state, check.clone());
                });
                let rest = ui.available_width();
                cell(ui, rest, cell_height, from_right, |ui| self.actions(ui, app, state, busy));
            });
        }
    }

    fn name(&self, ui: &mut egui::Ui, app: &App, state: &AppState) {
        ui.spacing_mut().item_spacing.x = 6.0;
        ui.label(RichText::new(&app.name).strong().size(15.0));
        match app.origin {
            Origin::Custom => drop(ui.label(RichText::new("custom").small().color(MUTED))),
            Origin::Community => drop(ui.label(RichText::new("community").small().color(MUTED))),
            Origin::Official => {}
        }
        if state.kind == StateKind::New {
            ui.label(RichText::new("NEW").small().strong().color(YELLOW));
        }
    }

    fn tagline(&self, ui: &mut egui::Ui, app: &App) {
        let tagline = if app.tagline.is_empty() { app.id.as_str() } else { app.tagline.as_str() };
        ui.label(RichText::new(tagline).small().color(MUTED)).on_hover_text(tagline);
    }

    /// What the engine works on, in a line: the open library with its Release button, or the
    /// folder it may use with its Change button.
    fn extra_value(&mut self, ui: &mut egui::Ui, app: &App, state: &AppState, busy: bool) {
        ui.spacing_mut().item_spacing.x = 8.0;
        // The text gives way to the button, which must stay in view.
        let text_width = ui.available_width() - 84.0;
        if app.profile.library {
            match &state.library {
                Some(library) => {
                    let name = std::path::Path::new(library).file_name().map(|n| n.to_string_lossy().into_owned()).unwrap_or_else(|| library.clone());
                    label_up_to(ui, text_width, RichText::new(name)).on_hover_text(library);
                    if ui.add_enabled(!busy, egui::Button::new("Release").small()).on_hover_text("Close it, so the desktop app can open it").clicked() {
                        self.send(Job::CloseLibrary(app.id.clone()));
                    }
                }
                None if state.is_on() => drop(ui.label(RichText::new("none (the AI client opens one)").color(MUTED))),
                None => drop(ui.label(RichText::new("—").color(MUTED))),
            }
        } else if let Some(roots) = app.roots() {
            match roots.as_slice() {
                [] => drop(
                    label_up_to(ui, text_width, RichText::new("any file").color(YELLOW))
                        .on_hover_text("Not restricted: an AI client can have this engine open or save any file you can."),
                ),
                [only] => drop(label_up_to(ui, text_width, RichText::new(only)).on_hover_text(only)),
                [first, more @ ..] => {
                    // The count has its own place, so it stays when the first folder is cut short.
                    label_up_to(ui, text_width - 30.0, RichText::new(first)).on_hover_text(roots.join("\n"));
                    ui.label(format!("+{}", more.len())).on_hover_text(roots.join("\n"));
                }
            }
            if ui.add_enabled(!busy, egui::Button::new("Change…").small()).clicked() {
                self.root_edit = Some(RootEdit {
                    id: app.id.clone(),
                    name: app.name.clone(),
                    folders: roots.join("\n"),
                    several: app.profile.multi_root,
                    required: app.profile.root_required,
                });
            }
        }
    }

    fn port_field(&mut self, ui: &mut egui::Ui, app: &App, busy: bool) {
        if app.port == 0 || !app.installed() {
            ui.label(RichText::new("—").color(MUTED));
            return;
        }
        let mut port = *self.ports.get(&app.id).unwrap_or(&app.port);
        let field = ui.add_enabled(!busy, egui::DragValue::new(&mut port).range(1024..=65535).speed(0.1));
        if field.has_focus() || field.dragged() {
            self.ports.insert(app.id.clone(), port);
        } else {
            self.ports.remove(&app.id);
        }
        if (field.lost_focus() || field.drag_stopped()) && port != app.port {
            self.send(Job::SetPort(app.id.clone(), port));
        }
    }

    fn status_head(&self, ui: &mut egui::Ui, state: &AppState) {
        ui.spacing_mut().item_spacing.x = 6.0;
        let color = match state.kind {
            StateKind::On => GREEN,
            StateKind::Starting | StateKind::New | StateKind::Other => YELLOW,
            StateKind::Error | StateKind::PortBusy => RED,
            _ => MUTED,
        };
        // Painted, not a character: the built-in font has no round bullet.
        let (dot, _) = ui.allocate_exact_size(egui::vec2(10.0, 14.0), egui::Sense::hover());
        if matches!(state.kind, StateKind::Off | StateKind::NotInstalled) {
            ui.painter().circle_stroke(dot.center(), 4.0, egui::Stroke::new(1.5_f32, color));
        } else {
            ui.painter().circle_filled(dot.center(), 4.5, color);
        }
        let label = ui.label(RichText::new(&state.label).strong().color(color));
        if !state.tip.is_empty() {
            label.on_hover_text(&state.tip);
        }
    }

    fn status_detail(&self, ui: &mut egui::Ui, state: &AppState, check: Option<(bool, String)>) {
        match check.filter(|_| state.is_on()) {
            Some((ok, text)) => {
                let verdict = if ok { "checked:" } else { "check failed:" };
                ui.label(RichText::new(format!("{verdict} {text}")).small().color(if ok { GREEN } else { RED })).on_hover_text(text);
            }
            None => {
                // What needs attention comes before the plain detail.
                let (detail, color) = match (&state.partial, state.stale) {
                    (Some(missing), _) => (missing.as_str(), YELLOW),
                    (None, true) => ("restart to apply settings", YELLOW),
                    (None, false) => (state.detail.as_str(), MUTED),
                };
                let label = ui.label(RichText::new(detail).small().color(color));
                if !state.tip.is_empty() {
                    label.on_hover_text(&state.tip);
                }
            }
        }
    }

    /// The row's buttons, added from the right edge inwards so the main one lines up in every row.
    fn actions(&mut self, ui: &mut egui::Ui, app: &App, state: &AppState, busy: bool) {
        ui.spacing_mut().item_spacing.x = 8.0;
        let (text, job) = match state.kind {
            StateKind::New => ("Add", Job::Add(app.id.clone())),
            StateKind::Off | StateKind::Other | StateKind::NotInstalled => ("Turn on", Job::TurnOn(app.id.clone())),
            _ => ("Turn off", Job::TurnOff(app.id.clone())),
        };
        let mut button = egui::Button::new(RichText::new(text).strong()).min_size(egui::vec2(84.0, 0.0));
        if matches!(state.kind, StateKind::New | StateKind::Off | StateKind::Other) {
            button = button.fill(Color32::from_rgb(52, 92, 168));
        }
        if ui.add_enabled(!busy && app.available(), button).clicked() {
            self.send(job);
        }
        if ui.add_enabled(!busy, egui::Button::new("Diagnose")).clicked() {
            self.send(Job::Diagnose(app.id.clone()));
        }
        let can_restart = state.is_on();
        let can_remove = app.origin == Origin::Custom && !state.is_on();
        if can_restart || can_remove {
            ui.add_enabled_ui(!busy, |ui| {
                ui.menu_button("…", |ui| {
                    if state.partial.is_some() && ui.button("Register in every client").clicked() {
                        self.send(Job::TurnOn(app.id.clone()));
                        ui.close_menu();
                    }
                    if can_restart && ui.button("Restart the server").clicked() {
                        self.send(Job::Restart(app.id.clone()));
                        ui.close_menu();
                    }
                    if can_remove && ui.button("Remove from the hub").clicked() {
                        self.send(Job::Remove(app.id.clone()));
                        ui.close_menu();
                    }
                })
                .response
                .on_hover_text("More");
            });
        }
    }

    fn windows(&mut self, ctx: &egui::Context, busy: bool) {
        // Diagnosis
        let diagnosis = lock(&self.shared).diagnosis.as_ref().map(|d| (d.id.clone(), d.name.clone(), d.text.clone()));
        if let Some((id, name, text)) = diagnosis {
            let mut open = true;
            egui::Window::new(format!("Diagnosis · {name}")).id(egui::Id::new("diagnosis")).open(&mut open).default_size([860.0, 520.0]).show(ctx, |ui| {
                let shown = text.clone().unwrap_or_else(|| "Checking…".to_string());
                egui::ScrollArea::vertical().max_height(ui.available_height() - 44.0).show(ui, |ui| {
                    ui.add(egui::TextEdit::multiline(&mut shown.as_str()).font(egui::TextStyle::Monospace).desired_width(f32::INFINITY));
                });
                ui.add_space(6.0);
                ui.horizontal(|ui| {
                    if ui.add_enabled(!busy, egui::Button::new("Check again")).clicked() {
                        self.send(Job::Diagnose(id.clone()));
                    }
                    if ui.button("Copy").clicked() {
                        ctx.copy_text(shown.clone());
                    }
                });
            });
            if !open {
                lock(&self.shared).diagnosis = None;
            }
        }

        // Uninstalling: first what it will do, then what it did.
        let uninstalled = lock(&self.shared).uninstalled.clone();
        if let Some(report) = uninstalled {
            egui::Window::new("Craft MCP Hub was uninstalled").collapsible(false).resizable(false).anchor(egui::Align2::CENTER_CENTER, [0.0, 0.0]).show(
                ctx,
                |ui| {
                    ui.set_max_width(620.0);
                    ui.label(report);
                    ui.add_space(8.0);
                    if ui.button(RichText::new("Close").strong()).clicked() {
                        ctx.send_viewport_cmd(egui::ViewportCommand::Close);
                    }
                },
            );
            return;
        }
        let mut close_uninstall = false;
        if let Some(plan) = &self.uninstall {
            egui::Window::new("Uninstall Craft MCP Hub?").collapsible(false).resizable(false).anchor(egui::Align2::CENTER_CENTER, [0.0, 0.0]).show(ctx, |ui| {
                ui.set_max_width(620.0);
                if plan.is_empty() {
                    ui.label("There is nothing to remove.");
                } else {
                    ui.label("This will:");
                    for step in plan {
                        ui.label(format!("•  {step}"));
                    }
                }
                ui.add_space(4.0);
                ui.label(RichText::new("The ArtCraft apps themselves are not touched. Open AI client sessions lose these servers.").small().color(MUTED));
                ui.add_space(8.0);
                ui.horizontal(|ui| {
                    let confirm = egui::Button::new(RichText::new("Uninstall").strong().color(Color32::WHITE)).fill(Color32::from_rgb(178, 48, 48));
                    if ui.add_enabled(!busy && !plan.is_empty(), confirm).clicked() {
                        self.send(Job::Uninstall);
                        close_uninstall = true;
                    }
                    if ui.button("Cancel").clicked() {
                        close_uninstall = true;
                    }
                });
            });
        }
        if close_uninstall {
            self.uninstall = None;
        }

        // What went wrong
        let error = lock(&self.shared).error.clone();
        if let Some(error) = error {
            egui::Window::new("Something went wrong").collapsible(false).resizable(false).anchor(egui::Align2::CENTER_CENTER, [0.0, 0.0]).show(ctx, |ui| {
                ui.set_max_width(560.0);
                ui.label(&error);
                ui.add_space(8.0);
                if ui.button("OK").clicked() {
                    lock(&self.shared).error = None;
                }
            });
        }

        // The folders an engine may use
        let mut close_root = false;
        if let Some(edit) = &mut self.root_edit {
            let mut open = true;
            egui::Window::new(format!("Files {} may use", edit.name)).open(&mut open).collapsible(false).resizable(false).show(ctx, |ui| {
                ui.set_max_width(480.0);
                if edit.several {
                    ui.label("The folders (and everything in them) the engine may read and write, one per line:");
                    ui.add(egui::TextEdit::multiline(&mut edit.folders).desired_rows(4).desired_width(460.0));
                } else {
                    ui.label("The one folder (and everything in it) the engine may read and write:");
                    ui.add(egui::TextEdit::singleline(&mut edit.folders).desired_width(460.0));
                }
                ui.label(RichText::new("An AI client can't have this engine open or save anything outside.").small().color(MUTED));
                ui.add_space(6.0);
                let folders: Vec<String> = edit.folders.lines().map(|line| line.trim().trim_matches('"').to_string()).filter(|line| !line.is_empty()).collect();
                ui.horizontal(|ui| {
                    let label = if edit.several { "Use these folders" } else { "Use this folder" };
                    if ui.add_enabled(!busy && !folders.is_empty(), egui::Button::new(RichText::new(label).strong())).clicked() {
                        let _ = self.jobs.send(Job::SetRoots(edit.id.clone(), folders.clone()));
                        close_root = true;
                    }
                    // Not offered for an engine that has no file access at all without a folder.
                    if !edit.required
                        && ui
                            .add_enabled(!busy, egui::Button::new("Allow any file"))
                            .on_hover_text("Lift the restriction: it may use any file you can")
                            .clicked()
                    {
                        let _ = self.jobs.send(Job::SetRoots(edit.id.clone(), Vec::new()));
                        close_root = true;
                    }
                });
            });
            close_root |= !open;
        }
        if close_root {
            self.root_edit = None;
        }

        // A custom app
        let mut close_custom = false;
        if let Some(form) = &mut self.custom {
            // A file dropped on the window is the app's command-line tool.
            if let Some(dropped) = ctx.input(|i| i.raw.dropped_files.first().and_then(|f| f.path.clone())) {
                form.cli = dropped.to_string_lossy().into_owned();
                if form.id.is_empty() {
                    let stem = dropped.file_stem().map(|s| s.to_string_lossy().to_lowercase()).unwrap_or_default();
                    form.id = stem.trim_end_matches("-cli").replace(|c: char| !c.is_ascii_alphanumeric() && c != '-', "-");
                }
            }
            let mut open = true;
            egui::Window::new("Add a custom app").open(&mut open).collapsible(false).resizable(false).show(ctx, |ui| {
                ui.label("A fork or your own build of an ArtCraft app. Its command-line tool must have an `mcp` command.");
                ui.add_space(6.0);
                egui::Grid::new("custom-form").num_columns(2).spacing([12.0, 8.0]).show(ui, |ui| {
                    ui.label("Command-line tool");
                    ui.add(egui::TextEdit::singleline(&mut form.cli).hint_text("full path to …-cli — or drop the file on this window").desired_width(430.0));
                    ui.end_row();
                    ui.label("Id");
                    ui.add(egui::TextEdit::singleline(&mut form.id).hint_text("short name the AI client sees, e.g. chromaforge").desired_width(430.0));
                    ui.end_row();
                    ui.label("Name");
                    ui.add(egui::TextEdit::singleline(&mut form.name).hint_text("optional").desired_width(430.0));
                    ui.end_row();
                    ui.label("Start it like");
                    egui::ComboBox::from_id_salt("like")
                        .selected_text(if form.like.is_empty() { "work it out from its --help" } else { form.like.as_str() })
                        .show_ui(ui, |ui| {
                            ui.selectable_value(&mut form.like, String::new(), "work it out from its --help");
                            for known in ["lightcraft", "vectorcraft", "photocraft", "figcraft"] {
                                ui.selectable_value(&mut form.like, known.to_string(), known);
                            }
                        });
                    ui.end_row();
                    ui.label("Settings folder");
                    ui.add(
                        egui::TextEdit::singleline(&mut form.settings).hint_text("optional: where a photo app lists its recent libraries").desired_width(430.0),
                    );
                    ui.end_row();
                });
                ui.add_space(8.0);
                let ready = !form.cli.trim().is_empty() && !form.id.trim().is_empty();
                if ui.add_enabled(!busy && ready, egui::Button::new(RichText::new("Add and turn on").strong())).clicked() {
                    let text = |s: &str| (!s.trim().is_empty()).then(|| s.trim().to_string());
                    let options = AddOptions {
                        cli: text(&form.cli.replace('"', "")),
                        name: text(&form.name),
                        like: text(&form.like),
                        port: None,
                        settings: text(&form.settings).into_iter().collect(),
                    };
                    let _ = self.jobs.send(Job::AddCustom { id: form.id.trim().to_lowercase(), options });
                    close_custom = true;
                }
            });
            close_custom |= !open;
        }
        if close_custom {
            self.custom = None;
        }
    }

    /// Saves a picture of the window once it has something to show, then closes it.
    fn take_screenshot(&mut self, ctx: &egui::Context, ready: bool) {
        let Some(path) = self.screenshot.clone() else { return };
        self.frames += 1;
        if ready && self.frames > 40 && !self.shot_requested {
            ctx.send_viewport_cmd(egui::ViewportCommand::Screenshot(Default::default()));
            self.shot_requested = true;
        }
        let image = ctx.input(|i| {
            i.events.iter().find_map(|event| match event {
                egui::Event::Screenshot { image, .. } => Some(image.clone()),
                _ => None,
            })
        });
        if let Some(image) = image {
            if let Ok(file) = std::fs::File::create(&path) {
                let mut encoder = png::Encoder::new(std::io::BufWriter::new(file), image.size[0] as u32, image.size[1] as u32);
                encoder.set_color(png::ColorType::Rgba);
                encoder.set_depth(png::BitDepth::Eight);
                if let Ok(mut writer) = encoder.write_header() {
                    let _ = writer.write_image_data(image.as_raw());
                }
            }
            ctx.send_viewport_cmd(egui::ViewportCommand::Close);
        }
        ctx.request_repaint_after(Duration::from_millis(50));
    }
}

impl eframe::App for Panel {
    fn update(&mut self, ctx: &egui::Context, _frame: &mut eframe::Frame) {
        let (rows, client_rows, direct, message, busy_text, checks) = {
            let mut shared = lock(&self.shared);
            shared.checks.retain(|_, check| check.at.elapsed() < Duration::from_secs(90));
            let checks: HashMap<String, (bool, String)> = shared.checks.iter().map(|(id, c)| (id.clone(), (c.ok, c.text.clone()))).collect();
            (shared.rows.clone(), shared.clients.clone(), shared.direct, shared.message.clone(), shared.busy.clone(), checks)
        };
        let busy = busy_text.is_some();
        let new_apps = rows.iter().filter(|r| r.state.kind == StateKind::New).count();
        let (present, missing): (Vec<&Row>, Vec<&Row>) = rows.iter().partition(|r| r.app.available() || r.app.configured);

        let newer = if self.newer_dismissed { None } else { lock(&self.shared).newer.clone() };
        egui::TopBottomPanel::top("header").show(ctx, |ui| {
            ui.add_space(8.0);
            ui.horizontal(|ui| {
                ui.label(RichText::new("Craft MCP Hub").size(22.0).strong());
                ui.add_space(12.0);
                let mut is_direct = direct;
                if ui
                    .add_enabled(!busy, egui::Checkbox::new(&mut is_direct, "Direct stdio mode (no background daemon)"))
                    .on_hover_text("When checked, AI clients run <app>-cli mcp directly over stdio without background servers")
                    .clicked()
                {
                    self.send(Job::SetMode(is_direct));
                }
            });
            if let Some(newer) = &newer {
                self.newer_notice(ui, newer);
            }
            ui.label(
                RichText::new(
                    "Each app that is on has one windowless server on its own port, shared by every AI client session. \
                     A session starts the server when it isn't running; closing this window stops nothing.",
                )
                .color(MUTED),
            );
            ui.add_space(2.0);
            // The AI clients the apps are registered with. One that isn't on this computer can't
            // be ticked, but can be unticked if it was enabled before.
            ui.horizontal_wrapped(|ui| {
                ui.label(RichText::new("AI CLIENTS").small().color(MUTED));
                for client in &client_rows {
                    let mut on = client.enabled;
                    let hover = match (client.enabled, client.needs_restart) {
                        (true, _) => format!("Take the hub's servers out of {}", client.name),
                        (false, true) => format!("Register the apps that are on with {} (restart it afterwards)", client.name),
                        (false, false) => format!("Register the apps that are on with {}", client.name),
                    };
                    let checkbox = ui
                        .add_enabled(!busy && (client.found || client.enabled), egui::Checkbox::new(&mut on, client.name))
                        .on_hover_text(hover)
                        .on_disabled_hover_text(format!("{} was not found on this computer", client.name));
                    if checkbox.clicked() {
                        self.send(Job::SetClient(client.id, on));
                    }
                }
            });
            ui.add_space(6.0);
        });

        egui::TopBottomPanel::bottom("footer").show(ctx, |ui| {
            ui.add_space(8.0);
            // Wrapped: in a narrow window the buttons go onto a second line instead of out of view.
            ui.horizontal_wrapped(|ui| {
                let set_up = egui::Button::new(RichText::new(format!("Set up new apps ({new_apps})")).strong()).fill(Color32::from_rgb(52, 92, 168));
                if ui.add_enabled(!busy && new_apps > 0, set_up).on_hover_text("Add and turn on every installed app that isn't in the hub yet").clicked() {
                    self.send(Job::SetUpNew);
                }
                if ui.add_enabled(!busy, egui::Button::new("Turn all on")).clicked() {
                    self.send(Job::AllOn);
                }
                if ui.add_enabled(!busy, egui::Button::new("Turn all off")).clicked() {
                    self.send(Job::AllOff);
                }
                if ui.add_enabled(!busy, egui::Button::new("Check all")).on_hover_text("A full MCP round trip through every server that is on").clicked() {
                    self.send(Job::CheckAll);
                }
                if ui.add_enabled(!busy, egui::Button::new("Add custom app…")).clicked() {
                    self.custom = Some(CustomForm::default());
                }
                if ui.add_enabled(!busy, egui::Button::new("Uninstall…")).on_hover_text("Take Craft MCP Hub off this computer").clicked() {
                    self.uninstall = Some(uninstall::plan());
                }
            });
            let status = busy_text.as_deref().unwrap_or(&message);
            if !status.is_empty() {
                ui.label(RichText::new(status).color(MUTED));
            }
            ui.label(
                RichText::new(
                    "Photo apps start with no library open: the AI client opens the one it needs (library_list, library_open). \
                     A library opens in one program at a time: Release it here before opening it in the desktop app.",
                )
                .small()
                .color(MUTED),
            );
            ui.add_space(6.0);
        });

        egui::CentralPanel::default().show(ctx, |ui| {
            // Both ways: in a narrow window the actions at the end of each row must stay reachable.
            egui::ScrollArea::both().auto_shrink([false, false]).show(ui, |ui| {
                if rows.is_empty() {
                    ui.add_space(20.0);
                    ui.label(RichText::new("Looking for the apps…").color(MUTED));
                    return;
                }
                // The rows fill the window; when it is too narrow for one line each they take two,
                // and only below the table's smallest width does it scroll sideways.
                let available = ui.available_width();
                let compact = available < WIDE_MIN_WIDTH;
                let width = available.max(COMPACT_MIN_WIDTH);
                ui.spacing_mut().item_spacing.y = 2.0;
                for (index, row) in present.iter().enumerate() {
                    self.app_row(ui, index, width, compact, row, busy, checks.get(&row.app.id).cloned());
                }
                if !missing.is_empty() {
                    ui.add_space(12.0);
                    egui::CollapsingHeader::new(RichText::new(format!("Not installed ({})", missing.len())).color(MUTED)).show(ui, |ui| {
                        ui.horizontal(|ui| {
                            ui.label(RichText::new("They show up above once installed.").small().color(MUTED));
                            if ui
                                .add_enabled(!busy, egui::Button::new("Refresh the catalog").small())
                                .on_hover_text("Fetch the current app list from getartcraft.com")
                                .clicked()
                            {
                                self.send(Job::UpdateCatalog);
                            }
                        });
                        for row in &missing {
                            ui.label(format!("{}  ·  {}", row.app.name, row.app.tagline));
                        }
                    });
                }
            });
        });

        self.windows(ctx, busy);
        self.take_screenshot(ctx, !rows.is_empty() && !busy);
    }
}

/// The hub's icon, from the picture built into the program.
fn window_icon() -> Option<egui::IconData> {
    // On macOS the icon sits on the system's icon grid, with a margin, so it is the size of the
    // other apps' icons in the Dock.
    #[cfg(target_os = "macos")]
    let picture = &include_bytes!("../../assets/icon-macos-512.png")[..];
    #[cfg(not(target_os = "macos"))]
    let picture = &include_bytes!("../../assets/icon-256.png")[..];
    let mut reader = png::Decoder::new(picture).read_info().ok()?;
    let mut rgba = vec![0; reader.output_buffer_size()];
    let frame = reader.next_frame(&mut rgba).ok()?;
    rgba.truncate(frame.buffer_size());
    let plain = frame.color_type == png::ColorType::Rgba && frame.bit_depth == png::BitDepth::Eight;
    plain.then_some(egui::IconData { rgba, width: frame.width, height: frame.height })
}

fn main() -> eframe::Result {
    // `CRAFTHUB_PANEL_SIZE=800x500` opens the window at that size, to take a picture of it small.
    let size = std::env::var("CRAFTHUB_PANEL_SIZE").ok().and_then(|size| {
        let (width, height) = size.split_once('x')?;
        Some([width.parse::<f32>().ok()?, height.parse::<f32>().ok()?])
    });
    let mut viewport =
        egui::ViewportBuilder::default().with_title("Craft MCP Hub").with_inner_size(size.unwrap_or([1000.0, 780.0])).with_min_inner_size([480.0, 320.0]);
    // The window's own icon: its title bar, the taskbar, the dock.
    if let Some(icon) = window_icon() {
        viewport = viewport.with_icon(icon);
    }
    let options = eframe::NativeOptions { viewport, ..Default::default() };
    eframe::run_native("craft-mcp-hub", options, Box::new(|cc| Ok(Box::new(Panel::new(cc)))))
}
