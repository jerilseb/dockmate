//! Application state and the reducer that key presses and daemon events run
//! through.
//!
//! The main loop owns this exclusively; background tasks only ever send
//! [`AppEvent`]s, so there are no locks anywhere near the render path.

use std::collections::{HashMap, VecDeque};
use std::time::{Duration, Instant};

use crossterm::event::{
    KeyCode, KeyEvent, KeyEventKind, KeyModifiers, MouseButton, MouseEvent, MouseEventKind,
};
use ratatui::layout::{Position, Rect};
use tokio::sync::mpsc;
use tokio::task::JoinHandle;

use crate::action::{Command, resolve};
use crate::docker::actions::{self, Job};
use crate::docker::logs::{self, LogLine};
use crate::docker::model::{ContainerRow, DaemonInfo, ImageRow, NetworkRow, State, VolumeRow};
use crate::docker::refresh::Refresher;
use crate::docker::stats::{StatSample, StatsManager};
use crate::docker::{Client, model};
use crate::event::AppEvent;
use crate::ui::theme::{Symbols, Theme};
use crate::util::{clipboard, fuzzy};

/// How long a toast stays on screen.
const TOAST_TTL: Duration = Duration::from_secs(4);
/// Log lines kept for the selected container.
const LOG_CAPACITY: usize = 5_000;
/// Grace period before opening a log stream, so holding ↓ doesn't open one per
/// row it passes through.
const LOG_DEBOUNCE: Duration = Duration::from_millis(250);

// ---------------------------------------------------------------------------
// Small pieces
// ---------------------------------------------------------------------------

#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum Tab {
    Containers,
    Images,
    Volumes,
    Networks,
}

impl Tab {
    pub const ALL: [Tab; 4] = [Tab::Containers, Tab::Images, Tab::Volumes, Tab::Networks];

    pub fn index(self) -> usize {
        match self {
            Tab::Containers => 0,
            Tab::Images => 1,
            Tab::Volumes => 2,
            Tab::Networks => 3,
        }
    }

    pub fn title(self) -> &'static str {
        match self {
            Tab::Containers => "Containers",
            Tab::Images => "Images",
            Tab::Volumes => "Volumes",
            Tab::Networks => "Networks",
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Focus {
    List,
    Logs,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Mode {
    Normal,
    Filter,
    Palette,
    Confirm,
    Help,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ToastKind {
    Info,
    Success,
    Error,
}

#[derive(Debug, Clone)]
pub struct Toast {
    pub kind: ToastKind,
    pub text: String,
    pub born: Instant,
}

/// A one-line text field with a cursor. Small enough not to warrant a crate.
#[derive(Debug, Default, Clone)]
pub struct Input {
    pub value: String,
    /// Cursor position in *characters*, not bytes.
    pub cursor: usize,
}

impl Input {
    pub fn insert(&mut self, c: char) {
        let byte = self.byte_at(self.cursor);
        self.value.insert(byte, c);
        self.cursor += 1;
    }

    pub fn backspace(&mut self) {
        if self.cursor == 0 {
            return;
        }
        let start = self.byte_at(self.cursor - 1);
        let end = self.byte_at(self.cursor);
        self.value.replace_range(start..end, "");
        self.cursor -= 1;
    }

    pub fn delete(&mut self) {
        let len = self.value.chars().count();
        if self.cursor >= len {
            return;
        }
        let start = self.byte_at(self.cursor);
        let end = self.byte_at(self.cursor + 1);
        self.value.replace_range(start..end, "");
    }

    pub fn left(&mut self) {
        self.cursor = self.cursor.saturating_sub(1);
    }

    pub fn right(&mut self) {
        self.cursor = (self.cursor + 1).min(self.value.chars().count());
    }

    pub fn home(&mut self) {
        self.cursor = 0;
    }

    pub fn end(&mut self) {
        self.cursor = self.value.chars().count();
    }

    pub fn clear(&mut self) {
        self.value.clear();
        self.cursor = 0;
    }

    pub fn is_empty(&self) -> bool {
        self.value.is_empty()
    }

    fn byte_at(&self, char_index: usize) -> usize {
        self.value
            .char_indices()
            .nth(char_index)
            .map(|(i, _)| i)
            .unwrap_or(self.value.len())
    }
}

/// Sort columns, per tab.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Sort {
    Name,
    State,
    Cpu,
    Mem,
    Size,
    Created,
    Driver,
}

impl Sort {
    pub fn label(self) -> &'static str {
        match self {
            Sort::Name => "name",
            Sort::State => "state",
            Sort::Cpu => "cpu",
            Sort::Mem => "mem",
            Sort::Size => "size",
            Sort::Created => "age",
            Sort::Driver => "driver",
        }
    }

    /// Which way this column runs when the sort isn't reversed.
    ///
    /// The comparators aren't uniformly ascending, and they shouldn't be: names
    /// read best from A, but nobody opens a cpu column to look at the idle
    /// containers first. The chevron has to follow that or it would claim
    /// `NAME ▾` counts downwards.
    pub fn descends_by_default(self) -> bool {
        match self {
            Sort::Cpu | Sort::Mem | Sort::Size => true,
            // `Created` sorts newest-first, but the column it marks shows an
            // *age*. Newest is the smallest of those, so the column ascends
            // even though the timestamps behind it descend.
            Sort::Name | Sort::State | Sort::Driver | Sort::Created => false,
        }
    }
}

/// The sort columns offered on each tab, in cycle order.
fn sort_cycle(tab: Tab) -> &'static [Sort] {
    match tab {
        Tab::Containers => &[Sort::Name, Sort::State, Sort::Cpu, Sort::Mem, Sort::Created],
        Tab::Images => &[Sort::Name, Sort::Size, Sort::Created],
        Tab::Volumes => &[Sort::Name, Sort::Size, Sort::Created],
        Tab::Networks => &[Sort::Name, Sort::Driver, Sort::Created],
    }
}

/// One row of a rendered list.
///
/// Grouping is the only reason this isn't just an index: a header has to occupy
/// a row of its own so it can be scrolled to, selected and clicked like any
/// other. Ungrouped tabs only ever emit `Item`.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ViewRow {
    /// Index into [`App::groups`].
    Group(usize),
    /// Index into the tab's resource vector.
    Item(usize),
}

/// A stack, as summarised on its header row.
pub struct Group {
    /// The stack name, or empty for the bucket that collects containers no stack
    /// claims. Doubles as the key in [`App::collapsed`].
    pub key: String,
    /// Containers in the stack, whether or not they're currently listed — a
    /// collapsed group still says how much it's hiding.
    pub items: usize,
    pub running: usize,
    pub collapsed: bool,
}

impl Group {
    pub fn label(&self) -> &str {
        if self.key.is_empty() {
            "standalone"
        } else {
            &self.key
        }
    }

    /// The bucket for containers with no stack at all. Sorted last, and styled
    /// down, because it's a leftovers pile rather than a thing someone deployed.
    pub fn is_standalone(&self) -> bool {
        self.key.is_empty()
    }
}

/// Everything about the log pane.
pub struct LogState {
    pub container_id: Option<String>,
    pub generation: u64,
    pub lines: VecDeque<LogLine>,
    /// Lines scrolled up from the bottom. 0 means pinned to the newest line.
    pub scroll: usize,
    pub follow: bool,
    pub wrap: bool,
    pub timestamps: bool,
    pub error: Option<String>,
    pub loading: bool,
    task: Option<JoinHandle<()>>,
}

impl Default for LogState {
    fn default() -> Self {
        Self {
            container_id: None,
            generation: 0,
            lines: VecDeque::new(),
            scroll: 0,
            follow: true,
            wrap: false,
            timestamps: true,
            error: None,
            loading: false,
            task: None,
        }
    }
}

impl LogState {
    fn reset_for(&mut self, id: Option<String>) {
        if let Some(task) = self.task.take() {
            task.abort();
        }
        self.generation += 1;
        self.container_id = id;
        self.lines.clear();
        self.scroll = 0;
        self.error = None;
        self.loading = self.container_id.is_some();
    }
}

/// A draggable boundary between two panes.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Splitter {
    /// Between the list and the log pane, dragged vertically.
    Logs,
    /// Between the list and the detail pane when they sit side by side,
    /// dragged horizontally.
    DetailSide,
    /// The same boundary when the detail pane is stacked underneath instead,
    /// dragged vertically.
    DetailBelow,
}

/// Smallest a pane may be dragged to. Below these it stops being a pane and
/// starts being a decoration — two rows of border and nothing between them.
const MIN_LOG_ROWS: u16 = 5;
const MIN_LIST_ROWS: u16 = 6;
const MIN_LIST_COLS: u16 = 40;
const MIN_DETAIL_ROWS: u16 = 4;
const MIN_DETAIL_COLS: u16 = 24;

