//! The egui front end: a dense, sortable process table with kill and suspend.

use std::cmp::Ordering;
use std::collections::{BTreeSet, HashSet};
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};

use eframe::App;
use egui::{Align, Color32, Margin, RichText, Sense, ThemePreference, ViewportCommand};
use egui_extras::{Column, TableBuilder};
use raw_window_handle::{HasWindowHandle as _, RawWindowHandle};

use crate::collector::lock;
use crate::collector::{self, Row, Sampler, Snapshot};
use crate::fmt;
use crate::sys::{control, elevation, instance, priority, tray};

/// How long a status-bar message stays visible.
const NOTICE_TTL: Duration = Duration::from_secs(5);
/// Below this a value is just noise, and colouring it distracts.
const NOISE: f64 = 0.05;
const ROW_HEIGHT: f32 = 18.0;
const HEADER_HEIGHT: f32 = 24.0;

const COLOR_HOT: Color32 = Color32::from_rgb(0xff, 0x7a, 0x7a);
const COLOR_WARM: Color32 = Color32::from_rgb(0xff, 0xd1, 0x66);
const COLOR_COOL: Color32 = Color32::from_rgb(0xa5, 0xd6, 0xa7);
const COLOR_INFO: Color32 = Color32::from_rgb(0x80, 0xd8, 0xff);
const COLOR_DANGER: Color32 = Color32::from_rgb(0xff, 0x9a, 0x9a);
const COLOR_SUSPENDED: Color32 = COLOR_WARM;

/// The sortable, hideable columns of the process table.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum Col {
    Name,
    Pid,
    Status,
    Cpu,
    Memory,
    MemoryPercent,
    Gpu,
    Disk,
    Threads,
    CommandLine,
}

impl Col {
    const ALL: [Col; 10] = [
        Col::Name,
        Col::Pid,
        Col::Status,
        Col::Cpu,
        Col::Memory,
        Col::MemoryPercent,
        Col::Gpu,
        Col::Disk,
        Col::Threads,
        Col::CommandLine,
    ];

    fn label(self) -> &'static str {
        match self {
            Col::Name => "Name",
            Col::Pid => "PID",
            Col::Status => "Status",
            Col::Cpu => "CPU",
            Col::Memory => "Memory",
            Col::MemoryPercent => "Memory %",
            Col::Gpu => "GPU",
            Col::Disk => "I/O  read / write",
            Col::Threads => "Threads",
            Col::CommandLine => "Command line",
        }
    }

    /// Columns whose absence would make the table unusable.
    fn required(self) -> bool {
        matches!(self, Col::Name)
    }

    fn default_width(self) -> f32 {
        match self {
            Col::Name => 220.0,
            Col::Pid => 62.0,
            Col::Status => 86.0,
            Col::Cpu => 70.0,
            Col::Memory => 92.0,
            Col::MemoryPercent => 90.0,
            Col::Gpu => 70.0,
            Col::Disk => 164.0,
            Col::Threads => 66.0,
            Col::CommandLine => 260.0,
        }
    }

    fn default_visible(self) -> bool {
        matches!(
            self,
            Col::Name
                | Col::Pid
                | Col::Status
                | Col::Cpu
                | Col::Memory
                | Col::MemoryPercent
                | Col::Gpu
                | Col::Disk
        )
    }

    /// Descending feels right for counts and percentages; names sort up.
    fn wants_descending_on_first_click(self) -> bool {
        !matches!(self, Col::Name | Col::CommandLine)
    }

    fn left_aligned(self) -> bool {
        matches!(self, Col::Name | Col::Status | Col::CommandLine)
    }
}

fn percent(part: u64, total: u64) -> f64 {
    if total == 0 {
        0.0
    } else {
        part as f64 / total as f64 * 100.0
    }
}

/// Case-insensitive comparison that does not allocate.
fn cmp_ignore_case(lhs: &str, rhs: &str) -> Ordering {
    lhs.chars()
        .flat_map(char::to_lowercase)
        .cmp(rhs.chars().flat_map(char::to_lowercase))
}

fn compare_rows(lhs: &Row, rhs: &Row, by: Col) -> Ordering {
    let ordering = match by {
        Col::Name => cmp_ignore_case(&lhs.info.name, &rhs.info.name),
        Col::Pid => lhs.pid.cmp(&rhs.pid),
        Col::Status => lhs.suspended.cmp(&rhs.suspended),
        Col::Cpu => lhs.cpu.total_cmp(&rhs.cpu),
        // Same total for every row, so the percentage sorts like the bytes.
        Col::Memory | Col::MemoryPercent => lhs.memory.cmp(&rhs.memory),
        Col::Gpu => lhs.gpu.total_cmp(&rhs.gpu),
        Col::Disk => (lhs.read_bps + lhs.write_bps).total_cmp(&(rhs.read_bps + rhs.write_bps)),
        Col::Threads => lhs.threads.cmp(&rhs.threads),
        Col::CommandLine => cmp_ignore_case(&lhs.info.command_line, &rhs.info.command_line),
    };
    // Deterministic order when values tie.
    ordering.then_with(|| lhs.pid.cmp(&rhs.pid))
}

