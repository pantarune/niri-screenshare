use std::cell::RefCell;
use std::io::{BufRead, BufReader, Write};
use std::path::PathBuf;
use std::process::{Command, Stdio};
use std::rc::Rc;
use std::sync::Arc;
use std::time::Duration;

use gtk4::gdk::Display;
use gtk4::gio;
use gtk4::glib;
use gtk4::prelude::*;
use gtk4::{
    Align, Application, ApplicationWindow, Box as GtkBox, CssProvider, Label, ListBox, Orientation,
    PolicyType, ScrolledWindow, SelectionMode, STYLE_PROVIDER_PRIORITY_APPLICATION,
};
use libadwaita::prelude::*;
use libadwaita::{ActionRow, HeaderBar, ViewStack, ViewSwitcher, ViewSwitcherPolicy};

use crate::niri_ipc::{NiriOutput, NiriWindow};

const PICKER_APP_ID: &str = "io.github.niri.screenshare.picker";

#[derive(Debug, Clone)]
pub enum PickerChoice {
    Monitor(String),
    Window(u64),
}

#[derive(Clone)]
struct DisplayItem {
    name: String,
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
}

/// Holds the picker child so Session.Close can kill it on cancel/retry.
pub type PickerChildSlot = Arc<std::sync::Mutex<Option<std::process::Child>>>;

/// Spawn `--picker` in a child process (GTK needs its own Application).
pub fn show_picker(outputs: &[NiriOutput], windows: &[NiriWindow]) -> Option<PickerChoice> {
    show_picker_cancellable(outputs, windows, None)
}

pub fn show_picker_cancellable(
    outputs: &[NiriOutput],
    windows: &[NiriWindow],
    child_slot: Option<PickerChildSlot>,
) -> Option<PickerChoice> {
    let displays: Vec<DisplayItem> = outputs.iter().map(DisplayItem::from).collect();
    let wins: Vec<WindowItem> = windows.iter().map(WindowItem::from).collect();

    if displays.is_empty() && wins.is_empty() {
        return None;
    }
    if displays.len() + wins.len() == 1 {
        if let Some(d) = displays.first() {
            return Some(PickerChoice::Monitor(d.name.clone()));
        }
        if let Some(w) = wins.first() {
            return Some(PickerChoice::Window(w.id));
        }
    }

    let bin = picker_bin();
    let mut child = match Command::new(&bin)
        .arg("--picker")
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .stderr(Stdio::inherit())
        .spawn()
    {
        Ok(c) => c,
        Err(e) => {
            tracing::error!("failed to spawn picker {}: {e}", bin.display());
            return None;
        }
    };

    if let Some(mut stdin) = child.stdin.take() {
        if let Err(e) = write_picker_targets(&mut stdin, &displays, &wins) {
            tracing::error!("failed to write picker targets: {e}");
            let _ = child.kill();
            return None;
        }
    }

    let stdout = child.stdout.take();

    let status = if let Some(slot) = &child_slot {
        if let Ok(mut guard) = slot.lock() {
            if let Some(mut old) = guard.take() {
                let _ = old.kill();
                let _ = old.wait();
            }
            *guard = Some(child);
        }

        loop {
            let finished = {
                let mut guard = slot.lock().ok();
                match guard.as_mut().and_then(|g| g.as_mut()) {
                    Some(c) => match c.try_wait() {
                        Ok(Some(status)) => Some(Ok(status)),
                        Ok(None) => None,
                        Err(e) => Some(Err(e)),
                    },
                    None => Some(Err(std::io::Error::other("picker killed"))),
                }
            };
            match finished {
                Some(result) => break result,
                None => std::thread::sleep(Duration::from_millis(50)),
            }
        }
    } else {
        child.wait()
    };

    if let Some(slot) = &child_slot {
        if let Ok(mut guard) = slot.lock() {
            if let Some(mut c) = guard.take() {
                let _ = c.wait();
            }
        }
    }

    let status = match status {
        Ok(s) => s,
        Err(e) => {
            tracing::info!("picker wait ended: {e}");
            return None;
        }
    };

    if !status.success() {
        tracing::info!("picker exited with {status}");
        return None;
    }

    let Some(stdout) = stdout else {
        return None;
    };
    for line in BufReader::new(stdout).lines().map_while(Result::ok) {
        let line = line.trim();
        if let Some(name) = line.strip_prefix("M:") {
            return Some(PickerChoice::Monitor(name.to_string()));
        }
        if let Some(id) = line.strip_prefix("W:") {
            if let Ok(id) = id.parse::<u64>() {
                return Some(PickerChoice::Window(id));
            }
        }
    }
    None
}

pub fn kill_slotted_picker(slot: &PickerChildSlot) {
    if let Ok(mut guard) = slot.lock() {
        if let Some(mut child) = guard.take() {
            tracing::info!("killing in-flight picker pid={}", child.id());
            let _ = child.kill();
            let _ = child.wait();
        }
    }
}