/// Where the last frame put things the mouse can hit.
///
/// A TUI has no widget tree to hit-test against, so the render pass records the
/// rectangles it drew and the mouse reducer consults them. It's rebuilt from
/// scratch every frame, which keeps it honest: a region can never outlive the
/// thing that drew it.
#[derive(Debug, Default, Clone)]
pub struct HitMap {
    pub tabs: Vec<(Rect, Tab)>,
    /// The table's data rows, excluding the column header.
    pub list_body: Option<Rect>,
    /// View position of the first row inside `list_body`.
    pub list_offset: usize,
    /// Column headers that select a sort when clicked.
    pub list_headers: Vec<(Rect, Sort)>,
    pub logs: Option<Rect>,
    pub detail: Option<Rect>,
    /// The boundaries between panes, each a couple of cells thick so they can
    /// actually be grabbed.
    pub splitters: Vec<(Rect, Splitter)>,
    /// Palette entries, paired with their index into `action::COMMANDS`.
    pub palette_rows: Vec<(Rect, usize)>,
    pub confirm_yes: Option<Rect>,
    pub confirm_no: Option<Rect>,
    /// The modal box itself, so a click outside it can dismiss.
    pub modal: Option<Rect>,
    /// Toasts, paired with their index in `App::toasts`.
    pub toasts: Vec<(Rect, usize)>,
}

impl HitMap {
    fn clear(&mut self) {
        self.tabs.clear();
        self.list_body = None;
        self.list_offset = 0;
        self.list_headers.clear();
        self.logs = None;
        self.detail = None;
        self.splitters.clear();
        self.palette_rows.clear();
        self.confirm_yes = None;
        self.confirm_no = None;
        self.modal = None;
        self.toasts.clear();
    }
}

fn hit(rect: Option<Rect>, at: Position) -> bool {
    rect.is_some_and(|r| r.contains(at))
}

/// Two clicks on the same row this close together count as a double-click.
/// Crossterm reports individual presses, so the app has to pair them up.
const DOUBLE_CLICK: Duration = Duration::from_millis(400);
/// Rows moved per wheel notch.
const SCROLL_STEP: isize = 3;

// ---------------------------------------------------------------------------
// App
// ---------------------------------------------------------------------------

pub struct App {
    pub theme: Theme,
    pub symbols: Symbols,

    client: Client,
    tx: mpsc::UnboundedSender<AppEvent>,
    refresher: Refresher,
    stats_mgr: StatsManager,

    pub tab: Tab,
    pub focus: Focus,
    pub mode: Mode,

    pub containers: Vec<ContainerRow>,
    pub images: Vec<ImageRow>,
    pub volumes: Vec<VolumeRow>,
    pub networks: Vec<NetworkRow>,

    /// The rows each tab currently shows, after filtering, sorting and grouping.
    views: [Vec<ViewRow>; 4],
    /// The stacks of the containers tab, rebuilt with its view. Empty unless
    /// grouping is on.
    pub groups: Vec<Group>,
    /// Stacks the user has folded shut, by name. Kept outside `groups` so a
    /// collapsed stack stays collapsed across refreshes — and across a stack
    /// going away and coming back, which is what `compose down && compose up`
    /// looks like from here.
    collapsed: std::collections::HashSet<String>,
    pub group_by_stack: bool,
    /// Selection, remembered by identity so it survives a refresh that
    /// reorders rows.
    selected: [Option<String>; 4],
    /// Set instead of `selected[0]` when the cursor is on a group header. The
    /// two are mutually exclusive; `on_group` is the question to ask.
    selected_group: Option<String>,
    /// Scroll offset of each table, kept so the selection stays put visually.
    pub offset: [usize; 4],

    pub filter: [Input; 4],
    sort_index: [usize; 4],
    sort_reverse: [bool; 4],
    pub show_stopped: bool,

    /// Most recent reading per container. Only the latest is kept — nothing in
    /// the UI plots history.
    pub stats: HashMap<String, StatSample>,
    pub logs: LogState,
    log_pending: Option<(String, Instant)>,

    pub palette_input: Input,
    pub palette_cursor: usize,

    /// Resource id → the verb for the job currently running against it.
    pub pending: HashMap<String, &'static str>,
    pub global_pending: Vec<String>,
    pub confirm: Option<Job>,
    /// Which button the confirm dialog has focused. Reset to `true` every time
    /// the dialog opens, so a stale choice can't carry over into the next one.
    pub confirm_accept: bool,

    pub toasts: Vec<Toast>,
    pub daemon: DaemonInfo,
    pub daemon_error: Option<String>,

    pub show_detail: bool,
    pub show_logs: bool,

    /// Pane sizes the user has dragged to, in cells. `None` means "whatever
    /// suits this terminal", which is what everyone gets until they drag.
    log_rows: Option<u16>,
    detail_cols: Option<u16>,
    detail_rows: Option<u16>,

    /// Rebuilt by every render pass; read by the mouse reducer.
    pub hits: HitMap,
    last_click: Option<(Instant, Position)>,
    /// The splitter currently under the button, if a drag is in progress.
    drag: Option<Splitter>,

    pub spinner: usize,
    pub dirty: bool,
    pub should_quit: bool,
    /// Set when the user asks for a shell; drained by the main loop, which owns
    /// the terminal and therefore has to be the one to hand it over.
    pub exec_request: Option<(String, String)>,
}

impl App {
    pub fn new(
        client: Client,
        tx: mpsc::UnboundedSender<AppEvent>,
        refresher: Refresher,
        theme: Theme,
    ) -> Self {
        let symbols = Symbols::new(theme.glyphs);
        Self {
            theme,
            symbols,
            client,
            tx,
            refresher,
            stats_mgr: StatsManager::new(),
            tab: Tab::Containers,
            focus: Focus::List,
            mode: Mode::Normal,
            containers: Vec::new(),
            images: Vec::new(),
            volumes: Vec::new(),
            networks: Vec::new(),
            views: Default::default(),
            groups: Vec::new(),
            collapsed: Default::default(),
            group_by_stack: false,
            selected: Default::default(),
            selected_group: None,
            offset: [0; 4],
            filter: Default::default(),
            sort_index: [0; 4],
            sort_reverse: [false; 4],
            show_stopped: true,
            stats: HashMap::new(),
            logs: LogState::default(),
            log_pending: None,
            palette_input: Input::default(),
            palette_cursor: 0,
            pending: HashMap::new(),
            global_pending: Vec::new(),
            confirm: None,
            confirm_accept: true,
            toasts: Vec::new(),
            daemon: DaemonInfo::default(),
            daemon_error: None,
            show_detail: false,
            show_logs: true,
            log_rows: None,
            detail_cols: None,
            detail_rows: None,
            hits: HitMap::default(),
            last_click: None,
            drag: None,
            spinner: 0,
            dirty: true,
            should_quit: false,
            exec_request: None,
        }
    }

    // -- accessors used by the UI -------------------------------------------

    pub fn view(&self) -> &[ViewRow] {
        &self.views[self.tab.index()]
    }

    /// How many resources the view lists, ignoring group headers. This is the
    /// number worth showing next to a total — `22/21` would be nonsense.
    pub fn visible_items(&self) -> usize {
        self.view()
            .iter()
            .filter(|row| matches!(row, ViewRow::Item(_)))
            .count()
    }

    /// True when the cursor sits on a group header rather than a resource, in
    /// which case there is no selected container and the panes that describe one
    /// have nothing to say.
    pub fn on_group(&self) -> bool {
        self.grouping() && self.selected_group.is_some()
    }

    /// Grouping only applies to containers; the other tabs have no stack to
    /// group by.
    pub fn grouping(&self) -> bool {
        self.tab == Tab::Containers && self.group_by_stack
    }

    /// The group the cursor is in, whether it's on the header or on one of the
    /// rows underneath.
    pub fn current_group(&self) -> Option<&Group> {
        let key = match &self.selected_group {
            Some(key) => key.clone(),
            None => self.selected_container()?.stack.clone().unwrap_or_default(),
        };
        self.groups.iter().find(|g| g.key == key)
    }

    pub fn total_rows(&self) -> usize {
        match self.tab {
            Tab::Containers => self.containers.len(),
            Tab::Images => self.images.len(),
            Tab::Volumes => self.volumes.len(),
            Tab::Networks => self.networks.len(),
        }
    }

    /// Position of the selection within the current view.
    pub fn cursor(&self) -> Option<usize> {
        if self.on_group() {
            let key = self.selected_group.as_deref()?;
            return self.view().iter().position(|row| match row {
                ViewRow::Group(g) => self.groups.get(*g).is_some_and(|group| group.key == key),
                ViewRow::Item(_) => false,
            });
        }
        let id = self.selected[self.tab.index()].as_deref()?;
        let tab = self.tab;
        self.view().iter().position(|row| match row {
            ViewRow::Item(i) => self.row_key_at(tab, *i).as_deref() == Some(id),
            ViewRow::Group(_) => false,
        })
    }