/// Processes Windows keeps a close eye on; warn before shutting them down.
fn is_critical_name(name: &str) -> bool {
    const CRITICAL: [&str; 6] = [
        "csrss.exe",
        "wininit.exe",
        "winlogon.exe",
        "services.exe",
        "lsass.exe",
        "smss.exe",
    ];
    CRITICAL
        .iter()
        .any(|candidate| name.eq_ignore_ascii_case(candidate))
}

/// Machine-wide context a cell needs for its text and tooltips.
#[derive(Clone, Copy)]
struct TableContext {
    total_memory: u64,
}

/// A cell's text plus optional colouring and a longer tooltip.
struct Cell {
    text: String,
    colour: Option<Color32>,
    tooltip: Option<String>,
}

fn cell_content(row: &Row, column: Col, ctx: TableContext) -> Cell {
    let plain = Cell {
        text: String::new(),
        colour: None,
        tooltip: None,
    };
    match column {
        Col::Pid => Cell {
            text: row.pid.to_string(),
            ..plain
        },
        Col::Cpu => Cell {
            text: fmt::pct(row.cpu, 1),
            colour: (row.cpu > NOISE).then(|| heat(row.cpu / 100.0)),
            ..plain
        },
        Col::Memory => Cell {
            text: fmt::bytes(row.memory),
            ..plain
        },
        Col::MemoryPercent => Cell {
            text: fmt::pct(percent(row.memory, ctx.total_memory), 2),
            ..plain
        },
        Col::Gpu => Cell {
            text: fmt::pct(row.gpu, 0),
            colour: (row.gpu > NOISE).then_some(COLOR_INFO),
            ..plain
        },
        Col::Disk => Cell {
            text: format!("{} / {}", fmt::rate(row.read_bps), fmt::rate(row.write_bps)),
            colour: ((row.read_bps + row.write_bps) > 128.0 * 1024.0).then_some(COLOR_COOL),
            tooltip: Some(format!(
                "Read {}\nWritten {}",
                fmt::bytes(row.total_read),
                fmt::bytes(row.total_written)
            )),
        },
        Col::Threads => Cell {
            text: row.threads.to_string(),
            ..plain
        },
        Col::CommandLine => Cell {
            text: row.info.command_line.clone(),
            ..plain
        },
        // Name and Status are rendered by the table itself.
        Col::Name | Col::Status => plain,
    }
}

fn heat(fraction: f64) -> Color32 {
    let fraction = fraction.clamp(0.0, 1.0);
    if fraction > 0.7 {
        COLOR_HOT
    } else if fraction > 0.3 {
        COLOR_WARM
    } else if fraction > NOISE {
        COLOR_COOL
    } else {
        Color32::GRAY
    }
}

/// What the user asked us to do while interacting with a row.
enum RowAction {
    Select(usize),
    Toggle(usize),
    AddRange(usize, usize),
    SelectAll,
    Kill,
    Suspend,
    Resume,
    Reveal,
    CopyDetail(usize),
}

#[derive(Clone, Copy, PartialEq, Eq)]
enum Page {
    Processes,
    Performance,
}

/// Whether the elevated logon task is registered. Querying and changing it
/// spawns `schtasks`, so it happens off the UI thread.
#[derive(Clone, Copy, PartialEq, Eq)]
enum TaskStatus {
    Checking,
    Installed,
    Missing,
}

struct StartupTask {
    status: TaskStatus,
    /// A result to show in the status bar, once.
    message: Option<String>,
}

pub struct TaskyApp {
    sampler: Sampler,
    /// PIDs we suspended, shared with the sampler so both agree on labelling.
    suspended: Arc<Mutex<HashSet<u32>>>,

    snapshot: Arc<Snapshot>,
    last_sequence: u64,
    /// Row indices into `snapshot.rows`, after filtering and sorting.
    visible: Vec<usize>,

    filter: String,
    search_focused: bool,
    sort_by: Col,
    sort_descending: bool,
    columns: [bool; Col::ALL.len()],
    refresh_interval: Duration,
    settings_open: bool,

    selected: BTreeSet<u32>,
    last_clicked: Option<usize>,
    page: Page,
    theme: ThemePreference,

    tray: Option<tray::Tray>,
    /// Hidden into the tray; eframe does not report that itself.
    hidden: bool,
    /// The bound hotkey's label, if one could be registered.
    hotkey: Option<&'static str>,
    elevated: bool,
    startup: Arc<Mutex<StartupTask>>,
    /// Whether the working set has been pinned yet.
    pinned: bool,

    notice: Option<(String, Instant)>,
    confirm_kill: Option<Vec<u32>>,
    search_needs_focus: bool,
    clipboard: Option<String>,
}

