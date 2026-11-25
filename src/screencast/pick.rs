use std::cell::RefCell;
use std::collections::HashMap;
use std::io::{BufRead, BufReader, Write};
use std::path::PathBuf;
use std::process::{Command, Stdio};
use std::rc::Rc;
use std::sync::{
    atomic::{AtomicBool, Ordering},
    Arc,
};
use std::time::Duration;

use futures_util::{stream, StreamExt};
use gtk4::gdk::{self, Display, Key, ModifierType};
use gtk4::gio;
use gtk4::glib;
use gtk4::pango::EllipsizeMode;
use gtk4::prelude::*;
use gtk4::{
    Align, Application, ApplicationWindow, Box as GtkBox, Button, ContentFit, CssProvider,
    EventControllerKey, Fixed, FlowBox, FlowBoxChild, GestureClick, IconTheme, Image, Label,
    Orientation, Overlay, Picture, PolicyType, ScrolledWindow, SelectionMode, Spinner, Stack,
    StackTransitionType, STYLE_PROVIDER_PRIORITY_APPLICATION,
};
#[allow(unused_imports)]
use libadwaita::prelude::*;
use libadwaita::{HeaderBar, ViewStack, ViewSwitcher, ViewSwitcherPolicy};

use super::preview::{self, Thumbnail, DEADLINE, WORKERS};
use super::stop_niri_session;
use crate::niri_ipc::{NiriOutput, NiriWindow, NiriWorkspace};

const PICKER_APP_ID: &str = "io.github.niri.screenshare.picker";
const CARD_ICON_SIZE: i32 = 28;
const PLACEHOLDER_ICON_SIZE: i32 = 34;
const FALLBACK_APP_ICON: &str = "application-x-executable";
const SPATIAL_STAGE_WIDTH: f64 = 740.0;
const SPATIAL_STAGE_HEIGHT: f64 = 400.0;
const SPATIAL_STAGE_PADDING: f64 = 20.0;
const SPATIAL_TILE_GAP: f64 = 4.0;

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum PickerChoice {
    Monitor(String),
    Window(u64),
}

impl PickerChoice {
    pub fn write_to(&self, out: &mut impl Write) -> std::io::Result<()> {
        match self {
            Self::Monitor(name) => writeln!(out, "M:{name}")?,
            Self::Window(id) => writeln!(out, "W:{id}")?,
        }
        out.flush()
    }
}

#[derive(Clone)]
struct DisplayItem {
    name: String,
    x: i32,
    y: i32,
    width: i32,
    height: i32,
}

#[derive(Clone)]
struct WindowItem {
    id: u64,
    title: String,
    app_id: String,
    width: i32,
    height: i32,
    /// Human-readable workspace/output, e.g. "workspace 2 · DP-1". May be empty.
    location: String,
}

/// Cancellation is sticky even if Close arrives before the child is spawned.
#[derive(Default)]
pub struct PickerChild {
    child: Option<std::process::Child>,
    cancelled: bool,
}

pub type PickerChildSlot = Arc<std::sync::Mutex<PickerChild>>;

pub fn show_picker_cancellable(
    outputs: &[NiriOutput],
    windows: &[NiriWindow],
    child_slot: PickerChildSlot,
    conn: &zbus::Connection,
) -> Option<PickerChoice> {
    supervise_picker(
        outputs.iter().map(DisplayItem::from).collect(),
        window_items_with_niri_workspaces(windows),
        child_slot,
        conn,
    )
}

fn supervise_picker(
    displays: Vec<DisplayItem>,
    wins: Vec<WindowItem>,
    child_slot: PickerChildSlot,
    conn: &zbus::Connection,
) -> Option<PickerChoice> {
    if displays.is_empty() && wins.is_empty() {
        return None;
    }

    // Serialize spawning with cancellation so a closed request cannot leave a
    // newly spawned chooser behind. Release the lock before writing its input.
    let (stdin, stdout) = {
        let mut slot = child_slot.lock().ok()?;
        if slot.cancelled {
            return None;
        }
        // Even one possible target still requires explicit user consent.
        let bin = picker_bin();
        let mut child = match Command::new(&bin)
            .arg("--picker-ui")
            .stdin(Stdio::piped())
            .stdout(Stdio::piped())
            .stderr(Stdio::inherit())
            .spawn()
        {
            Ok(child) => child,
            Err(e) => {
                tracing::error!("failed to spawn picker {}: {e}", bin.display());
                return None;
            }
        };
        let pipes = (child.stdin.take(), child.stdout.take());
        slot.child = Some(child);
        pipes
    };

    let mut stdin = stdin?;
    if write_picker_targets(&mut stdin, &displays, &wins).is_err() {
        kill_slotted_picker(&child_slot);
        return None;
    }

    let targets: Vec<_> = displays
        .iter()
        .map(|d| PickerChoice::Monitor(d.name.clone()))
        .chain(wins.iter().map(|w| PickerChoice::Window(w.id)))
        .collect();
    let choice = serve_picker(conn, &targets, BufReader::new(stdout?), stdin);
    let mut slot = child_slot.lock().ok()?;
    let status = slot.child.take()?.wait().ok()?;
    choice.filter(|_| status.success() && !slot.cancelled)
}