    pub fn selected_container(&self) -> Option<&ContainerRow> {
        if self.tab != Tab::Containers || self.on_group() {
            return None;
        }
        let id = self.selected[0].as_deref()?;
        self.containers.iter().find(|c| c.id == id)
    }

    pub fn selected_image(&self) -> Option<&ImageRow> {
        if self.tab != Tab::Images {
            return None;
        }
        let id = self.selected[1].as_deref()?;
        self.images.iter().find(|i| i.reference() == id)
    }

    pub fn selected_volume(&self) -> Option<&VolumeRow> {
        if self.tab != Tab::Volumes {
            return None;
        }
        let id = self.selected[2].as_deref()?;
        self.volumes.iter().find(|v| v.name == id)
    }

    pub fn selected_network(&self) -> Option<&NetworkRow> {
        if self.tab != Tab::Networks {
            return None;
        }
        let id = self.selected[3].as_deref()?;
        self.networks.iter().find(|n| n.id == id)
    }

    pub fn sort(&self) -> Sort {
        sort_cycle(self.tab)[self.sort_index[self.tab.index()]]
    }

    pub fn sort_reversed(&self) -> bool {
        self.sort_reverse[self.tab.index()]
    }

    /// Which way the active column currently runs, largest-first being "down".
    ///
    /// The single source for both the chevron on the column header and the
    /// arrow in the pane subtitle, so the two can't end up pointing opposite
    /// ways on the same screen.
    pub fn sort_descending(&self) -> bool {
        self.sort().descends_by_default() != self.sort_reversed()
    }

    pub fn latest_stat(&self, id: &str) -> Option<&StatSample> {
        self.stats.get(id)
    }

    /// Called at the top of every render pass so hit regions never survive the
    /// frame that drew them.
    pub fn reset_hits(&mut self) {
        self.hits.clear();
    }

    // -- pane sizes ----------------------------------------------------------
    //
    // Each of these resolves a dragged size against what the terminal can
    // currently show, and writes the result back. Clamping here rather than at
    // the point of the drag means a size stays valid across a window resize:
    // drag the log pane tall, shrink the terminal, and it gives way rather than
    // squeezing the list out of existence.

    pub fn log_height(&mut self, available: u16) -> u16 {
        fit(
            &mut self.log_rows,
            available / 5 * 2,
            available,
            MIN_LOG_ROWS,
            MIN_LIST_ROWS,
        )
    }

    pub fn detail_width(&mut self, available: u16) -> u16 {
        fit(
            &mut self.detail_cols,
            available * 38 / 100,
            available,
            MIN_DETAIL_COLS,
            MIN_LIST_COLS,
        )
    }

    pub fn detail_height(&mut self, available: u16) -> u16 {
        fit(
            &mut self.detail_rows,
            available * 45 / 100,
            available,
            MIN_DETAIL_ROWS,
            MIN_LIST_ROWS,
        )
    }

    pub fn running_count(&self) -> usize {
        self.containers
            .iter()
            .filter(|c| c.state.is_running())
            .count()
    }

    /// The identity used to remember the selection on each tab. Owned rather
    /// than borrowed because an image's identity is a computed `repo:tag`
    /// rather than a field we could hand out a reference to.
    fn row_key_at(&self, tab: Tab, index: usize) -> Option<String> {
        match tab {
            Tab::Containers => self.containers.get(index).map(|c| c.id.clone()),
            Tab::Images => self.images.get(index).map(|i| i.reference()),
            Tab::Volumes => self.volumes.get(index).map(|v| v.name.clone()),
            Tab::Networks => self.networks.get(index).map(|n| n.id.clone()),
        }
    }

    // -- event handling ------------------------------------------------------

    pub fn on_key(&mut self, ev: KeyEvent) {
        // Terminals that report key releases would otherwise fire everything
        // twice.
        if ev.kind != KeyEventKind::Press {
            return;
        }
        self.dirty = true;

        match self.mode {
            Mode::Filter => self.on_key_filter(ev),
            Mode::Palette => self.on_key_palette(ev),
            Mode::Confirm => self.on_key_confirm(ev),
            Mode::Help => {
                // Any key dismisses help, except a second `?`.
                self.mode = Mode::Normal;
            }
            Mode::Normal => {
                if let Some(cmd) = resolve(&ev) {
                    self.run(cmd);
                }
            }
        }
    }

    // -- mouse ---------------------------------------------------------------

    pub fn on_mouse(&mut self, ev: MouseEvent) {
        let at = Position {
            x: ev.column,
            y: ev.row,
        };
        match ev.kind {
            MouseEventKind::Down(MouseButton::Left) => self.on_click(at),
            // Only meaningful while a splitter is held; `on_drag` is a no-op
            // otherwise, so dragging across the list still selects nothing.
            MouseEventKind::Drag(MouseButton::Left) => self.on_drag(at),
            MouseEventKind::Up(MouseButton::Left) => self.drag = None,
            MouseEventKind::ScrollDown => self.on_scroll(at, SCROLL_STEP),
            MouseEventKind::ScrollUp => self.on_scroll(at, -SCROLL_STEP),
            // The other buttons carry no meaning here, and reacting to them
            // would only cause surprises.
            _ => return,
        }
        self.dirty = true;
    }

    /// Move the held splitter to the pointer.
    ///
    /// The new size is measured from the far edge of the pane being resized, as
    /// the previous frame drew it, so the boundary lands under the cursor
    /// wherever the layout happens to have put things. It's stored raw; the
    /// layout clamps it and writes the clamped value back, which is what keeps
    /// a drag past the end from banking distance it would have to be dragged
    /// back through.
    fn on_drag(&mut self, at: Position) {
        match self.drag {
            Some(Splitter::Logs) => {
                if let Some(logs) = self.hits.logs {
                    self.log_rows = Some(logs.bottom().saturating_sub(at.y));
                }
            }
            Some(Splitter::DetailSide) => {
                if let Some(detail) = self.hits.detail {
                    self.detail_cols = Some(detail.right().saturating_sub(at.x));
                }
            }
            Some(Splitter::DetailBelow) => {
                if let Some(detail) = self.hits.detail {
                    self.detail_rows = Some(detail.bottom().saturating_sub(at.y));
                }
            }
            None => {}
        }
    }

    fn on_click(&mut self, at: Position) {
        // A modal owns the screen while it's up: nothing behind it is live.
        match self.mode {
            Mode::Help => {
                self.mode = Mode::Normal;
                return;
            }
            Mode::Palette => {
                self.click_palette(at);
                return;
            }
            Mode::Confirm => {
                self.click_confirm(at);
                return;
            }
            Mode::Normal | Mode::Filter => {}
        }

        // Toasts sit on top of everything; clicking one dismisses it early.
        if let Some(&(_, index)) = self.hits.toasts.iter().find(|(r, _)| r.contains(at)) {
            if index < self.toasts.len() {
                self.toasts.remove(index);
            }
            return;
        }

        if let Some(&(_, tab)) = self.hits.tabs.iter().find(|(r, _)| r.contains(at)) {
            self.set_tab(tab);
            return;
        }

        if let Some(&(_, sort)) = self.hits.list_headers.iter().find(|(r, _)| r.contains(at)) {
            self.set_sort(sort);
            return;
        }

        // Before the panes themselves: a splitter lies on the border rows they
        // include in their own rectangles.
        if let Some(&(_, splitter)) = self.hits.splitters.iter().find(|(r, _)| r.contains(at)) {
            let double = self
                .last_click
                .is_some_and(|(when, prev)| prev == at && when.elapsed() < DOUBLE_CLICK);
            self.last_click = Some((Instant::now(), at));
            if double {
                // The only way back to the default size once you've dragged.
                match splitter {
                    Splitter::Logs => self.log_rows = None,
                    Splitter::DetailSide => self.detail_cols = None,
                    Splitter::DetailBelow => self.detail_rows = None,
                }
            } else {
                // Pressing alone doesn't move anything — the boundary is
                // already where it is. The drag that follows moves it.
                self.drag = Some(splitter);
            }
            return;
        }

        if hit(self.hits.logs, at) {
            self.focus = Focus::Logs;
            return;
        }

        if let Some(body) = self.hits.list_body
            && body.contains(at)
        {
            self.focus = Focus::List;
            let row = self.hits.list_offset + (at.y - body.y) as usize;
            // Clicking past the last row shouldn't move the selection.
            let Some(&target) = self.view().get(row) else {
                return;
            };

            let double = self
                .last_click
                .is_some_and(|(when, prev)| prev == at && when.elapsed() < DOUBLE_CLICK);
            self.last_click = Some((Instant::now(), at));

            self.set_cursor(row);
            match target {
                // A disclosure triangle opens and shuts on a plain click, which
                // is what one looks like it should do.
                ViewRow::Group(_) => self.toggle_collapse(),
                ViewRow::Item(_) => {
                    if double {
                        self.show_detail = !self.show_detail;
                    }
                }
            }
        }
    }