impl TaskyApp {
    /// `background`: started hidden in the tray (by the logon task).
    pub fn new(
        cc: &eframe::CreationContext<'_>,
        sampler: Sampler,
        suspended: Arc<Mutex<HashSet<u32>>>,
        background: bool,
    ) -> Self {
        let ctx = cc.egui_ctx.clone();

        if let Ok(handle) = cc.window_handle() {
            if let RawWindowHandle::Win32(win) = handle.as_raw() {
                instance::set_main_window(windows::Win32::Foundation::HWND(
                    win.hwnd.get() as *mut _
                ));
            }
        }
        let repaint = ctx.clone();
        instance::on_show(move || repaint.request_repaint());

        let repaint = ctx.clone();
        sampler.set_on_update(move || repaint.request_repaint());
        sampler.set_active(!background);

        // The tray's "Exit" ends the process from inside its menu handler, so
        // it needs its own way to thaw anything we froze.
        let to_resume = Arc::clone(&suspended);
        let tray = tray::Tray::new(move || control::resume_all(&mut lock(&to_resume)));

        let startup = Arc::new(Mutex::new(StartupTask {
            status: TaskStatus::Checking,
            message: None,
        }));
        refresh_task_status(&startup, &ctx);

        let theme = ThemePreference::System;
        ctx.set_theme(theme);
        ctx.all_styles_mut(|style| {
            style.spacing.item_spacing = egui::vec2(8.0, 5.0);
            style.spacing.button_padding = egui::vec2(6.0, 3.0);
        });

        let mut columns = [false; Col::ALL.len()];
        for (position, column) in Col::ALL.iter().enumerate() {
            columns[position] = column.default_visible();
        }

        let app = Self {
            refresh_interval: collector::SAMPLE_INTERVAL,
            sampler,
            suspended,
            last_sequence: 0,
            snapshot: Arc::new(Snapshot::default()),
            visible: Vec::new(),
            filter: String::new(),
            search_focused: false,
            sort_by: Col::Cpu,
            sort_descending: true,
            columns,
            selected: BTreeSet::new(),
            last_clicked: None,
            tray,
            hidden: background,
            hotkey: instance::hotkey_label(),
            elevated: elevation::is_elevated(),
            startup,
            pinned: false,
            notice: None,
            confirm_kill: None,
            search_needs_focus: true,
            settings_open: false,
            page: Page::Processes,
            theme,
            clipboard: None,
        };
        app.sync_gpu_collection();
        app
    }

    // ------------------------------------------------------------------ sampling

    fn pull_snapshot(&mut self) {
        let Some(next) = self.sampler.latest() else {
            return;
        };
        if next.sequence == self.last_sequence {
            return;
        }
        self.last_sequence = next.sequence;
        self.snapshot = next;
        self.rebuild_visible();

        // Processes we had selected can vanish at any time.
        let live: HashSet<u32> = self.snapshot.rows.iter().map(|row| row.pid).collect();
        self.selected.retain(|pid| live.contains(pid));
    }

    fn rebuild_visible(&mut self) {
        let needle = self.filter.trim().to_lowercase();
        let rows = &self.snapshot.rows;

        // Filter first, so a narrow search only sorts what it shows.
        self.visible.clear();
        self.visible
            .extend((0..rows.len()).filter(|&index| rows[index].matches(&needle)));
        let (by, descending) = (self.sort_by, self.sort_descending);
        self.visible.sort_unstable_by(|&a, &b| {
            let ordering = compare_rows(&rows[a], &rows[b], by);
            if descending {
                ordering.reverse()
            } else {
                ordering
            }
        });
    }

    fn row_at(&self, index: usize) -> Option<&Row> {
        self.visible.get(index).map(|&row| &self.snapshot.rows[row])
    }

    // ------------------------------------------------------------------ notices

    fn notify(&mut self, message: impl Into<String>) {
        self.notice = Some((message.into(), Instant::now() + NOTICE_TTL));
    }

    fn current_notice(&mut self) -> Option<&str> {
        let expired = self
            .notice
            .as_ref()
            .map(|(_, until)| *until < Instant::now())
            .unwrap_or(true);
        if expired {
            self.notice = None;
            None
        } else {
            self.notice.as_ref().map(|(message, _)| message.as_str())
        }
    }

    // ------------------------------------------------------------ system glue

    /// Bring eframe's view of the window in line after something outside it
    /// (hotkey, tray, second launch) has already shown it natively.
    fn on_shown(&mut self, ctx: &egui::Context) {
        ctx.send_viewport_cmd(ViewportCommand::Visible(true));
        ctx.send_viewport_cmd(ViewportCommand::Minimized(false));
        ctx.send_viewport_cmd(ViewportCommand::Focus);
        self.search_needs_focus = true;
        self.hidden = false;
    }

    fn hide_to_tray(&mut self, ctx: &egui::Context) {
        ctx.send_viewport_cmd(ViewportCommand::CancelClose);
        ctx.send_viewport_cmd(ViewportCommand::Visible(false));
        self.hidden = true;
    }

    fn set_always_on_top(&self, on: bool) {
        instance::set_always_on_top(on);
        if let Some(tray) = &self.tray {
            tray.set_topmost_checked(on);
        }
    }

    fn column_visible(&self, column: Col) -> bool {
        column.required() || self.columns[Col::ALL.iter().position(|c| *c == column).unwrap()]
    }

    fn sync_gpu_collection(&self) {
        self.sampler.set_collect_gpu(self.column_visible(Col::Gpu));
    }

    // ------------------------------------------------------------------ actions

    fn selected_pids(&self) -> Vec<u32> {
        self.selected.iter().copied().collect()
    }

    fn selected_rows(&self) -> impl Iterator<Item = &Row> {
        self.snapshot
            .rows
            .iter()
            .filter(|row| self.selected.contains(&row.pid))
    }