/// CLI entry for `--picker` / `--debug-picker`. Targets from stdin, else niri IPC.
pub fn run_picker_process() -> Option<PickerChoice> {
    let (displays, windows) = read_targets_or_query_niri();
    run_gtk_application(displays, windows)
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
    // D:name\tw\th  W:id\ttitle\tapp\tw\th  END
    for d in displays {
        writeln!(out, "D:{}\t{}\t{}", escape_field(&d.name), d.width, d.height)?;
    }
    for w in windows {
        writeln!(
            out,
            "W:{}\t{}\t{}\t{}\t{}",
            w.id,
            escape_field(&w.title),
            escape_field(&w.app_id),
            w.width,
            w.height
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
    let windows = crate::niri_ipc::list_windows()
        .unwrap_or_default()
        .iter()
        .map(WindowItem::from)
        .collect();
    (displays, windows)
}

fn read_targets_from_stdin() -> Option<(Vec<DisplayItem>, Vec<WindowItem>)> {
    use std::io::IsTerminal;
    if std::io::stdin().is_terminal() {
        return None;
    }
    let mut displays = Vec::new();
    let mut windows = Vec::new();
    let stdin = std::io::stdin();
    for line in stdin.lock().lines().map_while(Result::ok) {
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

fn run_gtk_application(
    displays: Vec<DisplayItem>,
    windows: Vec<WindowItem>,
) -> Option<PickerChoice> {
    let result: Rc<RefCell<Option<PickerChoice>>> = Rc::new(RefCell::new(None));

    let app = Application::builder()
        .application_id(PICKER_APP_ID)
        .flags(gio::ApplicationFlags::NON_UNIQUE)
        .build();

    let result_activate = result.clone();
    app.connect_activate(move |app| {
        build_and_present(app, displays.clone(), windows.clone(), result_activate.clone());
    });

    let exit = app.run_with_args::<&str>(&[]);
    if exit != glib::ExitCode::SUCCESS {
        tracing::warn!("picker gtk application exited with {exit:?}");
    }

    let choice = result.borrow().clone();
    choice
}

fn build_and_present(
    app: &Application,
    displays: Vec<DisplayItem>,
    windows: Vec<WindowItem>,
    result: Rc<RefCell<Option<PickerChoice>>>,
) {
    let _ = libadwaita::init();

    let selection: Rc<RefCell<Option<PickerChoice>>> = Rc::new(RefCell::new(None));

    let window = ApplicationWindow::builder()
        .application(app)
        .title("Screen Sharing")
        .default_width(520)
        .default_height(480)
        .build();

    let header = HeaderBar::new();

    let stack = ViewStack::new();
    stack.set_vexpand(true);

    let switcher = ViewSwitcher::builder()
        .stack(&stack)
        .policy(ViewSwitcherPolicy::Wide)
        .build();
    header.set_title_widget(Some(&switcher));

    let display_list = build_display_list(&displays, &selection);
    let display_scroll = ScrolledWindow::builder()
        .child(&display_list)
        .hscrollbar_policy(PolicyType::Never)
        .vexpand(true)
        .build();
    let displays_page = stack.add_titled(&display_scroll, Some("displays"), "Displays");
    displays_page.set_icon_name(Some("video-display-symbolic"));

    let window_list = build_window_list(&windows, &selection);
    let window_scroll = ScrolledWindow::builder()
        .child(&window_list)
        .hscrollbar_policy(PolicyType::Never)
        .vexpand(true)
        .build();
    let windows_page = stack.add_titled(&window_scroll, Some("windows"), "Windows");
    windows_page.set_icon_name(Some("window-symbolic"));

    let cancel = gtk4::Button::with_label("Cancel");
    cancel.add_css_class("pill");
    let share = gtk4::Button::with_label("Share");
    share.add_css_class("pill");
    share.add_css_class("suggested-action");
    share.set_sensitive(false);

    let share_btn = share.clone();
    let selection_watch = selection.clone();
    glib::timeout_add_local(Duration::from_millis(100), move || {
        share_btn.set_sensitive(selection_watch.borrow().is_some());
        glib::ControlFlow::Continue
    });

    let button_row = GtkBox::new(Orientation::Horizontal, 12);
    button_row.set_halign(Align::End);
    button_row.set_margin_top(6);
    button_row.set_margin_bottom(12);
    button_row.set_margin_start(12);
    button_row.set_margin_end(12);
    button_row.append(&cancel);
    button_row.append(&share);

    let content = GtkBox::new(Orientation::Vertical, 0);
    content.append(&header);
    content.append(&stack);
    content.append(&button_row);
    window.set_child(Some(&content));

    let finish = Rc::new({
        let result = result.clone();
        let selection = selection.clone();
        let window = window.clone();
        move |accepted: bool| {
            if accepted {
                *result.borrow_mut() = selection.borrow().clone();
            }
            window.close();
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

    display_list.connect_row_activated({
        let finish = finish.clone();
        let selection = selection.clone();
        move |_, _| {
            if selection.borrow().is_some() {
                finish(true);
            }
        }
    });
    window_list.connect_row_activated({
        let finish = finish.clone();
        let selection = selection.clone();
        move |_, _| {
            if selection.borrow().is_some() {
                finish(true);
            }
        }
    });

    if let Some(display) = Display::default() {
        let provider = CssProvider::new();
        provider.load_from_data("headerbar { min-height: 48px; }");
        gtk4::style_context_add_provider_for_display(
            &display,
            &provider,
            STYLE_PROVIDER_PRIORITY_APPLICATION,
        );
    }

    window.present();
    glib::timeout_add_local_once(Duration::from_millis(50), focus_picker_on_niri);
}

fn build_display_list(
    displays: &[DisplayItem],
    selection: &Rc<RefCell<Option<PickerChoice>>>,
) -> ListBox {
    let list = ListBox::new();
    list.set_selection_mode(SelectionMode::Single);
    list.add_css_class("boxed-list");
    list.set_margin_top(12);
    list.set_margin_bottom(12);
    list.set_margin_start(12);
    list.set_margin_end(12);

    if displays.is_empty() {
        list.append(&empty_label("No displays found"));
        return list;
    }

    for d in displays {
        let row = ActionRow::builder()
            .title(&d.name)
            .subtitle(format!("{}×{}", d.width, d.height))
            .activatable(true)
            .selectable(true)
            .build();
        list.append(&row);
    }

    let items = displays.to_vec();
    let selection = selection.clone();
    list.connect_row_selected(move |_, row| {
        let Some(row) = row else { return };
        if let Some(d) = items.get(row.index() as usize) {
            *selection.borrow_mut() = Some(PickerChoice::Monitor(d.name.clone()));
        }
    });

    list
}

fn build_window_list(
    windows: &[WindowItem],
    selection: &Rc<RefCell<Option<PickerChoice>>>,
) -> ListBox {
    let list = ListBox::new();
    list.set_selection_mode(SelectionMode::Single);
    list.add_css_class("boxed-list");
    list.set_margin_top(12);
    list.set_margin_bottom(12);
    list.set_margin_start(12);
    list.set_margin_end(12);

    if windows.is_empty() {
        list.append(&empty_label("No windows found"));
        return list;
    }

    for w in windows {
        let title = if w.title.is_empty() {
            w.app_id.clone()
        } else {
            w.title.clone()
        };
        let subtitle = if w.app_id.is_empty() || w.app_id == title {
            format!("{}×{}", w.width, w.height)
        } else {
            format!("{} · {}×{}", w.app_id, w.width, w.height)
        };
        let row = ActionRow::builder()
            .title(&title)
            .subtitle(subtitle)
            .activatable(true)
            .selectable(true)
            .build();
        list.append(&row);
    }

    let items = windows.to_vec();
    let selection = selection.clone();
    list.connect_row_selected(move |_, row| {
        let Some(row) = row else { return };
        if let Some(w) = items.get(row.index() as usize) {
            *selection.borrow_mut() = Some(PickerChoice::Window(w.id));
        }
    });

    list
}

fn empty_label(text: &str) -> Label {
    let label = Label::new(Some(text));
    label.add_css_class("dim-label");
    label.set_margin_top(24);
    label.set_margin_bottom(24);
    label.set_halign(Align::Center);
    label
}

fn focus_picker_on_niri() {
    focus_picker_on_niri_attempt(0);
}

fn focus_picker_on_niri_attempt(attempt: u32) {
    if try_float_picker_window() || attempt >= 20 {
        return;
    }
    glib::timeout_add_local_once(Duration::from_millis(50), move || {
        focus_picker_on_niri_attempt(attempt + 1);
    });
}

fn try_float_picker_window() -> bool {
    let Ok(windows) = crate::niri_ipc::list_windows() else {
        return false;
    };
    let Some(w) = windows.iter().rev().find(|w| {
        w.app_id == PICKER_APP_ID
            || w.app_id.contains("screenshare.picker")
            || w.app_id.contains("niri-screenshare-picker")
            || w.title == "Screen Sharing"
    }) else {
        return false;
    };
    let id = w.id.to_string();
    let niri = crate::niri_ipc::niri_bin();
    let _ = Command::new(&niri)
        .args(["msg", "action", "focus-window", "--id", &id])
        .output();
    let _ = Command::new(&niri)
        .args(["msg", "action", "move-window-to-floating", "--id", &id])
        .output();
    let _ = Command::new(&niri)
        .args(["msg", "action", "center-window", "--id", &id])
        .output();
    true
}

impl From<&NiriOutput> for DisplayItem {
    fn from(o: &NiriOutput) -> Self {
        Self {
            name: o.name.clone(),
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
        }
    }
}