    fn click_palette(&mut self, at: Position) {
        if let Some(&(_, index)) = self.hits.palette_rows.iter().find(|(r, _)| r.contains(at)) {
            self.palette_input.clear();
            self.palette_cursor = 0;
            self.mode = Mode::Normal;
            self.run(crate::action::COMMANDS[index].command);
            return;
        }
        // Clicking away from the box is the same as pressing escape.
        if !hit(self.hits.modal, at) {
            self.palette_input.clear();
            self.mode = Mode::Normal;
        }
    }

    fn click_confirm(&mut self, at: Position) {
        if hit(self.hits.confirm_yes, at) {
            self.accept_confirm();
            return;
        }
        // Anywhere else — the cancel button, or outside the dialog entirely —
        // is a "no". Destructive actions should be hard to trigger by accident
        // and easy to back out of.
        if hit(self.hits.confirm_no, at) || !hit(self.hits.modal, at) {
            self.cancel_confirm();
        }
    }

    fn on_scroll(&mut self, at: Position, delta: isize) {
        // The palette is a list too — the wheel should move through it.
        if self.mode == Mode::Palette {
            let len = self.palette_matches().len();
            let next = (self.palette_cursor as isize + delta.signum())
                .clamp(0, len.saturating_sub(1) as isize);
            self.palette_cursor = next as usize;
            return;
        }
        if matches!(self.mode, Mode::Help | Mode::Confirm) {
            return;
        }

        // Scroll acts on whatever is under the pointer, not on whatever has
        // keyboard focus — that's what makes a wheel feel right.
        if hit(self.hits.logs, at) {
            self.scroll_logs(delta);
        } else {
            self.move_selection(delta);
        }
    }

    /// Jump straight to a sort column, or reverse it if it's already active.
    /// Clicking a column header is the only way in.
    fn set_sort(&mut self, sort: Sort) {
        let idx = self.tab.index();
        let Some(pos) = sort_cycle(self.tab).iter().position(|&s| s == sort) else {
            return;
        };
        if self.sort_index[idx] == pos {
            self.sort_reverse[idx] = !self.sort_reverse[idx];
        } else {
            self.sort_index[idx] = pos;
            self.sort_reverse[idx] = false;
        }
        self.rebuild_view(self.tab);
    }

    fn on_key_filter(&mut self, ev: KeyEvent) {
        let idx = self.tab.index();
        match ev.code {
            KeyCode::Esc => {
                self.filter[idx].clear();
                self.mode = Mode::Normal;
                self.rebuild_view(self.tab);
            }
            KeyCode::Enter => self.mode = Mode::Normal,
            KeyCode::Backspace => {
                self.filter[idx].backspace();
                self.rebuild_view(self.tab);
            }
            KeyCode::Delete => {
                self.filter[idx].delete();
                self.rebuild_view(self.tab);
            }
            KeyCode::Left => self.filter[idx].left(),
            KeyCode::Right => self.filter[idx].right(),
            KeyCode::Home => self.filter[idx].home(),
            KeyCode::End => self.filter[idx].end(),
            KeyCode::Up => self.move_cursor(-1),
            KeyCode::Down => self.move_cursor(1),
            KeyCode::Char('u') if ev.modifiers.contains(KeyModifiers::CONTROL) => {
                self.filter[idx].clear();
                self.rebuild_view(self.tab);
            }
            KeyCode::Char(c) if !ev.modifiers.contains(KeyModifiers::CONTROL) => {
                self.filter[idx].insert(c);
                self.rebuild_view(self.tab);
            }
            _ => {}
        }
    }

    fn on_key_palette(&mut self, ev: KeyEvent) {
        match ev.code {
            KeyCode::Esc => {
                self.palette_input.clear();
                self.mode = Mode::Normal;
            }
            KeyCode::Enter => {
                let matches = self.palette_matches();
                let chosen = matches.get(self.palette_cursor).copied();
                self.palette_input.clear();
                self.palette_cursor = 0;
                self.mode = Mode::Normal;
                if let Some(i) = chosen {
                    self.run(crate::action::COMMANDS[i].command);
                }
            }
            KeyCode::Up => self.palette_cursor = self.palette_cursor.saturating_sub(1),
            KeyCode::Down => {
                let len = self.palette_matches().len();
                if len > 0 {
                    self.palette_cursor = (self.palette_cursor + 1).min(len - 1);
                }
            }
            KeyCode::Backspace => {
                self.palette_input.backspace();
                self.palette_cursor = 0;
            }
            KeyCode::Left => self.palette_input.left(),
            KeyCode::Right => self.palette_input.right(),
            KeyCode::Char('u') if ev.modifiers.contains(KeyModifiers::CONTROL) => {
                self.palette_input.clear();
                self.palette_cursor = 0;
            }
            KeyCode::Char(c) if !ev.modifiers.contains(KeyModifiers::CONTROL) => {
                self.palette_input.insert(c);
                self.palette_cursor = 0;
            }
            _ => {}
        }
    }

    fn on_key_confirm(&mut self, ev: KeyEvent) {
        match ev.code {
            // Move between the two buttons. There are only ever two, so every
            // one of these is a toggle rather than a direction.
            KeyCode::Left
            | KeyCode::Right
            | KeyCode::Tab
            | KeyCode::BackTab
            | KeyCode::Char('h')
            | KeyCode::Char('l') => self.confirm_accept = !self.confirm_accept,

            KeyCode::Enter => {
                if self.confirm_accept {
                    self.accept_confirm();
                } else {
                    self.cancel_confirm();
                }
            }
            // `y` and `n` answer outright, without having to move first.
            KeyCode::Char('y') | KeyCode::Char('Y') => self.accept_confirm(),
            KeyCode::Char('n') | KeyCode::Char('N') | KeyCode::Esc => self.cancel_confirm(),
            _ => {}
        }
    }

    fn accept_confirm(&mut self) {
        if let Some(job) = self.confirm.take() {
            self.start_job(job);
        }
        self.mode = Mode::Normal;
    }

    fn cancel_confirm(&mut self) {
        self.confirm = None;
        self.mode = Mode::Normal;
    }

    /// Rank every command against the palette query.
    ///
    /// A hit in the command's *name* always outranks one that only appears in
    /// its description — otherwise typing `rest` surfaces "copy id" (whose help
    /// text contains "resource") above "container: restart".
    pub fn palette_matches(&self) -> Vec<usize> {
        let needle = &self.palette_input.value;
        if needle.is_empty() {
            return (0..crate::action::COMMANDS.len()).collect();
        }

        // Comfortably above any score `fuzzy` can produce, including its
        // substring band.
        const NAME_BONUS: i32 = 100_000;
        let mut scored: Vec<(usize, i32)> = crate::action::COMMANDS
            .iter()
            .enumerate()
            .filter_map(|(i, spec)| {
                if let Some(m) = fuzzy::match_str(spec.name, needle) {
                    Some((i, m.score + NAME_BONUS))
                } else {
                    fuzzy::match_str(spec.help, needle).map(|m| (i, m.score))
                }
            })
            .collect();
        scored.sort_by_key(|&(_, score)| std::cmp::Reverse(score));
        scored.into_iter().map(|(i, _)| i).collect()
    }

    pub fn on_app_event(&mut self, ev: AppEvent) {
        self.dirty = true;
        match ev {
            AppEvent::Snapshot(snap) => {
                self.containers = snap.containers;
                self.images = snap.images;
                self.volumes = snap.volumes;
                self.networks = snap.networks;
                model::annotate_usage(&self.containers, &mut self.volumes, &mut self.networks);

                // Drop stats history for containers that no longer exist, or it
                // grows without bound across a long session.
                let live: std::collections::HashSet<&str> =
                    self.containers.iter().map(|c| c.id.as_str()).collect();
                self.stats.retain(|id, _| live.contains(id.as_str()));

                self.stats_mgr
                    .sync(&self.client, &self.containers, &self.tx);
                self.rebuild_all_views();
                self.sync_log_target();
            }
            AppEvent::Stat { id, sample } => {
                self.stats.insert(id, sample);
                // Only re-sort when the ordering actually depends on stats.
                if matches!(self.sort(), Sort::Cpu | Sort::Mem) {
                    self.rebuild_view(self.tab);
                }
            }
            AppEvent::LogLine { generation, line } => {
                if generation != self.logs.generation {
                    return; // a stale stream we've already moved on from
                }
                self.logs.loading = false;
                if self.logs.lines.len() >= LOG_CAPACITY {
                    self.logs.lines.pop_front();
                    // Keep the viewport anchored to the same text when we're
                    // scrolled back and the buffer is rolling.
                    if self.logs.scroll > 0 {
                        self.logs.scroll = self.logs.scroll.saturating_sub(1);
                    }
                }
                self.logs.lines.push_back(line);
                if !self.logs.follow && self.logs.scroll > 0 {
                    self.logs.scroll += 1;
                }
            }
            AppEvent::LogAttached { generation } => {
                // Stop the spinner but leave any error in place: the pane then
                // falls through to "no output yet", which is the truth.
                if generation == self.logs.generation {
                    self.logs.loading = false;
                }
            }
            AppEvent::LogEnd { generation } => {
                if generation == self.logs.generation {
                    self.logs.loading = false;
                }
            }
            AppEvent::LogError {
                generation,
                message,
            } => {
                if generation == self.logs.generation {
                    self.logs.loading = false;
                    self.logs.error = Some(message);
                }
            }
            AppEvent::JobDone { job, result } => {
                if let Some(id) = job.target_id() {
                    self.pending.remove(id);
                }
                let desc = job.describe();
                self.global_pending.retain(|d| d != &desc);
                match result {
                    Ok(msg) => self.toast(ToastKind::Success, msg),
                    Err(msg) => self.toast(ToastKind::Error, msg),
                }
                self.refresher.refresh_now();
            }
            AppEvent::Daemon(info) => self.daemon = *info,
            AppEvent::DaemonError(msg) => {
                self.daemon_error = Some(msg);
                self.stats_mgr.clear();
            }
            AppEvent::DaemonOk => {
                if self.daemon_error.take().is_some() {
                    self.toast(
                        ToastKind::Success,
                        "reconnected to the docker daemon".into(),
                    );
                }
            }
        }
    }