    fn is_suspended(&self, pid: u32) -> bool {
        self.suspended.lock().unwrap().contains(&pid)
    }

    fn request_kill(&mut self) {
        let pids = self.selected_pids();
        if pids.is_empty() {
            return;
        }
        // Ending a whole selection, or a process Windows guards, deserves a
        // confirmation; a single ordinary process does not.
        if pids.len() > 1 || self.selection_is_critical() {
            self.confirm_kill = Some(pids);
        } else {
            self.do_kill(pids);
        }
    }

    fn selection_is_critical(&self) -> bool {
        self.selected_rows()
            .any(|row| row.pid <= 8 || row.ppid == 0 || is_critical_name(&row.info.name))
    }

    fn do_kill(&mut self, pids: Vec<u32>) {
        let me = std::process::id();
        let targets: Vec<u32> = pids
            .into_iter()
            .filter(|pid| *pid != me && *pid > 4)
            .collect();

        let errors = control::kill(&targets);
        self.selected.clear();
        self.confirm_kill = None;

        if errors.is_empty() {
            self.notify(format!(
                "Requested termination for {} process(es)",
                targets.len()
            ));
        } else {
            self.notify(format!(
                "Could not end pid {}: {}",
                errors[0].pid, errors[0].message
            ));
        }
    }

    /// `None` toggles against the current state; `Some(true)`/`Some(false)`
    /// forces a direction.
    fn set_suspended(&mut self, force: Option<bool>) {
        let pids = self.selected_pids();
        if pids.is_empty() {
            return;
        }
        let any_suspended = pids.iter().any(|pid| self.is_suspended(*pid));
        let suspend = force.unwrap_or(!any_suspended);

        let mut lock = self.suspended.lock().unwrap();
        let mut succeeded = 0;
        let mut failed = 0;
        for pid in pids {
            // Suspension nests: suspending twice needs two resumes, and we
            // only ever resume once.
            if suspend && lock.contains(&pid) {
                continue;
            }
            let outcome = if suspend {
                control::suspend(pid)
            } else {
                control::resume(pid)
            };
            match outcome {
                Ok(()) => {
                    succeeded += 1;
                    if suspend {
                        lock.insert(pid);
                    } else {
                        lock.remove(&pid);
                    }
                }
                Err(_) => failed += 1,
            }
        }

        drop(lock);

        let mut message = format!(
            "{} {succeeded} process(es)",
            if suspend { "Suspended" } else { "Resumed" }
        );
        if failed > 0 {
            message.push_str(&format!(", {failed} refused"));
        }
        self.notify(message);
    }