/// Only targets supplied to this picker can be previewed or returned as a choice.
pub(super) fn serve_picker(
    conn: &zbus::Connection,
    targets: &[PickerChoice],
    commands: impl BufRead,
    replies: impl Write,
) -> Option<PickerChoice> {
    let rt = tokio::runtime::Handle::current();
    let stop = |path: &str| {
        rt.block_on(async {
            if tokio::time::timeout(DEADLINE, stop_niri_session(conn, path))
                .await
                .is_err()
            {
                tracing::warn!("timed out stopping preview session {path}");
            }
        })
    };
    let mut replies = Some(replies);
    let mut sessions = HashMap::new();
    let mut choice = None;
    for line in commands.lines().map_while(Result::ok) {
        if let Some(index) = line
            .strip_prefix("OPEN ")
            .and_then(|s| s.parse::<usize>().ok())
        {
            let Some(writer) = replies.as_mut() else {
                continue;
            };
            let mut node = None;
            if let Some(target) = targets.get(index) {
                if sessions.len() < WORKERS && !sessions.contains_key(&index) {
                    match rt.block_on(super::preview::open_session(conn, target)) {
                        Ok((session, id)) => {
                            sessions.insert(index, session);
                            node = Some(id);
                        }
                        Err(e) => tracing::debug!("could not start preview: {e:#}"),
                    }
                }
            }
            if writeln!(
                writer,
                "{}",
                node.map(|id| id.to_string()).unwrap_or_default()
            )
            .is_err()
            {
                // A final choice can still be buffered on the child's stdout.
                replies = None;
            }
        } else if let Some(index) = line
            .strip_prefix("STOP ")
            .and_then(|s| s.parse::<usize>().ok())
        {
            if let Some(path) = sessions.remove(&index) {
                stop(&path);
            }
        } else if let Some(name) = line.strip_prefix("M:") {
            choice = Some(PickerChoice::Monitor(name.to_owned()));
        } else if let Some(id) = line.strip_prefix("W:").and_then(|s| s.parse().ok()) {
            choice = Some(PickerChoice::Window(id));
        }
    }
    // EOF also covers a killed picker, including a lost reply to OPEN.
    for path in sessions.into_values() {
        stop(&path);
    }
    choice.filter(|target| targets.contains(target))
}

pub fn kill_slotted_picker(slot: &PickerChildSlot) {
    if let Ok(mut slot) = slot.lock() {
        slot.cancelled = true;
        if let Some(mut child) = slot.child.take() {
            tracing::info!("killing in-flight picker pid={}", child.id());
            let _ = child.kill();
            let _ = child.wait();
        }
    }
}

/// CLI entry for `--picker` / `--debug-picker`. Targets from stdin, else niri IPC.
pub fn run_picker_process(ui: bool) -> Option<PickerChoice> {
    if ui {
        let (displays, windows) = read_targets_from_stdin()?;
        run_gtk_application(displays, windows);
    }
    let rt = tokio::runtime::Builder::new_multi_thread()
        .worker_threads(2)
        .enable_all()
        .build()
        .ok()?;
    let _enter = rt.enter();
    let (displays, windows) = read_targets_or_query_niri();
    let conn = rt.block_on(zbus::Connection::session()).ok()?;
    supervise_picker(displays, windows, PickerChildSlot::default(), &conn)
}

fn picker_bin() -> PathBuf {
    // Prefer argv[0] so a Nix GApps wrapper is reused when packaged.
    if let Some(arg0) = std::env::args_os().next() {
        let p = PathBuf::from(arg0);
        if p.exists() {
            return p;
        }
    }
    std::env::current_exe().unwrap_or_else(|_| PathBuf::from("niri-screenshare"))
}

fn write_picker_targets(
    out: &mut impl Write,
    displays: &[DisplayItem],
    windows: &[WindowItem],
) -> std::io::Result<()> {
    // D:name\tw\th\tx\ty  W:id\ttitle\tapp\tw\th\tlocation  END
    for d in displays {
        writeln!(
            out,
            "D:{}\t{}\t{}\t{}\t{}",
            escape_field(&d.name),
            d.width,
            d.height,
            d.x,
            d.y
        )?;
    }
    for w in windows {
        writeln!(
            out,
            "W:{}\t{}\t{}\t{}\t{}\t{}",
            w.id,
            escape_field(&w.title),
            escape_field(&w.app_id),
            w.width,
            w.height,
            escape_field(&w.location)
        )?;
    }
    writeln!(out, "END")?;
    out.flush()
}

fn escape_field(s: &str) -> String {
    s.replace('\\', "\\\\")
        .replace('\t', "\\t")
        .replace('\n', "\\n")
}

fn unescape_field(s: &str) -> String {
    let mut out = String::with_capacity(s.len());
    let mut chars = s.chars();
    while let Some(c) = chars.next() {
        if c == '\\' {
            match chars.next() {
                Some('\\') => out.push('\\'),
                Some('t') => out.push('\t'),
                Some('n') => out.push('\n'),
                Some(other) => {
                    out.push('\\');
                    out.push(other);
                }
                None => out.push('\\'),
            }
        } else {
            out.push(c);
        }
    }
    out
}

fn read_targets_or_query_niri() -> (Vec<DisplayItem>, Vec<WindowItem>) {
    if let Some(targets) = read_targets_from_stdin() {
        return targets;
    }
    let displays = crate::niri_ipc::list_outputs()
        .unwrap_or_default()
        .iter()
        .map(DisplayItem::from)
        .collect();
    let windows =
        window_items_with_niri_workspaces(&crate::niri_ipc::list_windows().unwrap_or_default());
    (displays, windows)
}

/// Workspace info is only decoration, so a failed lookup still lists windows.
fn window_items_with_niri_workspaces(windows: &[NiriWindow]) -> Vec<WindowItem> {
    if windows.is_empty() {
        return Vec::new();
    }
    let workspaces = crate::niri_ipc::list_workspaces().unwrap_or_else(|e| {
        tracing::warn!("list_workspaces failed: {e}");
        Vec::new()
    });
    window_items(windows, &workspaces)
}

fn window_items(windows: &[NiriWindow], workspaces: &[NiriWorkspace]) -> Vec<WindowItem> {
    windows
        .iter()
        .map(|w| {
            let mut item = WindowItem::from(w);
            item.location = w
                .workspace_id
                .and_then(|id| workspaces.iter().find(|ws| ws.id == id))
                .map(workspace_label)
                .unwrap_or_default();
            item
        })
        .collect()
}

fn workspace_label(ws: &NiriWorkspace) -> String {
    let mut label = match ws.name.as_deref() {
        Some(name) if !name.is_empty() => format!("workspace {name}"),
        _ => format!("workspace {}", ws.idx),
    };
    if let Some(output) = ws.output.as_deref().filter(|o| !o.is_empty()) {
        label.push_str(" · ");
        label.push_str(output);
    }
    label
}

fn read_targets_from_stdin() -> Option<(Vec<DisplayItem>, Vec<WindowItem>)> {
    use std::io::IsTerminal;
    if std::io::stdin().is_terminal() {
        return None;
    }
    parse_targets_stream(std::io::stdin().lock())
}