    /// Called on the render tick. Handles animation, toast expiry and the
    /// debounced log attach.
    pub fn on_tick(&mut self) {
        let animating = !self.pending.is_empty()
            || !self.global_pending.is_empty()
            || self.logs.loading
            || !self.toasts.is_empty();
        if animating {
            self.spinner = self.spinner.wrapping_add(1);
            self.dirty = true;
        }

        let before = self.toasts.len();
        self.toasts.retain(|t| t.born.elapsed() < TOAST_TTL);
        if self.toasts.len() != before {
            self.dirty = true;
        }

        if let Some((id, since)) = self.log_pending.clone()
            && since.elapsed() >= LOG_DEBOUNCE
        {
            self.log_pending = None;
            self.attach_logs(id);
            self.dirty = true;
        }
    }

    // -- commands ------------------------------------------------------------

    pub fn run(&mut self, cmd: Command) {
        // These all act on one container, and a stack header isn't one. Saying so
        // beats a key that silently does nothing.
        if self.on_group()
            && matches!(
                cmd,
                Command::Start
                    | Command::Stop
                    | Command::Restart
                    | Command::Pause
                    | Command::Kill
                    | Command::Remove
                    | Command::Exec
                    | Command::CopyId
            )
        {
            self.toast(
                ToastKind::Info,
                "that row is a stack — pick a container in it".into(),
            );
            return;
        }

        match cmd {
            Command::Quit => self.should_quit = true,
            Command::Help => self.mode = Mode::Help,
            Command::Palette => {
                self.palette_input.clear();
                self.palette_cursor = 0;
                self.mode = Mode::Palette;
            }
            Command::Filter => self.mode = Mode::Filter,
            Command::ClearFilter => {
                self.filter[self.tab.index()].clear();
                self.rebuild_view(self.tab);
            }
            Command::Refresh => {
                self.refresher.refresh_now();
                self.toast(ToastKind::Info, "refreshing…".into());
            }

            Command::NextTab => self.set_tab(Tab::ALL[(self.tab.index() + 1) % 4]),
            Command::PrevTab => self.set_tab(Tab::ALL[(self.tab.index() + 3) % 4]),
            Command::TabContainers => self.set_tab(Tab::Containers),
            Command::TabImages => self.set_tab(Tab::Images),
            Command::TabVolumes => self.set_tab(Tab::Volumes),
            Command::TabNetworks => self.set_tab(Tab::Networks),

            Command::Down => self.move_cursor(1),
            Command::Up => self.move_cursor(-1),
            Command::PageDown => self.move_cursor(10),
            Command::PageUp => self.move_cursor(-10),
            Command::Top => self.set_cursor(0),
            Command::Bottom => self.set_cursor(self.view().len().saturating_sub(1)),

            Command::ToggleDetail => {
                self.show_detail = !self.show_detail;
            }
            Command::ToggleLogs => {
                if self.tab == Tab::Containers {
                    self.show_logs = !self.show_logs;
                    if !self.show_logs {
                        self.focus = Focus::List;
                    }
                    self.sync_log_target();
                }
            }
            Command::FocusNext => {
                if self.tab == Tab::Containers && self.show_logs {
                    self.focus = match self.focus {
                        Focus::List => Focus::Logs,
                        Focus::Logs => Focus::List,
                    };
                }
            }
            Command::ToggleFollow => {
                self.logs.follow = !self.logs.follow;
                if self.logs.follow {
                    self.logs.scroll = 0;
                }
            }
            Command::ToggleWrap => self.logs.wrap = !self.logs.wrap,
            Command::ToggleTimestamps => self.logs.timestamps = !self.logs.timestamps,

            Command::ToggleAll => {
                self.show_stopped = !self.show_stopped;
                self.rebuild_view(Tab::Containers);
            }
            Command::ToggleGroup => {
                if self.tab == Tab::Containers {
                    self.group_by_stack = !self.group_by_stack;
                    self.selected_group = None;
                    // The row count changes wholesale, so a scroll position from
                    // the old shape means nothing.
                    self.offset[0] = 0;
                    self.rebuild_view(Tab::Containers);
                }
            }
            Command::ToggleCollapse => self.toggle_collapse(),
            Command::ToggleCollapseAll => self.toggle_collapse_all(),
            Command::SortNext => {
                let idx = self.tab.index();
                self.sort_index[idx] = (self.sort_index[idx] + 1) % sort_cycle(self.tab).len();
                self.rebuild_view(self.tab);
            }
            Command::SortReverse => {
                let idx = self.tab.index();
                self.sort_reverse[idx] = !self.sort_reverse[idx];
                self.rebuild_view(self.tab);
            }

            Command::CopyId => self.copy_id(),
            Command::Exec => self.request_exec(),

            Command::Start
            | Command::Stop
            | Command::Restart
            | Command::Pause
            | Command::Kill
            | Command::Remove
            | Command::Prune => {
                if let Some(job) = self.build_job(cmd) {
                    self.submit(job);
                }
            }
        }
    }

    fn set_tab(&mut self, tab: Tab) {
        if self.tab == tab {
            return;
        }
        self.tab = tab;
        self.focus = Focus::List;
        self.ensure_selection();
        self.sync_log_target();
    }

    /// Keyboard movement: the arrows follow focus, so they scroll the log pane
    /// when that's what's focused.
    fn move_cursor(&mut self, delta: isize) {
        if self.focus == Focus::Logs {
            self.scroll_logs(delta);
            return;
        }
        self.move_selection(delta);
    }

    /// Move the list selection regardless of where focus is. The mouse wheel
    /// uses this directly, since it targets whatever is under the pointer.
    fn move_selection(&mut self, delta: isize) {
        let len = self.view().len();
        if len == 0 {
            return;
        }
        let current = self.cursor().unwrap_or(0) as isize;
        let next = (current + delta).clamp(0, len as isize - 1) as usize;
        self.set_cursor(next);
    }

    fn set_cursor(&mut self, position: usize) {
        let Some(&row) = self.view().get(position) else {
            return;
        };
        match row {
            ViewRow::Item(index) => {
                let tab = self.tab;
                self.selected_group = None;
                self.selected[tab.index()] = self.row_key_at(tab, index);
            }
            // A header is selectable in its own right: it has to be, or a stack
            // folded shut couldn't be reached to open again.
            ViewRow::Group(g) => {
                if let Some(group) = self.groups.get(g) {
                    self.selected_group = Some(group.key.clone());
                }
            }
        }
        self.sync_log_target();
    }

    /// Fold the stack under the cursor, or the one containing it.
    ///
    /// Collapsing from a row moves the cursor up onto the header, because the row
    /// it was on is about to stop being listed.
    fn toggle_collapse(&mut self) {
        if !self.grouping() {
            return;
        }
        let Some(key) = self.current_group().map(|g| g.key.clone()) else {
            return;
        };
        if !self.collapsed.remove(&key) {
            self.collapsed.insert(key.clone());
        }
        self.selected_group = Some(key);
        self.rebuild_view(Tab::Containers);
    }

    /// Fold every stack, or unfold every stack if none are folded. With a dozen
    /// projects this is the fastest way to get an overview of what's deployed.
    fn toggle_collapse_all(&mut self) {
        if !self.grouping() {
            return;
        }
        let any_open = self.groups.iter().any(|g| !g.collapsed);
        if any_open {
            for group in &self.groups {
                self.collapsed.insert(group.key.clone());
            }
            // Every row is about to disappear, so park the cursor on the header
            // of whichever stack it was in.
            if let Some(key) = self.current_group().map(|g| g.key.clone()) {
                self.selected_group = Some(key);
            }
        } else {
            self.collapsed.clear();
        }
        self.rebuild_view(Tab::Containers);
    }