    fn suspend_label(&self) -> &'static str {
        if self.selected.iter().any(|pid| self.is_suspended(*pid)) {
            "Resume"
        } else {
            "Suspend"
        }
    }

    fn reveal_selection(&mut self) {
        let opened = self
            .selected_rows()
            .filter(|row| control::reveal_in_explorer(&row.info.executable))
            .count();
        if opened == 0 {
            self.notify("Could not open any of the selected executables");
        }
    }

    fn copy_detail(&mut self, row: &Row) {
        let info = &row.info;
        self.clipboard = Some(if info.executable.is_empty() {
            format!("{} (PID {})", info.name, row.pid)
        } else {
            format!("{}  {}  PID {}", info.name, info.executable, row.pid)
        });
    }

    // ------------------------------------------------------------ row behaviour

    fn apply_row_actions(&mut self, actions: Vec<RowAction>) {
        for action in actions {
            match action {
                RowAction::Select(index) => {
                    let pid = self.row_at(index).map(|row| row.pid);
                    self.selected.clear();
                    if let Some(pid) = pid {
                        self.selected.insert(pid);
                    }
                }
                RowAction::Toggle(index) => {
                    if let Some(pid) = self.row_at(index).map(|row| row.pid) {
                        if !self.selected.remove(&pid) {
                            self.selected.insert(pid);
                        }
                    }
                }
                RowAction::AddRange(from, to) => {
                    let (low, high) = if from <= to { (from, to) } else { (to, from) };
                    for index in low..=high {
                        if let Some(pid) = self.row_at(index).map(|row| row.pid) {
                            self.selected.insert(pid);
                        }
                    }
                }
                RowAction::SelectAll => {
                    let pids: Vec<u32> = self
                        .visible
                        .iter()
                        .map(|&index| self.snapshot.rows[index].pid)
                        .collect();
                    self.selected.clear();
                    self.selected.extend(pids);
                }
                RowAction::Kill => self.request_kill(),
                RowAction::Suspend => self.set_suspended(Some(true)),
                RowAction::Resume => self.set_suspended(Some(false)),
                RowAction::Reveal => self.reveal_selection(),
                RowAction::CopyDetail(index) => {
                    if let Some(row) = self.row_at(index) {
                        let row = row.clone();
                        self.copy_detail(&row);
                    }
                }
            }
        }
    }

    // -------------------------------------------------------------------- panels

    fn toolbar(&mut self, ctx: &egui::Context) {
        let mut always_on_top = instance::is_always_on_top();
        let mut topmost_clicked = false;
        egui::TopBottomPanel::top("toolbar")
            .frame(
                egui::Frame::new()
                    .fill(ctx.style().visuals.faint_bg_color)
                    .inner_margin(Margin::symmetric(10, 6)),
            )
            .show(ctx, |ui| {
                ui.horizontal_wrapped(|ui| {
                    ui.label(RichText::new("Tasky").strong().size(17.0));
                    ui.separator();

                    let search = ui.add(
                        egui::TextEdit::singleline(&mut self.filter)
                            .hint_text("Search name, path or PID")
                            .desired_width(210.0)
                            .id_source("tasky-search"),
                    );
                    self.search_focused = search.has_focus();
                    if search.changed() {
                        self.rebuild_visible();
                    }
                    if self.search_needs_focus {
                        search.request_focus();
                        self.search_needs_focus = false;
                    }

                    ui.separator();

                    let count = self.selected.len();
                    ui.add_enabled_ui(count > 0, |ui| {
                        ui.label(
                            RichText::new(format!("{count} selected"))
                                .small()
                                .color(ui.visuals().weak_text_color()),
                        );

                        if ui
                            .button(RichText::new("End task").color(COLOR_DANGER))
                            .on_hover_text("Terminate the selection  [Delete]")
                            .clicked()
                        {
                            self.request_kill();
                        }
                        if ui
                            .button(self.suspend_label())
                            .on_hover_text("Freeze or unfreeze the selection")
                            .clicked()
                        {
                            self.set_suspended(None);
                        }
                        if ui
                            .button("Open file location")
                            .on_hover_text("Reveal the executable in Explorer")
                            .clicked()
                        {
                            self.reveal_selection();
                        }
                    });

                    ui.separator();

                    if ui
                        .toggle_value(&mut always_on_top, "Always on top")
                        .clicked()
                    {
                        topmost_clicked = true;
                    }

                    if ui
                        .selectable_label(self.page == Page::Processes, "Processes")
                        .clicked()
                    {
                        self.page = Page::Processes;
                    }

                    if ui
                        .selectable_label(self.page == Page::Performance, "Performance")
                        .clicked()
                    {
                        self.page = Page::Performance;
                    }

                    if ui
                        .button("Settings")
                        .on_hover_text("Refresh rate and visible columns")
                        .clicked()
                    {
                        self.settings_open = !self.settings_open;
                    }
                });
            });

        if topmost_clicked {
            self.set_always_on_top(always_on_top);
        }
    }

    fn performance_page(&self, ctx: &egui::Context) {
        let totals = &self.snapshot.totals;
        egui::CentralPanel::default().show(ctx, |ui| {
            ui.label(RichText::new("Per-core CPU usage").strong().size(16.0));
            ui.separator();
            egui::ScrollArea::vertical().show(ui, |ui| {
                for (index, core) in totals.per_core_cpu.iter().enumerate() {
                    ui.horizontal(|ui| {
                        ui.label(format!("Core {index:>2}"));
                        ui.add(
                            egui::ProgressBar::new((core / 100.0).clamp(0.0, 1.0) as f32)
                                .desired_width(260.0)
                                .desired_height(12.0)
                                .show_percentage(),
                        );
                    });
                }
                ui.separator();
                ui.label(format!(
                    "Total CPU: {:.1}%  ({} logical processors)",
                    totals.cpu,
                    totals.per_core_cpu.len()
                ));
                ui.label(format!(
                    "RAM: {} / {}",
                    fmt::bytes(totals.memory_used),
                    fmt::bytes(totals.memory_total)
                ));
            });
        });
    }

    fn process_table(&mut self, ctx: &egui::Context) {
        let snapshot = Arc::clone(&self.snapshot);
        let visible = std::mem::take(&mut self.visible);
        let columns = self.columns;
        let sort_by = self.sort_by;
        let table_ctx = TableContext {
            total_memory: snapshot.totals.memory_total,
        };
        let selected = &self.selected;

        let mut header_change: Option<Col> = None;
        let mut header_descending = self.sort_descending;
        let mut actions: Vec<RowAction> = Vec::new();
        let mut last_clicked = self.last_clicked;

        egui::CentralPanel::default().show(ctx, |ui| {
            let mut builder = TableBuilder::new(ui)
                .striped(true)
                .resizable(true)
                .vscroll(true)
                .sense(Sense::click())
                .cell_layout(egui::Layout::left_to_right(Align::Center));
            for (position, column) in Col::ALL.iter().enumerate() {
                if !columns[position] && !column.required() {
                    continue;
                }
                builder = builder.column(
                    Column::initial(column.default_width())
                        .at_least(34.0)
                        .resizable(true),
                );
            }
            let table = builder.header(HEADER_HEIGHT, |mut header| {
                for (position, column) in Col::ALL.iter().enumerate() {
                    if !columns[position] && !column.required() {
                        continue;
                    }
                    header.col(|ui| {
                        let active = sort_by == *column;
                        let marker = match (active, header_descending) {
                            (true, true) => " v",
                            (true, false) => " ^",
                            (false, _) => "",
                        };
                        let colour = if active {
                            ui.visuals().strong_text_color()
                        } else {
                            ui.visuals().text_color()
                        };
                        let label =
                            RichText::new(format!("{}{marker}", column.label())).color(colour);
                        if ui
                            .add(egui::Label::new(label).sense(Sense::click()))
                            .clicked()
                        {
                            if active {
                                header_descending = !header_descending;
                            } else {
                                header_change = Some(*column);
                                header_descending = column.wants_descending_on_first_click();
                            }
                        }
                    });
                }
            });

            table.body(|body| {
                body.rows(ROW_HEIGHT, visible.len(), |mut row| {
                    let data = &snapshot.rows[visible[row.index()]];
                    let pid = data.pid;
                    row.set_selected(selected.contains(&pid));

                    for (position, column) in Col::ALL.iter().enumerate() {
                        if !columns[position] && !column.required() {
                            continue;
                        }
                        row.col(|ui| match column {
                            Col::Name => {
                                let colour = if data.suspended {
                                    COLOR_SUSPENDED
                                } else {
                                    ui.visuals().text_color()
                                };
                                let response = ui.add(
                                    egui::Label::new(RichText::new(&data.info.name).color(colour))
                                        .truncate(),
                                );
                                if !data.info.executable.is_empty() {
                                    response.on_hover_text(format!(
                                        "{}\nPID {pid}  ·  parent PID {}",
                                        data.info.executable, data.ppid
                                    ));
                                }
                            }
                            Col::Status => {
                                let label = if data.suspended {
                                    RichText::new("Suspended").color(COLOR_SUSPENDED).strong()
                                } else {
                                    RichText::new("Running").weak()
                                };
                                ui.label(label);
                            }
                            other => {
                                let cell = cell_content(data, *other, table_ctx);
                                let align = if other.left_aligned() {
                                    egui::Layout::left_to_right(Align::Center)
                                } else {
                                    egui::Layout::right_to_left(Align::Center)
                                };
                                ui.with_layout(align, |ui| {
                                    let text = match cell.colour {
                                        Some(colour) => RichText::new(cell.text).color(colour),
                                        None => RichText::new(cell.text),
                                    };
                                    let response = ui.add(egui::Label::new(text).truncate());
                                    if let Some(tooltip) = cell.tooltip {
                                        response.on_hover_text(tooltip);
                                    }
                                });
                            }
                        });
                    }

                    let response = row.response();
                    let index = row.index();
                    if response.clicked() {
                        let modifiers = ctx.input(|i| i.modifiers);
                        if modifiers.ctrl {
                            actions.push(RowAction::Toggle(index));
                        } else if modifiers.shift {
                            match last_clicked {
                                Some(anchor) => actions.push(RowAction::AddRange(anchor, index)),
                                None => actions.push(RowAction::Select(index)),
                            }
                        } else {
                            actions.push(RowAction::Select(index));
                        }
                        last_clicked = Some(index);
                    }
                    if response.double_clicked() {
                        actions.push(RowAction::Reveal);
                    }

                    response.context_menu(|ui| {
                        if ui.button("End task").clicked() {
                            actions.push(RowAction::Kill);
                            ui.close();
                        }
                        if ui.button("Suspend").clicked() {
                            actions.push(RowAction::Suspend);
                            ui.close();
                        }
                        if ui.button("Resume").clicked() {
                            actions.push(RowAction::Resume);
                            ui.close();
                        }
                        ui.separator();
                        if ui.button("Open file location").clicked() {
                            actions.push(RowAction::Reveal);
                            ui.close();
                        }
                        if ui.button("Copy process details").clicked() {
                            actions.push(RowAction::CopyDetail(index));
                            ui.close();
                        }
                        if ui.button("Select all").clicked() {
                            actions.push(RowAction::SelectAll);
                            ui.close();
                        }
                    });
                });
            });
        });

        self.visible = visible;
        self.last_clicked = last_clicked;
        self.apply_row_actions(actions);

        if let Some(column) = header_change {
            self.sort_by = column;
            self.sort_descending = header_descending;
            self.rebuild_visible();
        } else if header_descending != self.sort_descending {
            self.sort_descending = header_descending;
            self.rebuild_visible();
        }
    }

    fn status_bar(&mut self, ctx: &egui::Context) {
        let totals = self.snapshot.totals.clone();
        let gpu_available = self.snapshot.gpu_available;
        let notice = self.current_notice().map(|text| text.to_string());
        let hotkey = self.hotkey.unwrap_or("unavailable");

        egui::TopBottomPanel::bottom("status")
            .frame(
                egui::Frame::new()
                    .fill(ctx.style().visuals.faint_bg_color)
                    .inner_margin(Margin::symmetric(10, 4)),
            )
            .show(ctx, |ui| {
                ui.horizontal(|ui| {
                    ui.label(RichText::new(format!("{} processes", totals.processes)).weak());

                    ui.separator();
                    meter(ui, "CPU", totals.cpu / 100.0);
                    meter(ui, "Memory", totals.memory_percent() / 100.0);
                    // Only shown while GPU usage is being collected.
                    if gpu_available == Some(true) {
                        meter(ui, "GPU", totals.gpu / 100.0);
                    }

                    ui.separator();
                    ui.label(
                        RichText::new(format!(
                            "RAM {} / {}",
                            fmt::bytes(totals.memory_used),
                            fmt::bytes(totals.memory_total)
                        ))
                        .weak(),
                    );
                    ui.label(
                        RichText::new(format!(
                            "I/O ↓ {}  ↑ {}",
                            fmt::rate(totals.read_bps),
                            fmt::rate(totals.write_bps)
                        ))
                        .weak(),
                    );

                    ui.with_layout(egui::Layout::right_to_left(Align::Center), |ui| {
                        if let Some(notice) = notice {
                            ui.label(RichText::new(notice).color(COLOR_WARM));
                        } else if gpu_available == Some(false) {
                            ui.label(RichText::new("GPU counters unavailable").small().weak());
                        } else {
                            ui.label(RichText::new(format!("hotkey {hotkey}")).small().weak());
                        }
                    });
                });
            });
    }

    fn permissions_settings(&mut self, ui: &mut egui::Ui) {
        ui.label("Permissions");
        if self.elevated {
            ui.label(RichText::new("Running as administrator.").weak());
        } else {
            ui.label(
                RichText::new(
                    "Not running as administrator: processes of other users and services \
                     cannot be ended or suspended.",
                )
                .color(COLOR_WARM),
            );
            if ui.button("Restart as administrator").clicked() {
                self.restart_elevated();
            }
        }

        let status = lock(&self.startup).status;
        let mut installed = status == TaskStatus::Installed;
        let toggle = ui.add_enabled(
            status != TaskStatus::Checking,
            egui::Checkbox::new(&mut installed, "Start with Windows, as administrator"),
        );
        let toggle = toggle.on_hover_text(
            "Registers a logon task that starts Tasky elevated in the tray, with no UAC \
             prompt. Launching Tasky normally then uses that task too.",
        );
        if toggle.clicked() {
            change_task(&self.startup, ui.ctx(), installed, self.elevated);
        }
    }

    /// Start an elevated copy that takes over once this one has exited.
    fn restart_elevated(&mut self) {
        let Ok(exe) = std::env::current_exe() else {
            return;
        };
        let spawned = std::process::Command::new(exe)
            .args(["--wait-for", &std::process::id().to_string(), "--elevate"])
            .spawn();
        match spawned {
            Ok(_) => {
                control::resume_all(&mut lock(&self.suspended));
                std::process::exit(0);
            }
            Err(error) => self.notify(format!("Could not restart: {error}")),
        }
    }

    fn settings(&mut self, ctx: &egui::Context) {
        let mut open = self.settings_open;
        let mut columns_changed = false;
        egui::Window::new("Settings")
            .open(&mut open)
            .default_width(280.0)
            .resizable(false)
            .show(ctx, |ui| {
                ui.label("Refresh rate");
                for (label, value) in [
                    ("0.5 s", Duration::from_millis(500)),
                    ("1 s", collector::SAMPLE_INTERVAL),
                    ("2 s", Duration::from_millis(2000)),
                    ("5 s", Duration::from_millis(5000)),
                ] {
                    if ui
                        .selectable_label(self.refresh_interval == value, label)
                        .clicked()
                    {
                        self.refresh_interval = value;
                        self.sampler.set_interval(value);
                    }
                }

                ui.separator();
                ui.label("Theme");
                for (label, value) in [
                    ("System", ThemePreference::System),
                    ("Dark", ThemePreference::Dark),
                    ("Light", ThemePreference::Light),
                ] {
                    if ui.selectable_label(self.theme == value, label).clicked() {
                        self.theme = value;
                        ctx.set_theme(value);
                    }
                }

                ui.separator();
                ui.label("Columns");
                for (position, column) in Col::ALL.iter().enumerate() {
                    let mut visible = self.columns[position];
                    if !column.required() && ui.checkbox(&mut visible, column.label()).clicked() {
                        self.columns[position] = visible;
                        columns_changed = true;
                    }
                }

                ui.separator();
                self.permissions_settings(ui);

                ui.separator();
                if self.tray.is_some() {
                    ui.label("Closing the window keeps Tasky running in the tray.");
                    if let Some(hotkey) = self.hotkey {
                        ui.label(format!("Press the hotkey ({hotkey}) to summon it back."));
                    }
                } else {
                    ui.label("Closing the window quits Tasky.");
                }
            });
        self.settings_open = open;
        if columns_changed {
            self.sync_gpu_collection();
        }
    }

    fn confirm_kill_dialog(&mut self, ctx: &egui::Context) {
        let Some(pids) = self.confirm_kill.clone() else {
            return;
        };
        let rows: Vec<Row> = self.selected_rows().cloned().collect();
        let mut keep_open = true;

        egui::Window::new("End task")
            .open(&mut keep_open)
            .collapsible(false)
            .resizable(false)
            .anchor(egui::Align2::CENTER_CENTER, [0.0, 0.0])
            .show(ctx, |ui| {
                ui.label(format!(
                    "Terminate {} process(es)? Unsaved data will be lost.",
                    pids.len()
                ));
                egui::ScrollArea::vertical()
                    .max_height(140.0)
                    .show(ui, |ui| {
                        for row in rows.iter().take(12) {
                            ui.label(format!("{}  (PID {})", row.info.name, row.pid));
                        }
                        if rows.len() > 12 {
                            ui.label(
                                RichText::new(format!("…and {} more", rows.len() - 12)).weak(),
                            );
                        }
                    });
                ui.separator();
                ui.horizontal(|ui| {
                    if ui.button("Cancel").clicked() {
                        self.confirm_kill = None;
                    }
                    if ui
                        .button(RichText::new("End task").color(COLOR_DANGER))
                        .clicked()
                    {
                        self.do_kill(pids.clone());
                    }
                });
            });

        if !keep_open {
            self.confirm_kill = None;
        }
    }

    fn handle_keys(&mut self, ctx: &egui::Context) {
        // Let the search box keep its normal Delete and Escape behaviour.
        if self.search_focused {
            return;
        }

        let delete = ctx.input(|input| input.key_pressed(egui::Key::Delete));
        let escape = ctx.input(|input| input.key_pressed(egui::Key::Escape));
        let ctrl = ctx.input(|input| input.modifiers.ctrl);
        let select_all = ctrl && ctx.input(|input| input.key_pressed(egui::Key::A));

        if delete && !self.selected.is_empty() {
            self.request_kill();
        } else if select_all {
            self.apply_row_actions(vec![RowAction::SelectAll]);
        } else if escape {
            if self.selected.is_empty() && !self.filter.is_empty() {
                self.filter.clear();
                self.rebuild_visible();
            } else {
                self.selected.clear();
            }
        }
    }
}