fn parse_targets_stream<R: BufRead>(reader: R) -> Option<(Vec<DisplayItem>, Vec<WindowItem>)> {
    let mut displays = Vec::new();
    let mut windows = Vec::new();
    for line in reader.lines().map_while(Result::ok) {
        let line = line.trim_end().to_string();
        if line == "END" {
            return Some((displays, windows));
        }
        if let Some(rest) = line.strip_prefix("D:") {
            let parts: Vec<&str> = rest.split('\t').collect();
            if parts.len() >= 3 {
                displays.push(DisplayItem {
                    name: unescape_field(parts[0]),
                    width: parts[1].parse().unwrap_or(0),
                    height: parts[2].parse().unwrap_or(0),
                    x: parts.get(3).and_then(|p| p.parse().ok()).unwrap_or(0),
                    y: parts.get(4).and_then(|p| p.parse().ok()).unwrap_or(0),
                });
            }
        } else if let Some(rest) = line.strip_prefix("W:") {
            let parts: Vec<&str> = rest.split('\t').collect();
            if parts.len() >= 5 {
                if let Ok(id) = parts[0].parse::<u64>() {
                    windows.push(WindowItem {
                        id,
                        title: unescape_field(parts[1]),
                        app_id: unescape_field(parts[2]),
                        width: parts[3].parse().unwrap_or(0),
                        height: parts[4].parse().unwrap_or(0),
                        location: parts.get(5).map(|p| unescape_field(p)).unwrap_or_default(),
                    });
                }
            }
        }
    }
    if displays.is_empty() && windows.is_empty() {
        None
    } else {
        Some((displays, windows))
    }
}

#[derive(Clone)]
struct PreviewSlot {
    stack: Stack,
    picture: Picture,
    spinner: Spinner,
}

impl PreviewSlot {
    fn set_loading(&self) {
        self.spinner.set_visible(true);
        self.spinner.start();
    }

    fn set_thumbnail(&self, frame: Thumbnail) {
        let texture = gdk::MemoryTexture::new(
            frame.width,
            frame.height,
            gdk::MemoryFormat::R8g8b8a8,
            &glib::Bytes::from_owned(frame.rgba),
            frame.width as usize * 4,
        );
        self.picture.set_paintable(Some(&texture));
        self.spinner.stop();
        self.spinner.set_visible(false);
        self.stack.set_visible_child_name("preview");
    }

    fn set_failed(&self) {
        self.spinner.stop();
        self.spinner.set_visible(false);
        self.picture.set_paintable(None::<&gdk::Paintable>);
        self.stack.set_visible_child_name("placeholder");
    }
}

#[derive(Debug, Clone, PartialEq)]
struct ProjectedMonitor {
    index: usize,
    x: f64,
    y: f64,
    width: f64,
    height: f64,
}

/// Projects monitors' logical `(x, y, width, height)` rectangles into a
/// centered stage while preserving relative layout and aspect ratios.
fn project_monitor_layout(
    displays: &[DisplayItem],
    stage_w: f64,
    stage_h: f64,
    padding: f64,
) -> Vec<ProjectedMonitor> {
    if displays.is_empty() {
        return Vec::new();
    }

    let mut min_x = f64::INFINITY;
    let mut min_y = f64::INFINITY;
    let mut max_x = f64::NEG_INFINITY;
    let mut max_y = f64::NEG_INFINITY;

    for d in displays {
        let w = d.width.max(1) as f64;
        let h = d.height.max(1) as f64;
        let x = d.x as f64;
        let y = d.y as f64;
        min_x = min_x.min(x);
        min_y = min_y.min(y);
        max_x = max_x.max(x + w);
        max_y = max_y.max(y + h);
    }

    let total_w = (max_x - min_x).max(1.0);
    let total_h = (max_y - min_y).max(1.0);
    let avail_w = (stage_w - padding * 2.0).max(1.0);
    let avail_h = (stage_h - padding * 2.0).max(1.0);
    let scale = (avail_w / total_w).min(avail_h / total_h);

    let origin_x = (stage_w - total_w * scale) / 2.0;
    let origin_y = (stage_h - total_h * scale) / 2.0;
    let inset = if displays.len() > 1 {
        SPATIAL_TILE_GAP / 2.0
    } else {
        0.0
    };

    displays
        .iter()
        .enumerate()
        .map(|(index, d)| {
            let raw_w = d.width.max(1) as f64 * scale;
            let raw_h = d.height.max(1) as f64 * scale;
            ProjectedMonitor {
                index,
                x: origin_x + (d.x as f64 - min_x) * scale + inset,
                y: origin_y + (d.y as f64 - min_y) * scale + inset,
                width: (raw_w - inset * 2.0).max(28.0),
                height: (raw_h - inset * 2.0).max(24.0),
            }
        })
        .collect()
}

/// Fits a `(width, height)` target into a `(max_w, max_h)` preview slot while
/// preserving its aspect ratio (clamped to avoid hairline slivers).
fn fit_aspect_box(max_w: i32, max_h: i32, width: i32, height: i32) -> (i32, i32) {
    let ratio = if width <= 0 || height <= 0 {
        16.0_f64 / 9.0
    } else {
        (width as f64 / height as f64).clamp(0.45, 2.6)
    };
    let max_w = max_w.max(1) as f64;
    let max_h = max_h.max(1) as f64;
    let (w, h) = if max_w / max_h > ratio {
        (max_h * ratio, max_h)
    } else {
        (max_w, max_w / ratio)
    };
    ((w.round() as i32).max(1), (h.round() as i32).max(1))
}

fn run_gtk_application(displays: Vec<DisplayItem>, windows: Vec<WindowItem>) -> ! {
    let result: Rc<RefCell<Option<PickerChoice>>> = Rc::new(RefCell::new(None));
    let app = Application::builder()
        .application_id(PICKER_APP_ID)
        .flags(gio::ApplicationFlags::NON_UNIQUE)
        .build();

    let result_activate = result.clone();
    app.connect_activate(move |app| {
        build_and_present(
            app,
            displays.clone(),
            windows.clone(),
            result_activate.clone(),
        );
    });

    let exit = app.run_with_args::<&str>(&[]);
    if exit != glib::ExitCode::SUCCESS {
        tracing::warn!("picker gtk application exited with {exit:?}");
    }

    let accepted = exit == glib::ExitCode::SUCCESS
        && result
            .borrow()
            .as_ref()
            .is_some_and(|choice| choice.write_to(&mut std::io::stdout().lock()).is_ok());

    // Only the private UI child exits here. GL workers may still be running,
    // so skip driver atexit handlers. The supervisor stops sessions on EOF.
    // SAFETY: _exit takes only an exit status and never returns.
    unsafe { libc::_exit(if accepted { 0 } else { 1 }) }
}