    fn scroll_logs(&mut self, delta: isize) {
        let max = self.logs.lines.len();
        let next = (self.logs.scroll as isize - delta).clamp(0, max as isize) as usize;
        self.logs.scroll = next;
        // Scrolling away from the bottom implicitly turns off follow; coming
        // back to the bottom turns it on again. That's what everyone expects
        // from a log viewer.
        self.logs.follow = next == 0;
    }

    fn copy_id(&mut self) {
        let value = match self.tab {
            Tab::Containers => self.selected_container().map(|c| c.id.clone()),
            Tab::Images => self.selected_image().map(|i| i.id.clone()),
            Tab::Volumes => self.selected_volume().map(|v| v.name.clone()),
            Tab::Networks => self.selected_network().map(|n| n.id.clone()),
        };
        let Some(value) = value else { return };
        match clipboard::copy(&value) {
            Ok(()) => self.toast(
                ToastKind::Success,
                format!("copied {}", crate::util::format::short_id(&value)),
            ),
            Err(e) => self.toast(ToastKind::Error, format!("clipboard failed: {e}")),
        }
    }

    fn request_exec(&mut self) {
        let Some(c) = self.selected_container() else {
            return;
        };
        if !c.state.is_running() {
            self.toast(ToastKind::Error, format!("{} is not running", c.name));
            return;
        }
        self.exec_request = Some((c.id.clone(), c.name.clone()));
    }

    /// Turn a command plus the current selection into a concrete job.
    fn build_job(&self, cmd: Command) -> Option<Job> {
        match self.tab {
            Tab::Containers => {
                if cmd == Command::Prune {
                    return Some(Job::PruneContainers);
                }
                let c = self.selected_container()?;
                let (id, name) = (c.id.clone(), c.name.clone());
                Some(match cmd {
                    Command::Start => Job::Start { id, name },
                    Command::Stop => Job::Stop { id, name },
                    Command::Restart => Job::Restart { id, name },
                    Command::Pause => {
                        if c.state == State::Paused {
                            Job::Unpause { id, name }
                        } else {
                            Job::Pause { id, name }
                        }
                    }
                    Command::Kill => Job::Kill { id, name },
                    Command::Remove => Job::RemoveContainer {
                        id,
                        name,
                        force: c.state.is_live(),
                    },
                    _ => return None,
                })
            }
            Tab::Images => {
                if cmd == Command::Prune {
                    return Some(Job::PruneImages);
                }
                let i = self.selected_image()?;
                match cmd {
                    // Removing by `repo:tag` untags rather than nuking every
                    // tag that shares the layer stack — which is what someone
                    // looking at a tagged row means.
                    Command::Remove => Some(Job::RemoveImage {
                        id: if i.dangling {
                            i.id.clone()
                        } else {
                            i.reference()
                        },
                        name: i.reference(),
                        force: false,
                    }),
                    _ => None,
                }
            }
            Tab::Volumes => {
                if cmd == Command::Prune {
                    return Some(Job::PruneVolumes);
                }
                let v = self.selected_volume()?;
                match cmd {
                    Command::Remove => Some(Job::RemoveVolume {
                        name: v.name.clone(),
                        force: false,
                    }),
                    _ => None,
                }
            }
            Tab::Networks => {
                if cmd == Command::Prune {
                    return Some(Job::PruneNetworks);
                }
                let n = self.selected_network()?;
                match cmd {
                    Command::Remove => Some(Job::RemoveNetwork {
                        id: n.id.clone(),
                        name: n.name.clone(),
                    }),
                    _ => None,
                }
            }
        }
    }

    fn submit(&mut self, job: Job) {
        // Built-in networks can't be deleted; say so rather than letting the
        // daemon return a confusing error.
        if let Job::RemoveNetwork { name, .. } = &job
            && matches!(name.as_str(), "bridge" | "host" | "none")
        {
            self.toast(ToastKind::Error, format!("{name} is a built-in network"));
            return;
        }

        if job.needs_confirmation() {
            self.confirm = Some(job);
            self.confirm_accept = true;
            self.mode = Mode::Confirm;
        } else {
            self.start_job(job);
        }
    }

    fn start_job(&mut self, job: Job) {
        match job.target_id() {
            Some(id) => {
                self.pending.insert(id.to_string(), job.verb());
            }
            None => self.global_pending.push(job.describe()),
        }
        actions::spawn(self.client.clone(), job, self.tx.clone());
    }

    pub fn toast(&mut self, kind: ToastKind, text: String) {
        // Keep the stack short; the newest messages are the relevant ones.
        if self.toasts.len() >= 4 {
            self.toasts.remove(0);
        }
        self.toasts.push(Toast {
            kind,
            text,
            born: Instant::now(),
        });
        self.dirty = true;
    }

    // -- logs ---------------------------------------------------------------

    /// Called whenever the selection or the visibility of the log pane changes.
    fn sync_log_target(&mut self) {
        let wanted = if self.tab == Tab::Containers && self.show_logs {
            self.selected_container().map(|c| c.id.clone())
        } else {
            None
        };

        match wanted {
            None => {
                if self.logs.container_id.is_some() {
                    self.logs.reset_for(None);
                    self.log_pending = None;
                }
            }
            Some(id) => {
                if self.logs.container_id.as_deref() == Some(id.as_str()) {
                    return; // already attached
                }
                // Show the new (empty) pane immediately, but wait out the
                // debounce before actually opening a stream.
                self.logs.reset_for(Some(id.clone()));
                self.log_pending = Some((id, Instant::now()));
            }
        }
    }

    fn attach_logs(&mut self, id: String) {
        if self.logs.container_id.as_deref() != Some(id.as_str()) {
            return; // selection moved on again during the debounce
        }
        let handle = logs::spawn(
            self.client.clone(),
            id,
            self.logs.generation,
            self.tx.clone(),
        );
        self.logs.task = Some(handle);
    }

    // -- views ---------------------------------------------------------------

    fn rebuild_all_views(&mut self) {
        for tab in Tab::ALL {
            self.rebuild_view(tab);
        }
    }

    fn rebuild_view(&mut self, tab: Tab) {
        let needle = self.filter[tab.index()].value.clone();
        let sort = sort_cycle(tab)[self.sort_index[tab.index()]];
        let reverse = self.sort_reverse[tab.index()];

        let mut view: Vec<usize> = match tab {
            Tab::Containers => {
                let candidates: Vec<usize> = self
                    .containers
                    .iter()
                    .enumerate()
                    .filter(|(_, c)| self.show_stopped || c.state.is_live())
                    .map(|(i, _)| i)
                    .collect();
                self.filter_indices(&candidates, &needle, |i| self.containers[i].search_key())
            }
            Tab::Images => {
                let candidates: Vec<usize> = (0..self.images.len()).collect();
                self.filter_indices(&candidates, &needle, |i| self.images[i].search_key())
            }
            Tab::Volumes => {
                let candidates: Vec<usize> = (0..self.volumes.len()).collect();
                self.filter_indices(&candidates, &needle, |i| self.volumes[i].search_key())
            }
            Tab::Networks => {
                let candidates: Vec<usize> = (0..self.networks.len()).collect();
                self.filter_indices(&candidates, &needle, |i| self.networks[i].search_key())
            }
        };

        // With an active filter the fuzzy ranking *is* the order; imposing a
        // sort on top would bury the best match.
        if needle.is_empty() {
            self.sort_indices(tab, sort, &mut view);
            if reverse {
                view.reverse();
            }
        }

        self.views[tab.index()] = if tab == Tab::Containers && self.group_by_stack {
            let (rows, groups) = group_rows(&self.containers, view, &self.collapsed);
            self.groups = groups;
            rows
        } else {
            if tab == Tab::Containers {
                self.groups.clear();
            }
            view.into_iter().map(ViewRow::Item).collect()
        };

        if tab == self.tab {
            self.ensure_selection();
        }
    }

    fn filter_indices<F>(&self, candidates: &[usize], needle: &str, key: F) -> Vec<usize>
    where
        F: Fn(usize) -> String,
    {
        if needle.is_empty() {
            return candidates.to_vec();
        }
        fuzzy::rank(candidates, needle, |&i| key(i))
            .into_iter()
            .map(|pos| candidates[pos])
            .collect()
    }