/// A small labelled gauge for the status bar.
fn meter(ui: &mut egui::Ui, label: &str, fraction: f64) {
    ui.horizontal(|ui| {
        ui.label(RichText::new(label).weak());
        ui.add(
            egui::ProgressBar::new(fraction.clamp(0.0, 1.0) as f32)
                .desired_width(60.0)
                .desired_height(9.0),
        );
        ui.label(
            RichText::new(format!("{:.0}%", fraction * 100.0))
                .weak()
                .small(),
        );
    });
}

impl App for TaskyApp {
    fn update(&mut self, ctx: &egui::Context, _frame: &mut eframe::Frame) {
        if instance::take_show_request() {
            self.on_shown(ctx);
        }
        // Nobody can see a hidden or minimized window; stop sampling until
        // it is back.
        let minimized = ctx.input(|input| input.viewport().minimized.unwrap_or(false));
        self.sampler.set_active(!self.hidden && !minimized);

        self.pull_snapshot();
        self.handle_keys(ctx);

        // Once the window has drawn real data a few times, the pages it needs
        // are resident; keep them that way.
        if !self.pinned && self.snapshot.sequence >= 3 {
            priority::pin_working_set();
            self.pinned = true;
        }
        let message = lock(&self.startup).message.take();
        if let Some(message) = message {
            self.notify(message);
        }

        self.toolbar(ctx);
        self.status_bar(ctx);
        match self.page {
            Page::Processes => self.process_table(ctx),
            Page::Performance => self.performance_page(ctx),
        }

        if self.settings_open {
            self.settings(ctx);
        }
        self.confirm_kill_dialog(ctx);

        if let Some(text) = self.clipboard.take() {
            ctx.copy_text(text);
        }

        // Closing the window hides it into the tray rather than quitting,
        // which is what "always available" means in practice.
        if ctx.input(|input| input.viewport().close_requested()) && self.tray.is_some() {
            self.hide_to_tray(ctx);
            self.sampler.set_active(false);
        }

        // The sampler requests a repaint for each new snapshot; the only
        // other timed change is a status notice running out.
        if let Some((_, until)) = &self.notice {
            ctx.request_repaint_after(until.saturating_duration_since(Instant::now()));
        }
    }