fn build_and_present(
    app: &Application,
    displays: Vec<DisplayItem>,
    windows: Vec<WindowItem>,
    result: Rc<RefCell<Option<PickerChoice>>>,
) {
    let _ = libadwaita::init();
    install_css();

    let selection: Rc<RefCell<Option<PickerChoice>>> = Rc::new(RefCell::new(None));
    let mut preview_slots = Vec::new();
    let closed = Arc::new(AtomicBool::new(false));
    let window = ApplicationWindow::builder()
        .application(app)
        .title("Screen Sharing")
        .default_width(800)
        .default_height(600)
        .build();

    let header = HeaderBar::new();

    let stack = ViewStack::new();
    stack.set_vexpand(true);

    let switcher = ViewSwitcher::builder()
        .stack(&stack)
        .policy(ViewSwitcherPolicy::Wide)
        .build();
    header.set_title_widget(Some(&switcher));

    let refresh_btn = Button::from_icon_name("view-refresh-symbolic");
    refresh_btn.set_tooltip_text(Some("Refresh previews (F5)"));
    refresh_btn.add_css_class("flat");
    let header_spinner = Spinner::new();
    header_spinner.set_visible(false);
    let header_start = GtkBox::new(Orientation::Horizontal, 6);
    header_start.append(&refresh_btn);
    header_start.append(&header_spinner);
    header.pack_start(&header_start);

    let cancel = Button::with_label("Cancel");
    cancel.add_css_class("pill");
    let share = Button::with_label("Share");
    share.add_css_class("pill");
    share.add_css_class("suggested-action");
    share.set_sensitive(false);

    let finish: Rc<dyn Fn(bool)> = Rc::new({
        let result = result.clone();
        let selection = selection.clone();
        let window = window.clone();
        let closed = closed.clone();
        move |accepted: bool| {
            closed.store(true, Ordering::Relaxed);
            if accepted {
                *result.borrow_mut() = selection.borrow().clone();
            }
            window.close();
        }
    });

    let window_flow = FlowBox::new();
    let selected_monitor = Rc::new(glib::WeakRef::<Overlay>::new());
    let apply_selection: Rc<dyn Fn(Option<PickerChoice>)> = Rc::new({
        let selection = selection.clone();
        let share = share.clone();
        move |choice| {
            share.set_sensitive(choice.is_some());
            *selection.borrow_mut() = choice;
        }
    });

    let displays_container = build_displays_page(
        &displays,
        &selected_monitor,
        &mut preview_slots,
        &apply_selection,
        &finish,
    );
    let displays_page = stack.add_titled(&displays_container, Some("displays"), "Displays");
    displays_page.set_icon_name(Some("video-display-symbolic"));

    let windows_container = build_windows_page(
        &windows,
        &window_flow,
        &mut preview_slots,
        &apply_selection,
        &finish,
    );
    let window_scroll = ScrolledWindow::builder()
        .child(&windows_container)
        .hscrollbar_policy(PolicyType::Never)
        .vexpand(true)
        .build();
    let windows_page = stack.add_titled(&window_scroll, Some("windows"), "Windows");
    windows_page.set_icon_name(Some("window-symbolic"));

    stack.set_visible_child_name(initial_page(!displays.is_empty(), !windows.is_empty()));

    let button_row = GtkBox::new(Orientation::Horizontal, 12);
    button_row.set_halign(Align::End);
    button_row.set_margin_top(6);
    button_row.set_margin_bottom(12);
    button_row.set_margin_start(16);
    button_row.set_margin_end(16);
    button_row.append(&cancel);
    button_row.append(&share);

    let content = GtkBox::new(Orientation::Vertical, 0);
    content.append(&header);
    content.append(&stack);
    content.append(&button_row);
    window.set_child(Some(&content));

    // Switching tabs invalidates the previous page's consent target.
    stack.connect_visible_child_name_notify({
        let window_flow = window_flow.clone();
        let apply_selection = apply_selection.clone();
        move |_| {
            window_flow.unselect_all();
            if let Some(tile) = selected_monitor.upgrade() {
                tile.remove_css_class("selected");
            }
            selected_monitor.set(None);
            apply_selection(None);
        }
    });

    let previews_enabled = !std::env::var("NIRI_SCREENSHARE_NO_PREVIEWS").is_ok_and(|v| v == "1")
        && !crate::niri_ipc::output_is_being_cast();
    refresh_btn.set_visible(previews_enabled);
    let slots = Rc::new(preview_slots);
    let display_count = displays.len();
    refresh_btn.connect_clicked({
        let closed = closed.clone();
        let stack = stack.clone();
        move |button| {
            if !previews_enabled || !button.is_sensitive() {
                return;
            }
            button.set_sensitive(false);
            for slot in slots.iter() {
                slot.set_loading();
            }
            header_spinner.set_visible(true);
            header_spinner.start();
            let mut targets: Vec<_> = (0..slots.len()).collect();
            if stack.visible_child_name().as_deref() == Some("windows") {
                targets.rotate_left(display_count);
            }
            let closed = closed.clone();
            let slots = slots.clone();
            let button = button.clone();
            let spinner = header_spinner.clone();
            glib::spawn_future_local(async move {
                stream::iter(targets)
                    .for_each_concurrent(WORKERS, |index| {
                        let closed = closed.clone();
                        let slot = &slots[index];
                        async move {
                            if closed.load(Ordering::Relaxed) {
                                return;
                            }
                            let frame =
                                gio::spawn_blocking(move || preview::capture(index, &closed)).await;
                            match frame {
                                Ok(Ok(frame)) => slot.set_thumbnail(frame),
                                Ok(Err(e)) => {
                                    tracing::debug!("target {index} preview failed: {e:#}");
                                    slot.set_failed();
                                }
                                Err(_) => slot.set_failed(),
                            }
                        }
                    })
                    .await;
                spinner.stop();
                spinner.set_visible(false);
                button.set_sensitive(true);
            });
        }
    });

    cancel.connect_clicked({
        let finish = finish.clone();
        move |_| finish(false)
    });
    share.connect_clicked({
        let finish = finish.clone();
        let selection = selection.clone();
        move |_| {
            if selection.borrow().is_some() {
                finish(true);
            }
        }
    });

    let key_controller = EventControllerKey::new();
    key_controller.connect_key_pressed({
        let finish = finish.clone();
        let refresh_btn = refresh_btn.clone();
        move |_, key, _, mods| match key {
            Key::Escape => {
                finish(false);
                glib::Propagation::Stop
            }
            Key::F5 => {
                refresh_btn.emit_clicked();
                glib::Propagation::Stop
            }
            Key::r | Key::R if mods.contains(ModifierType::CONTROL_MASK) => {
                refresh_btn.emit_clicked();
                glib::Propagation::Stop
            }
            _ => glib::Propagation::Proceed,
        }
    });
    window.add_controller(key_controller);

    window.connect_close_request({
        move |_| {
            closed.store(true, Ordering::Relaxed);
            glib::Propagation::Proceed
        }
    });

    // niri floats fixed-size windows at map time, so opening non-resizable makes
    // the picker come up floating instead of being tiled and moved afterwards.
    // Resizability is handed back once it is mapped; the floating state sticks.
    window.set_resizable(false);
    window.connect_map({
        move |window| {
            let window = window.clone();
            glib::timeout_add_local_once(Duration::from_millis(150), move || {
                window.set_resizable(true);
            });
            refresh_btn.emit_clicked();
        }
    });
    window.present();
}