    fn sort_indices(&self, tab: Tab, sort: Sort, view: &mut [usize]) {
        match tab {
            Tab::Containers => view.sort_by(|&a, &b| {
                let (x, y) = (&self.containers[a], &self.containers[b]);
                match sort {
                    Sort::State => x.state.cmp(&y.state).then_with(|| x.name.cmp(&y.name)),
                    Sort::Cpu => cmp_f64(
                        self.latest_stat(&y.id)
                            .map(|s| s.cpu_percent)
                            .unwrap_or(-1.0),
                        self.latest_stat(&x.id)
                            .map(|s| s.cpu_percent)
                            .unwrap_or(-1.0),
                    )
                    .then_with(|| x.name.cmp(&y.name)),
                    Sort::Mem => self
                        .latest_stat(&y.id)
                        .map(|s| s.mem_bytes)
                        .unwrap_or(0)
                        .cmp(&self.latest_stat(&x.id).map(|s| s.mem_bytes).unwrap_or(0))
                        .then_with(|| x.name.cmp(&y.name)),
                    Sort::Created => y.created.cmp(&x.created),
                    _ => x.name.cmp(&y.name),
                }
            }),
            Tab::Images => view.sort_by(|&a, &b| {
                let (x, y) = (&self.images[a], &self.images[b]);
                match sort {
                    Sort::Size => y.size.cmp(&x.size),
                    Sort::Created => y.created.cmp(&x.created),
                    _ => x.reference().cmp(&y.reference()),
                }
            }),
            Tab::Volumes => view.sort_by(|&a, &b| {
                let (x, y) = (&self.volumes[a], &self.volumes[b]);
                match sort {
                    Sort::Size => y.size.unwrap_or(-1).cmp(&x.size.unwrap_or(-1)),
                    Sort::Created => y.created.cmp(&x.created),
                    _ => x.name.cmp(&y.name),
                }
            }),
            Tab::Networks => view.sort_by(|&a, &b| {
                let (x, y) = (&self.networks[a], &self.networks[b]);
                match sort {
                    Sort::Driver => x.driver.cmp(&y.driver).then_with(|| x.name.cmp(&y.name)),
                    Sort::Created => y.created.cmp(&x.created),
                    _ => x.name.cmp(&y.name),
                }
            }),
        }
    }

    /// Make sure something is selected, and that it still exists.
    fn ensure_selection(&mut self) {
        let tab = self.tab;
        let idx = tab.index();

        // A header selection outlives a refresh, but not the stack going away or
        // grouping being switched off.
        if self.selected_group.is_some() {
            let alive = self.grouping()
                && self
                    .selected_group
                    .as_ref()
                    .is_some_and(|key| self.groups.iter().any(|g| &g.key == key));
            if alive {
                return;
            }
            self.selected_group = None;
        }

        let still_there = self.selected[idx].as_ref().is_some_and(|id| {
            self.views[idx].iter().any(|row| match row {
                ViewRow::Item(i) => self.row_key_at(tab, *i).as_deref() == Some(id.as_str()),
                ViewRow::Group(_) => false,
            })
        });
        if still_there {
            return;
        }

        let first_item = self.views[idx].iter().find_map(|row| match row {
            ViewRow::Item(i) => Some(*i),
            ViewRow::Group(_) => None,
        });
        self.selected[idx] = first_item.and_then(|i| self.row_key_at(tab, i));

        // Every stack folded shut leaves no rows to select, so fall back to the
        // first header rather than leaving the list with no cursor at all.
        if self.selected[idx].is_none()
            && let Some(ViewRow::Group(g)) = self.views[idx].first()
            && let Some(group) = self.groups.get(*g)
        {
            self.selected_group = Some(group.key.clone());
        }
    }
}

/// Fold a flat run of container indices into stacks.
///
/// The order *within* each stack is whatever the caller established, so the
/// active sort and the fuzzy ranking both survive grouping — filtering by
/// `postgres` with grouping on shows one `postgres` under `argilla` and another
/// under `lakefs`, which is the whole point of grouping by the label rather than
/// by the name.
///
/// The stacks themselves go in name order, with the standalone bucket last. Not
/// in the active sort's order: a stack has no single cpu figure, and headers that
/// reshuffled as usage drifted would make the list impossible to keep your place
/// in.
fn group_rows(
    containers: &[ContainerRow],
    items: Vec<usize>,
    collapsed: &std::collections::HashSet<String>,
) -> (Vec<ViewRow>, Vec<Group>) {
    let mut keys: Vec<String> = Vec::new();
    let mut members: HashMap<String, Vec<usize>> = HashMap::new();

    for index in items {
        let key = containers[index].stack.clone().unwrap_or_default();
        if !members.contains_key(&key) {
            keys.push(key.clone());
        }
        members.entry(key).or_default().push(index);
    }

    keys.sort_by(|a, b| match (a.is_empty(), b.is_empty()) {
        (true, false) => std::cmp::Ordering::Greater,
        (false, true) => std::cmp::Ordering::Less,
        _ => a.cmp(b),
    });

    let mut groups = Vec::with_capacity(keys.len());
    let mut rows = Vec::new();

    for key in keys {
        let indices = members.remove(&key).unwrap_or_default();
        let folded = collapsed.contains(&key);
        let running = indices
            .iter()
            .filter(|&&i| containers[i].state.is_running())
            .count();

        rows.push(ViewRow::Group(groups.len()));
        groups.push(Group {
            key,
            items: indices.len(),
            running,
            collapsed: folded,
        });
        // A folded stack keeps its counts — the header still has to say how much
        // it's hiding.
        if !folded {
            rows.extend(indices.into_iter().map(ViewRow::Item));
        }
    }

    (rows, groups)
}

/// Resolve one pane's size against the space available.
///
/// `size` is the dragged size, or `None` for "use `default`". It's clamped to
/// leave `reserve` cells for the pane it shares the axis with, never taking
/// itself below `min`, and the clamped value is written back so a size that a
/// small terminal cut down doesn't spring back when the window grows again —
/// dragging is a choice, but so is the last size that actually fitted.
///
/// When the two minimums can't both be met the pane being sized wins, and the
/// layout squeezes its neighbour. Something has to give on a 10-row terminal,
/// and the alternative is a pane with no interior at all.
fn fit(size: &mut Option<u16>, default: u16, available: u16, min: u16, reserve: u16) -> u16 {
    let resolved = size
        .unwrap_or(default)
        .clamp(min, available.saturating_sub(reserve).max(min));
    if size.is_some() {
        *size = Some(resolved);
    }
    resolved
}