    fn on_exit(&mut self, _gl: Option<&eframe::glow::Context>) {
        control::resume_all(&mut lock(&self.suspended));
    }
}

fn refresh_task_status(startup: &Arc<Mutex<StartupTask>>, ctx: &egui::Context) {
    let (startup, ctx) = (Arc::clone(startup), ctx.clone());
    std::thread::spawn(move || {
        let status = if elevation::task_installed() {
            TaskStatus::Installed
        } else {
            TaskStatus::Missing
        };
        lock(&startup).status = status;
        ctx.request_repaint();
    });
}

/// Register or remove the logon task, asking for elevation when needed.
fn change_task(
    startup: &Arc<Mutex<StartupTask>>,
    ctx: &egui::Context,
    install: bool,
    elevated: bool,
) {
    lock(startup).status = TaskStatus::Checking;
    let (startup, ctx) = (Arc::clone(startup), ctx.clone());
    std::thread::spawn(move || {
        let (action, flag): (fn() -> Result<(), String>, _) = if install {
            (elevation::install_task, "--install-task")
        } else {
            (elevation::uninstall_task, "--uninstall-task")
        };
        let ok = if elevated {
            action().is_ok()
        } else {
            elevation::run_self_elevated(flag, true)
        };
        let installed = elevation::task_installed();
        let mut startup = lock(&startup);
        startup.status = if installed {
            TaskStatus::Installed
        } else {
            TaskStatus::Missing
        };
        startup.message = Some(match (ok, install) {
            (true, true) => "Tasky will start with Windows, as administrator".into(),
            (true, false) => "Tasky will no longer start with Windows".into(),
            (false, _) => "Could not change the startup task".into(),
        });
        drop(startup);
        ctx.request_repaint();
    });
}