fn build_displays_page(
    displays: &[DisplayItem],
    selected_monitor: &Rc<glib::WeakRef<Overlay>>,
    preview_slots: &mut Vec<PreviewSlot>,
    apply_selection: &Rc<dyn Fn(Option<PickerChoice>)>,
    finish: &Rc<dyn Fn(bool)>,
) -> GtkBox {
    let container = GtkBox::new(Orientation::Vertical, 0);
    container.set_margin_top(12);
    container.set_margin_bottom(12);
    container.set_margin_start(16);
    container.set_margin_end(16);
    container.set_vexpand(true);

    if displays.is_empty() {
        container.append(&empty_label("No displays found"));
        return container;
    }

    let wrapper = GtkBox::new(Orientation::Vertical, 0);
    wrapper.add_css_class("monitor-stage");
    wrapper.set_halign(Align::Fill);
    wrapper.set_valign(Align::Fill);

    let fixed = Fixed::new();
    fixed.set_size_request(SPATIAL_STAGE_WIDTH as i32, SPATIAL_STAGE_HEIGHT as i32);
    fixed.set_halign(Align::Center);
    fixed.set_valign(Align::Center);
    fixed.set_vexpand(true);

    let projected = project_monitor_layout(
        displays,
        SPATIAL_STAGE_WIDTH,
        SPATIAL_STAGE_HEIGHT,
        SPATIAL_STAGE_PADDING,
    );

    for rect in projected {
        let d = &displays[rect.index];
        let tile = Overlay::new();
        tile.add_css_class("spatial-monitor");
        tile.set_focusable(true);
        tile.set_size_request(rect.width.round() as i32, rect.height.round() as i32);
        tile.set_overflow(gtk4::Overflow::Hidden);
        tile.set_tooltip_text(Some(&format!("{} ({}×{})", d.name, d.width, d.height)));

        let icon_size = PLACEHOLDER_ICON_SIZE
            .min(rect.width.min(rect.height).round() as i32 / 3)
            .max(18);
        let (slot_widget, slot) = build_preview_slot(
            Image::from_icon_name("video-display-symbolic"),
            ContentFit::Contain,
            icon_size,
        );
        tile.set_child(Some(&slot_widget));
        preview_slots.push(slot);

        let badge = GtkBox::new(Orientation::Vertical, 1);
        badge.add_css_class("spatial-badge");
        badge.set_halign(Align::Center);
        badge.set_valign(Align::End);
        badge.set_margin_bottom(8);
        badge.set_margin_start(6);
        badge.set_margin_end(6);

        let name_lbl = Label::new(Some(&d.name));
        name_lbl.add_css_class("spatial-badge-title");
        name_lbl.set_ellipsize(EllipsizeMode::End);
        badge.append(&name_lbl);

        if rect.height >= 72.0 {
            let res_lbl = Label::new(Some(&format!("{}×{}", d.width, d.height)));
            res_lbl.add_css_class("spatial-badge-sub");
            res_lbl.set_ellipsize(EllipsizeMode::End);
            badge.append(&res_lbl);
        }
        tile.add_overlay(&badge);

        let click = GestureClick::new();
        let monitor_name = d.name.clone();
        let apply_sel = apply_selection.clone();
        let finish_cb = finish.clone();
        let selected_monitor = selected_monitor.clone();
        let activate = Rc::new(move |tile: &Overlay, accept: bool| {
            if let Some(previous) = selected_monitor.upgrade() {
                previous.remove_css_class("selected");
            }
            tile.grab_focus();
            tile.add_css_class("selected");
            selected_monitor.set(Some(tile));
            apply_sel(Some(PickerChoice::Monitor(monitor_name.clone())));
            if accept {
                finish_cb(true);
            }
        });
        let on_click = activate.clone();
        click.connect_pressed(move |controller, presses, _, _| {
            if let Some(tile) = controller.widget().and_downcast::<Overlay>() {
                on_click(&tile, presses >= 2);
            }
        });
        tile.add_controller(click);

        let key_ctrl = EventControllerKey::new();
        key_ctrl.connect_key_pressed(move |controller, key, _, _| {
            let accept = match key {
                Key::space => false,
                Key::Return | Key::KP_Enter => true,
                _ => return glib::Propagation::Proceed,
            };
            if let Some(tile) = controller.widget().and_downcast::<Overlay>() {
                activate(&tile, accept);
            }
            glib::Propagation::Stop
        });
        tile.add_controller(key_ctrl);

        fixed.put(&tile, rect.x, rect.y);
    }

    wrapper.append(&fixed);
    container.append(&wrapper);
    container
}