fn cmp_f64(a: f64, b: f64) -> std::cmp::Ordering {
    a.partial_cmp(&b).unwrap_or(std::cmp::Ordering::Equal)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn input_edits_by_character_not_byte() {
        let mut i = Input::default();
        for c in "héllo".chars() {
            i.insert(c);
        }
        assert_eq!(i.value, "héllo");
        i.left();
        i.backspace();
        assert_eq!(i.value, "hélo");
        i.home();
        i.delete();
        assert_eq!(i.value, "élo");
    }

    #[test]
    fn input_cursor_is_clamped() {
        let mut i = Input::default();
        i.left();
        assert_eq!(i.cursor, 0);
        i.insert('a');
        i.right();
        i.right();
        assert_eq!(i.cursor, 1);
    }

    fn at(x: u16, y: u16) -> Position {
        Position { x, y }
    }

    #[test]
    fn hit_testing_respects_bounds() {
        let r = Rect {
            x: 10,
            y: 5,
            width: 4,
            height: 2,
        };
        assert!(hit(Some(r), at(10, 5)));
        assert!(hit(Some(r), at(13, 6)));
        // Rect bounds are exclusive at the far edge.
        assert!(!hit(Some(r), at(14, 5)));
        assert!(!hit(Some(r), at(10, 7)));
        assert!(!hit(Some(r), at(9, 5)));
        assert!(!hit(None, at(10, 5)));
    }

    #[test]
    fn hit_map_clears_every_region() {
        let mut hits = HitMap {
            tabs: vec![(Rect::default(), Tab::Images)],
            list_body: Some(Rect::default()),
            list_offset: 7,
            list_headers: vec![(Rect::default(), Sort::Cpu)],
            logs: Some(Rect::default()),
            detail: Some(Rect::default()),
            splitters: vec![(Rect::default(), Splitter::Logs)],
            palette_rows: vec![(Rect::default(), 3)],
            confirm_yes: Some(Rect::default()),
            confirm_no: Some(Rect::default()),
            modal: Some(Rect::default()),
            toasts: vec![(Rect::default(), 0)],
        };
        hits.clear();
        // A region left over from a previous frame describes a layout that no
        // longer exists, so `clear` has to be exhaustive.
        assert!(hits.tabs.is_empty());
        assert!(hits.list_body.is_none());
        assert_eq!(hits.list_offset, 0);
        assert!(hits.list_headers.is_empty());
        assert!(hits.logs.is_none());
        assert!(hits.detail.is_none());
        assert!(hits.splitters.is_empty());
        assert!(hits.palette_rows.is_empty());
        assert!(hits.confirm_yes.is_none());
        assert!(hits.confirm_no.is_none());
        assert!(hits.modal.is_none());
        assert!(hits.toasts.is_empty());
    }

    #[test]
    fn every_sortable_column_is_in_its_tab_cycle() {
        // A column header can only sort by something the tab actually cycles
        // through; `set_sort` silently ignores anything else.
        for (tab, sorts) in [
            (
                Tab::Containers,
                vec![Sort::Name, Sort::State, Sort::Cpu, Sort::Mem],
            ),
            (Tab::Images, vec![Sort::Name, Sort::Size, Sort::Created]),
            (Tab::Volumes, vec![Sort::Name, Sort::Created]),
            (Tab::Networks, vec![Sort::Name, Sort::Driver]),
        ] {
            for sort in sorts {
                assert!(
                    sort_cycle(tab).contains(&sort),
                    "{:?} column on {:?} has no matching sort",
                    sort,
                    tab
                );
            }
        }
    }

    #[test]
    fn an_undragged_pane_takes_the_default() {
        let mut size = None;
        assert_eq!(fit(&mut size, 12, 40, 5, 6), 12);
        // Reading a size must not turn it into a dragged one, or the pane would
        // stop tracking the terminal as it resizes.
        assert_eq!(size, None);
    }

    #[test]
    fn a_dragged_pane_keeps_its_size() {
        let mut size = Some(20);
        assert_eq!(fit(&mut size, 12, 40, 5, 6), 20);
        assert_eq!(size, Some(20));
    }

    #[test]
    fn a_dragged_pane_gives_way_to_its_neighbour() {
        // 40 available, 6 reserved: 34 is as tall as this pane may be.
        let mut size = Some(200);
        assert_eq!(fit(&mut size, 12, 40, 5, 6), 34);
        // Written back, so shrinking the terminal and growing it again doesn't
        // restore a size the user can no longer see the effect of.
        assert_eq!(size, Some(34));
    }

    #[test]
    fn a_pane_is_never_dragged_away_entirely() {
        let mut size = Some(0);
        assert_eq!(fit(&mut size, 12, 40, 5, 6), 5);
    }

    #[test]
    fn a_terminal_too_small_for_both_still_yields_a_pane() {
        // 4 rows with 6 reserved: the reservation can't be honoured, and the
        // minimum has to win over it rather than underflowing.
        let mut size = None;
        assert_eq!(fit(&mut size, 12, 4, 5, 6), 5);
        let mut dragged = Some(30);
        assert_eq!(fit(&mut dragged, 12, 4, 5, 6), 5);
    }

    #[test]
    fn measured_columns_start_at_the_largest_value() {
        // Opening a cpu or size column to find the idlest, smallest rows at the
        // top would be useless, so these descend before they're reversed.
        assert!(Sort::Cpu.descends_by_default());
        assert!(Sort::Mem.descends_by_default());
        assert!(Sort::Size.descends_by_default());
    }

    #[test]
    fn text_columns_start_at_a() {
        assert!(!Sort::Name.descends_by_default());
        assert!(!Sort::State.descends_by_default());
        assert!(!Sort::Driver.descends_by_default());
    }

    #[test]
    fn the_age_column_ascends_because_newest_is_smallest() {
        // The comparator puts the newest row first, but the column shows a
        // duration, and the newest row has the smallest of those. The chevron
        // describes the column you can see, not the timestamp behind it.
        assert!(!Sort::Created.descends_by_default());
        assert_eq!(Sort::Created.label(), "age");
    }

    #[test]
    fn every_tab_has_a_sort_cycle() {
        for tab in Tab::ALL {
            assert!(!sort_cycle(tab).is_empty());
        }
    }

    // -- grouping ------------------------------------------------------------

    fn container(name: &str, stack: Option<&str>, state: State) -> ContainerRow {
        ContainerRow {
            id: name.into(),
            name: name.into(),
            image: String::new(),
            image_id: String::new(),
            command: String::new(),
            state,
            health: crate::docker::model::Health::None,
            status: String::new(),
            created: 0,
            ports: vec![],
            networks: vec![],
            mounts: vec![],
            stack: stack.map(str::to_string),
            service: Some(name.to_string()),
        }
    }

    /// Three stacks and two hand-started containers, which is the shape of a
    /// normal machine.
    fn fixture() -> Vec<ContainerRow> {
        vec![
            container("lakefs", Some("lakefs"), State::Running),
            container("argilla-postgres-1", Some("argilla"), State::Running),
            container("my-redis", None, State::Running),
            container("argilla-worker-1", Some("argilla"), State::Exited),
            container("temporal", Some("temporal"), State::Running),
            container("my-postgres", None, State::Exited),
            container("lakefs-postgres", Some("lakefs"), State::Running),
        ]
    }

    fn no_folds() -> std::collections::HashSet<String> {
        std::collections::HashSet::new()
    }

    /// The stack of each row, for readable assertions.
    fn shape(containers: &[ContainerRow], rows: &[ViewRow], groups: &[Group]) -> Vec<String> {
        rows.iter()
            .map(|row| match row {
                ViewRow::Group(g) => format!("[{}]", groups[*g].label()),
                ViewRow::Item(i) => containers[*i].name.clone(),
            })
            .collect()
    }

    #[test]
    fn stacks_are_gathered_with_standalone_last() {
        let containers = fixture();
        let all: Vec<usize> = (0..containers.len()).collect();
        let (rows, groups) = group_rows(&containers, all, &no_folds());

        assert_eq!(
            shape(&containers, &rows, &groups),
            vec![
                "[argilla]",
                "argilla-postgres-1",
                "argilla-worker-1",
                "[lakefs]",
                "lakefs",
                "lakefs-postgres",
                "[temporal]",
                "temporal",
                // Not a deployment, so it sorts below the things that are.
                "[standalone]",
                "my-redis",
                "my-postgres",
            ]
        );
    }

    #[test]
    fn a_header_counts_what_is_up() {
        let containers = fixture();
        let all: Vec<usize> = (0..containers.len()).collect();
        let (_, groups) = group_rows(&containers, all, &no_folds());

        let argilla = groups.iter().find(|g| g.key == "argilla").unwrap();
        assert_eq!((argilla.items, argilla.running), (2, 1));
        let lakefs = groups.iter().find(|g| g.key == "lakefs").unwrap();
        assert_eq!((lakefs.items, lakefs.running), (2, 2));
    }

    #[test]
    fn a_folded_stack_shows_its_header_and_nothing_else() {
        let containers = fixture();
        let all: Vec<usize> = (0..containers.len()).collect();
        let folded = ["argilla".to_string()].into_iter().collect();
        let (rows, groups) = group_rows(&containers, all, &folded);

        let shape = shape(&containers, &rows, &groups);
        assert!(shape.contains(&"[argilla]".to_string()));
        assert!(!shape.contains(&"argilla-postgres-1".to_string()));
        // Still knows what it's hiding, which is what the header reports.
        let argilla = groups.iter().find(|g| g.key == "argilla").unwrap();
        assert_eq!(argilla.items, 2);
        assert!(argilla.collapsed);
        // Its neighbours are untouched.
        assert!(shape.contains(&"lakefs-postgres".to_string()));
    }

    #[test]
    fn folding_everything_leaves_only_headers() {
        let containers = fixture();
        let all: Vec<usize> = (0..containers.len()).collect();
        let folded = ["argilla", "lakefs", "temporal", ""]
            .iter()
            .map(|s| s.to_string())
            .collect();
        let (rows, _) = group_rows(&containers, all, &folded);
        assert!(rows.iter().all(|r| matches!(r, ViewRow::Group(_))));
        assert_eq!(rows.len(), 4);
    }

    #[test]
    fn the_standalone_bucket_is_dropped_when_nothing_is_in_it() {
        let containers = vec![container("lakefs", Some("lakefs"), State::Running)];
        let (rows, groups) = group_rows(&containers, vec![0], &no_folds());
        assert_eq!(groups.len(), 1);
        assert_eq!(
            shape(&containers, &rows, &groups),
            vec!["[lakefs]", "lakefs"]
        );
    }

    #[test]
    fn order_within_a_stack_is_left_alone() {
        // Grouping runs after filtering and sorting, so whatever order it is
        // handed has to survive inside each group — otherwise a fuzzy ranking or
        // a cpu sort would be silently undone.
        let containers = fixture();
        let reversed: Vec<usize> = (0..containers.len()).rev().collect();
        let (rows, groups) = group_rows(&containers, reversed, &no_folds());
        let shape = shape(&containers, &rows, &groups);
        let lakefs = shape.iter().position(|s| s == "[lakefs]").unwrap();
        assert_eq!(
            &shape[lakefs + 1..lakefs + 3],
            ["lakefs-postgres", "lakefs"]
        );
    }

    #[test]
    fn a_service_name_shared_by_two_stacks_stays_in_both() {
        // The case that makes the stack label the right key and the service name
        // the wrong one: these two postgres containers are unrelated.
        let containers = vec![
            container("postgres", Some("argilla"), State::Running),
            container("postgres", Some("lakefs"), State::Running),
        ];
        let (rows, groups) = group_rows(&containers, vec![0, 1], &no_folds());
        assert_eq!(groups.len(), 2);
        assert_eq!(
            shape(&containers, &rows, &groups),
            vec!["[argilla]", "postgres", "[lakefs]", "postgres"]
        );
    }
}