fn build_windows_page(
    windows: &[WindowItem],
    flow: &FlowBox,
    preview_slots: &mut Vec<PreviewSlot>,
    apply_selection: &Rc<dyn Fn(Option<PickerChoice>)>,
    finish: &Rc<dyn Fn(bool)>,
) -> GtkBox {
    let container = GtkBox::new(Orientation::Vertical, 12);
    container.set_margin_top(12);
    container.set_margin_bottom(12);
    container.set_margin_start(16);
    container.set_margin_end(16);

    if windows.is_empty() {
        container.append(&empty_label("No windows found"));
        return container;
    }

    configure_preview_grid(flow);
    let icon_theme = Display::default().map(|d| IconTheme::for_display(&d));
    for w in windows {
        let card = build_window_card(w, icon_theme.as_ref(), preview_slots);
        let child = FlowBoxChild::new();
        child.set_child(Some(&card));
        child.add_css_class("preview-flow-child");
        flow.insert(&child, -1);
    }

    let items: Rc<[_]> = windows.iter().map(|w| w.id).collect();
    let selected_items = items.clone();
    let apply_sel = apply_selection.clone();
    flow.connect_selected_children_changed(move |fb| {
        let choice = fb
            .selected_children()
            .first()
            .and_then(|c| selected_items.get(c.index() as usize))
            .copied()
            .map(PickerChoice::Window);
        apply_sel(choice);
    });

    let apply_sel = apply_selection.clone();
    let finish_cb = finish.clone();
    flow.connect_child_activated(move |_, child| {
        if let Some(&id) = items.get(child.index() as usize) {
            apply_sel(Some(PickerChoice::Window(id)));
            finish_cb(true);
        }
    });

    container.append(flow);
    container
}

fn build_window_card(
    w: &WindowItem,
    icon_theme: Option<&IconTheme>,
    preview_slots: &mut Vec<PreviewSlot>,
) -> GtkBox {
    let card = GtkBox::new(Orientation::Vertical, 0);
    card.add_css_class("preview-card");
    card.set_overflow(gtk4::Overflow::Hidden);

    let title = window_title(w);
    let subtitle = window_subtitle(w, &title);
    card.set_tooltip_text(Some(&title));

    let stage = GtkBox::new(Orientation::Vertical, 0);
    stage.add_css_class("preview-stage");
    stage.set_size_request(-1, 152);
    stage.set_valign(Align::Center);

    let (frame_w, frame_h) = fit_aspect_box(228, 132, w.width, w.height);
    let icon_size = PLACEHOLDER_ICON_SIZE.min(frame_w.min(frame_h) / 2).max(18);
    let frame_box = build_geometry_frame(frame_w, frame_h);
    let (slot_widget, slot) = build_preview_slot(
        app_icon(&w.app_id, icon_theme, icon_size),
        ContentFit::Contain,
        icon_size,
    );
    frame_box.append(&slot_widget);
    stage.append(&frame_box);
    preview_slots.push(slot);
    card.append(&stage);

    let icon = app_icon(&w.app_id, icon_theme, CARD_ICON_SIZE);
    card.append(&build_card_footer(&icon, &title, &subtitle));

    card
}

fn configure_preview_grid(flow: &FlowBox) {
    flow.set_selection_mode(SelectionMode::Single);
    flow.set_activate_on_single_click(false);
    flow.set_homogeneous(true);
    flow.set_min_children_per_line(2);
    flow.set_max_children_per_line(3);
    flow.set_row_spacing(12);
    flow.set_column_spacing(12);
    flow.set_valign(Align::Start);
}

fn build_geometry_frame(width: i32, height: i32) -> GtkBox {
    let frame = GtkBox::new(Orientation::Vertical, 0);
    frame.add_css_class("geometry-frame");
    frame.set_size_request(width, height);
    frame.set_halign(Align::Center);
    frame.set_valign(Align::Center);
    frame.set_margin_top(10);
    frame.set_margin_bottom(10);
    frame.set_margin_start(10);
    frame.set_margin_end(10);
    frame.set_overflow(gtk4::Overflow::Hidden);
    frame
}

fn build_card_footer(icon: &Image, title: &str, subtitle: &str) -> GtkBox {
    let footer = GtkBox::new(Orientation::Horizontal, 10);
    footer.set_margin_top(10);
    footer.set_margin_bottom(10);
    footer.set_margin_start(12);
    footer.set_margin_end(12);
    footer.set_valign(Align::Center);
    footer.append(icon);

    let text_col = GtkBox::new(Orientation::Vertical, 2);
    text_col.set_hexpand(true);

    let title_lbl = Label::new(Some(title));
    title_lbl.add_css_class("card-title");
    title_lbl.set_halign(Align::Start);
    title_lbl.set_ellipsize(EllipsizeMode::End);
    text_col.append(&title_lbl);

    if !subtitle.is_empty() {
        let sub_lbl = Label::new(Some(subtitle));
        sub_lbl.add_css_class("dim-label");
        sub_lbl.add_css_class("caption");
        sub_lbl.set_halign(Align::Start);
        sub_lbl.set_ellipsize(EllipsizeMode::End);
        text_col.append(&sub_lbl);
    }

    footer.append(&text_col);
    footer
}

fn build_preview_slot(
    placeholder_icon: Image,
    fit: ContentFit,
    icon_size: i32,
) -> (Stack, PreviewSlot) {
    let stack = Stack::new();
    stack.set_transition_type(StackTransitionType::Crossfade);
    stack.set_transition_duration(140);
    stack.set_hexpand(true);
    stack.set_vexpand(true);

    let placeholder = GtkBox::new(Orientation::Vertical, 6);
    placeholder.set_halign(Align::Center);
    placeholder.set_valign(Align::Center);
    placeholder.set_hexpand(true);
    placeholder.set_vexpand(true);

    placeholder_icon.set_pixel_size(icon_size);
    placeholder_icon.add_css_class("dim-label");
    placeholder.append(&placeholder_icon);

    let spinner = Spinner::new();
    spinner.set_size_request(16, 16);
    spinner.set_visible(false);
    placeholder.append(&spinner);

    let picture = Picture::new();
    picture.set_content_fit(fit);
    picture.set_can_shrink(true);
    picture.set_hexpand(true);
    picture.set_vexpand(true);

    stack.add_named(&placeholder, Some("placeholder"));
    stack.add_named(&picture, Some("preview"));
    stack.set_visible_child_name("placeholder");

    let slot = PreviewSlot {
        stack: stack.clone(),
        picture,
        spinner,
    };
    (stack, slot)
}

fn install_css() {
    let Some(display) = Display::default() else {
        return;
    };
    let provider = CssProvider::new();
    provider.load_from_data(
        "
        headerbar { min-height: 48px; }
        .monitor-stage {
            background-color: alpha(@window_fg_color, 0.05);
            border: 1px solid alpha(@window_fg_color, 0.10);
            border-radius: 12px;
            padding: 4px;
        }
        .spatial-monitor {
            background-color: alpha(@window_fg_color, 0.10);
            border: 2px solid alpha(@window_fg_color, 0.25);
            border-radius: 8px;
            transition: border-color 120ms ease, box-shadow 120ms ease;
        }
        .spatial-monitor:hover { border-color: alpha(@accent_bg_color, 0.70); }
        .spatial-monitor.selected {
            border-color: @accent_bg_color;
            box-shadow: 0 0 0 2px alpha(@accent_bg_color, 0.30);
        }
        .spatial-badge {
            background-color: rgba(16, 16, 20, 0.78);
            color: #ffffff;
            border-radius: 7px;
            padding: 3px 8px;
        }
        .spatial-badge-title {
            font-size: 11px;
            font-weight: 700;
        }
        .spatial-badge-sub {
            font-size: 10px;
            opacity: 0.80;
            font-variant-numeric: tabular-nums;
        }
        .preview-flow-child { padding: 0; border-radius: 12px; background: transparent; }
        .preview-flow-child:selected { background: transparent; outline: none; }
        .preview-card {
            background-color: @card_bg_color;
            border: 2px solid alpha(@window_fg_color, 0.10);
            border-radius: 12px;
            transition: border-color 120ms ease, box-shadow 120ms ease;
        }
        .preview-flow-child:hover .preview-card { border-color: alpha(@accent_bg_color, 0.55); }
        .preview-flow-child:selected .preview-card {
            border-color: @accent_bg_color;
            box-shadow: 0 0 0 2px alpha(@accent_bg_color, 0.30);
        }
        .preview-stage {
            background-color: alpha(@window_fg_color, 0.05);
            border-bottom: 1px solid alpha(@window_fg_color, 0.08);
        }
        .geometry-frame {
            background-color: rgba(16, 16, 20, 0.40);
            border: 1px solid alpha(@window_fg_color, 0.20);
            border-radius: 6px;
        }
        .card-title { font-weight: 600; }
        ",
    );
    gtk4::style_context_add_provider_for_display(
        &display,
        &provider,
        STYLE_PROVIDER_PRIORITY_APPLICATION,
    );
}

/// Opens the picker on a tab with available targets. SelectSources filters
/// the lists to the requested source types, so an empty side is either
/// unrequested or has no currently available targets.
fn initial_page(has_displays: bool, has_windows: bool) -> &'static str {
    if !has_displays && has_windows {
        "windows"
    } else {
        "displays"
    }
}

fn window_title(w: &WindowItem) -> String {
    if w.title.is_empty() {
        w.app_id.clone()
    } else {
        w.title.clone()
    }
}

/// "app · workspace 2 · DP-1", skipping parts that are empty or repeat the title.
fn window_subtitle(w: &WindowItem, title: &str) -> String {
    let mut parts: Vec<String> = Vec::new();
    if !w.app_id.is_empty() && w.app_id != title {
        parts.push(w.app_id.clone());
    }
    if !w.location.is_empty() {
        parts.push(w.location.clone());
    }
    parts.join(" · ")
}

/// Desktop-file and icon-theme names commonly derived from a Wayland app_id,
/// e.g. "org.mozilla.firefox" -> also "firefox".
fn icon_name_candidates(app_id: &str) -> Vec<String> {
    let mut out: Vec<String> = Vec::new();
    let mut push = |s: String| {
        if !s.is_empty() && !out.contains(&s) {
            out.push(s);
        }
    };
    push(app_id.to_string());
    push(app_id.to_lowercase());
    if let Some(last) = app_id.rsplit('.').next() {
        push(last.to_string());
        push(last.to_lowercase());
    }
    out
}

fn app_icon(app_id: &str, theme: Option<&IconTheme>, size: i32) -> Image {
    let image = resolve_app_icon(app_id, theme)
        .map(|icon| Image::from_gicon(&icon))
        .unwrap_or_else(|| Image::from_icon_name(FALLBACK_APP_ICON));
    image.set_pixel_size(size);
    image
}

fn resolve_app_icon(app_id: &str, theme: Option<&IconTheme>) -> Option<gio::Icon> {
    let candidates = icon_name_candidates(app_id);
    // The desktop file's Icon= is the most reliable source; many apps' icon
    // names differ from their app_id.
    for id in &candidates {
        if let Some(icon) =
            gio::DesktopAppInfo::new(&format!("{id}.desktop")).and_then(|info| info.icon())
        {
            return Some(icon);
        }
    }
    let theme = theme?;
    candidates
        .iter()
        .find(|id| theme.has_icon(id))
        .map(|id| gio::ThemedIcon::new(id).upcast())
}

fn empty_label(text: &str) -> Label {
    let label = Label::new(Some(text));
    label.add_css_class("dim-label");
    label.set_margin_top(24);
    label.set_margin_bottom(24);
    label.set_halign(Align::Center);
    label
}

impl From<&NiriOutput> for DisplayItem {
    fn from(o: &NiriOutput) -> Self {
        Self {
            name: o.name.clone(),
            x: o.logical.x,
            y: o.logical.y,
            width: o.logical.width,
            height: o.logical.height,
        }
    }
}

impl From<&NiriWindow> for WindowItem {
    fn from(w: &NiriWindow) -> Self {
        Self {
            id: w.id,
            title: w.title.clone(),
            app_id: w.app_id.clone(),
            width: w.size.width,
            height: w.size.height,
            location: String::new(),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn escape_roundtrip_plain() {
        assert_eq!(unescape_field(&escape_field("hello")), "hello");
    }

    #[test]
    fn escape_roundtrip_special_chars() {
        let original = "tab\there\nnewline\\backslash";
        assert_eq!(unescape_field(&escape_field(original)), original);
    }

    #[test]
    fn escape_produces_tab_safe_output() {
        let escaped = escape_field("a\tb");
        assert!(!escaped.contains('\t'));
    }

    #[test]
    fn unescape_handles_trailing_backslash() {
        assert_eq!(unescape_field("end\\"), "end\\");
    }

    #[test]
    fn initial_page_prefers_windows_when_no_displays() {
        assert_eq!(initial_page(false, true), "windows");
    }

    #[test]
    fn initial_page_stays_on_displays_when_present() {
        assert_eq!(initial_page(true, true), "displays");
        assert_eq!(initial_page(true, false), "displays");
    }

    #[test]
    fn initial_page_falls_back_to_displays_when_empty() {
        assert_eq!(initial_page(false, false), "displays");
    }

    fn display(name: &str, x: i32, y: i32, width: i32, height: i32) -> DisplayItem {
        DisplayItem {
            name: name.into(),
            x,
            y,
            width,
            height,
        }
    }

    fn item(title: &str, app_id: &str, location: &str) -> WindowItem {
        WindowItem {
            id: 1,
            title: title.into(),
            app_id: app_id.into(),
            width: 1280,
            height: 720,
            location: location.into(),
        }
    }

    fn niri_window(id: u64, workspace_id: Option<u64>) -> NiriWindow {
        NiriWindow {
            id,
            title: String::new(),
            app_id: String::new(),
            size: crate::niri_ipc::Size {
                width: 10,
                height: 10,
            },
            workspace_id,
        }
    }

    fn ws(id: u64, idx: u32, name: Option<&str>, output: Option<&str>) -> NiriWorkspace {
        NiriWorkspace {
            id,
            idx,
            name: name.map(Into::into),
            output: output.map(Into::into),
        }
    }

    #[test]
    fn workspace_label_uses_index_and_output() {
        assert_eq!(
            workspace_label(&ws(9, 2, None, Some("DP-1"))),
            "workspace 2 · DP-1"
        );
    }

    #[test]
    fn workspace_label_prefers_name() {
        assert_eq!(
            workspace_label(&ws(9, 2, Some("web"), None)),
            "workspace web"
        );
        assert_eq!(workspace_label(&ws(9, 2, Some(""), None)), "workspace 2");
    }

    #[test]
    fn window_items_resolve_workspace_by_id() {
        let windows = [
            niri_window(1, Some(5)),
            niri_window(2, Some(99)),
            niri_window(3, None),
        ];
        let workspaces = [ws(5, 1, None, Some("HDMI-A-1"))];
        let items = window_items(&windows, &workspaces);
        assert_eq!(items[0].location, "workspace 1 · HDMI-A-1");
        assert_eq!(items[1].location, "");
        assert_eq!(items[2].location, "");
    }

    #[test]
    fn subtitle_includes_app_and_location() {
        let w = item("Mozilla Firefox", "firefox", "workspace 2 · DP-1");
        assert_eq!(
            window_subtitle(&w, &window_title(&w)),
            "firefox · workspace 2 · DP-1"
        );
    }

    #[test]
    fn subtitle_skips_app_id_equal_to_title_and_empty_location() {
        let w = item("", "foot", "");
        assert_eq!(window_subtitle(&w, &window_title(&w)), "");
    }

    #[test]
    fn project_monitor_layout_preserves_relative_order_and_bounds() {
        let displays = vec![
            display("HDMI-A-1", 0, 0, 1920, 1080),
            display("DP-2", 1920, 0, 2752, 1152),
            display("DP-1", 4672, 0, 900, 1440),
        ];
        let rects = project_monitor_layout(&displays, 720.0, 184.0, 14.0);
        assert_eq!(rects.len(), 3);
        assert!(rects[0].x < rects[1].x && rects[1].x < rects[2].x);
        assert!(rects[2].height > rects[2].width);
        assert!(rects[1].width > rects[1].height);
        for r in &rects {
            assert!(r.x >= 13.9 && r.x + r.width <= 720.0 - 13.9);
            assert!(r.y >= 13.9 && r.y + r.height <= 184.0 - 13.9);
        }
    }

    #[test]
    fn fit_aspect_box_respects_orientation_and_bounds() {
        assert_eq!(fit_aspect_box(228, 128, 1920, 1080), (228, 128));
        assert_eq!(fit_aspect_box(228, 128, 900, 1440), (80, 128));
        let (uw, uh) = fit_aspect_box(228, 128, 3440, 1440);
        assert_eq!(uw, 228);
        assert!(uh < 128 && uh > 80);
    }

    #[test]
    fn icon_candidates_cover_reverse_dns_app_ids() {
        assert_eq!(
            icon_name_candidates("org.Mozilla.Firefox"),
            vec![
                "org.Mozilla.Firefox",
                "org.mozilla.firefox",
                "Firefox",
                "firefox"
            ]
        );
        assert_eq!(icon_name_candidates("foot"), vec!["foot"]);
        assert!(icon_name_candidates("").is_empty());
    }

    #[test]
    fn target_protocol_roundtrips_position_and_location() {
        let displays = vec![display("DP-2", 1920, 0, 2752, 1152)];
        let windows = vec![item("a\tb", "app", "workspace 2 · DP-1")];
        let mut buf = Vec::new();
        write_picker_targets(&mut buf, &displays, &windows).unwrap();
        let (parsed_displays, parsed_windows) = parse_targets_stream(buf.as_slice()).unwrap();
        assert_eq!(parsed_displays.len(), 1);
        assert_eq!(parsed_displays[0].name, "DP-2");
        assert_eq!((parsed_displays[0].x, parsed_displays[0].y), (1920, 0));
        assert_eq!(parsed_windows.len(), 1);
        assert_eq!(parsed_windows[0].title, "a\tb");
        assert_eq!(parsed_windows[0].location, "workspace 2 · DP-1");
    }

    #[test]
    fn unescape_handles_unknown_escape() {
        assert_eq!(unescape_field("a\\xb"), "a\\xb");
    }
}
