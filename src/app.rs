//! The window: empty screen, grid and single-image viewer.

use std::collections::{HashMap, HashSet};
use std::path::{Path, PathBuf};
use std::sync::{Arc, Mutex};

use eframe::egui::{
    self, pos2, scroll_area::ScrollBarVisibility, text::LayoutJob, vec2, UiBuilder, Align, Align2, Button, Color32, CornerRadius, CursorIcon, Frame, Id,
    Image, Key as KeyCode, LayerId, Layout, Margin, Modifiers, Order, Pos2, Rect, Response, RichText,
    ScrollArea, Sense, Shape, Slider, Stroke, StrokeKind, TextFormat, TextureHandle, TextureOptions, Ui, Vec2,
    ViewportCommand,
};

use std::sync::atomic::{AtomicBool, AtomicUsize, Ordering as AtomicOrdering};

use crate::devices::{self, CardFolder};
use crate::export::{self, Done, Exporter};
use crate::keybinds::{self, Action as KeyAction};
use crate::library::{self, Shot, SortBy};
use crate::loader::{Key, Kind, Loader, Request, THUMB_PX};
use crate::picks;
use crate::platform;
use crate::settings::{DragFiles, Settings};
use crate::theme::{self, Palette, ThemeId};
use crate::update;

/// Grid thumbnails kept in memory; the least recently seen are dropped beyond this.
const MAX_THUMBS: usize = 700;
const NAME_AREA: f32 = 40.0;
const GAP_X: f32 = 14.0;
const ROW_GAP: f32 = 16.0;
/// Rows above/below the visible ones whose thumbnails are loaded ahead of time.
const LOOKAHEAD_ROWS: usize = 3;
/// Most zoomed in: 8 image pixels per screen pixel.
const MAX_ZOOM: f32 = 8.0;
/// How long the export confirmation stays fully visible, then fades.
const TOAST_HOLD: f64 = 1.0;
const TOAST_FADE: f64 = 0.4;

#[derive(Clone, Copy, PartialEq)]
enum Screen {
    Grid,
    Viewer,
}

#[derive(Clone, Copy, PartialEq)]
enum SettingsPage {
    Keybinds,
    DragDrop,
    ExportOnPick,
    Memory,
    Themes,
}

/// Something on the Export on Pick page that needs the app (a folder dialog).
enum ExportAction {
    None,
    TurnOn,
    TurnOff,
    ChooseFolder,
}

/// One photo's space in the viewer's strip.
const STRIP_CELL: Vec2 = vec2(88.0, 62.0);

/// The mouse controls listed on the Keybinds page: (what you do, what it does).
const MOUSE_CONTROLS: &[(&str, &str)] = &[
    ("Click", "Select a photo"),
    ("Ctrl + Click", "Add or remove from the selection"),
    ("Shift + Click", "Select everything from the last clicked photo"),
    ("Double-click", "Open the photo"),
    ("Drag a photo", "Drag it (or the selection) into another app or folder"),
    ("Scroll wheel", "In the viewer: zoom around the cursor"),
    ("Double-click", "In the viewer: zoom to that spot, or back to fit"),
    ("Drag", "In the viewer: move around while zoomed"),
    ("Mouse back button", "Back to the grid"),
];

#[derive(Clone, Copy, PartialEq, Eq, Debug)]
enum Media {
    Photos,
    Videos,
}

/// What the user did on the Keybinds page.
enum KeybindChange {
    None,
    /// Clicked an action's shortcut: start or stop waiting for its new key.
    Toggle(keybinds::Action),
    Restore,
}

/// A search through a whole drive, running in the background.
struct DriveScan {
    dir: PathBuf,
    result: std::sync::mpsc::Receiver<std::io::Result<Vec<Shot>>>,
    checked: Arc<AtomicUsize>,
    stop: Arc<AtomicBool>,
    /// A photo to open in the viewer afterwards.
    show: Option<PathBuf>,
    /// Updating the drive already open (after a delete), rather than opening it.
    refresh: bool,
}

#[derive(Clone, Copy, PartialEq, Debug)]
enum RangeMode {
    Off,
    PickStart,
    PickEnd(usize),
}

#[derive(Clone, Copy)]
struct Item {
    shot: usize,
    raw: bool,
}

/// A delete in progress and what to do when it ends.
struct Deleting {
    job: export::Deletion,
    shots: Vec<usize>,
    permanent: bool,
    started: f64,
}

/// How long a delete runs before its progress bar appears, in seconds.
const DELETE_BAR_DELAY: f64 = 0.2;

/// A drag out of the window: its files and the overlay's text. `shown` once the overlay
/// has been drawn, so the drag can start.
struct PendingDrag {
    paths: Vec<PathBuf>,
    /// Up to three photos' files for the stack under the cursor, front first.
    stack: Vec<PathBuf>,
    photos: usize,
    title: String,
    count: String,
    note: String,
    shown: bool,
    /// Set while macOS runs the drag (Windows is done before the app goes on); becomes
    /// true on the drop or cancel.
    ended: Option<Arc<AtomicBool>>,
}

enum Action {
    /// Select (or, as the second of a double-click, open).
    Click(usize),
    /// Started dragging this photo (and the rest of the selection, if it's in it).
    DragOut(usize),
    TogglePick(usize),
    Mark(usize),
}

struct Thumb {
    tex: TextureHandle,
    kind: Kind,
    last_used: u64,
}

/// A clip playing (or paused) in the viewer.
struct Playback {
    shot: usize,
    player: crate::player::Player,
    /// The newest frame.
    texture: Option<TextureHandle>,
    /// The volume has been set again since the clip started playing.
    audio_settled: bool,
}

struct FullImage {
    tex: TextureHandle,
    /// The photo's real size in pixels.
    original: Vec2,
    /// An animated GIF's frames and how long each shows (seconds); empty when still.
    frames: Vec<(TextureHandle, f64)>,
}

impl FullImage {
    /// The texture to show at `time` (seconds): the frame due then for an animation,
    /// and how long until the next frame is due.
    fn frame_at(&self, time: f64) -> (&TextureHandle, Option<f64>) {
        let total: f64 = self.frames.iter().map(|(_, d)| d).sum();
        if self.frames.is_empty() || total <= 0.0 {
            return (&self.tex, None);
        }
        let mut t = time % total;
        for (tex, delay) in &self.frames {
            if t < *delay {
                return (tex, Some(delay - t));
            }
            t -= delay;
        }
        (&self.frames[0].0, Some(0.0))
    }
}

struct Toast {
    text: String,
    started: f64,
}

struct Batch {
    remaining: usize,
    shots: usize,
    failed: bool,
    dest: PathBuf,
}

/// The tile nearest the middle of the grid, and where it sat on screen. Used to keep
/// that photo in place when the grid's layout changes.
#[derive(Clone, Copy)]
struct ViewAnchor {
    shot: usize,
    raw: bool,
    screen_y: f32,
}

/// Anything that changes which tiles the grid shows, or where.
#[derive(Clone, Copy, PartialEq, Debug)]
struct LayoutKey {
    show_raw: bool,
    range_mode: bool,
    view_picked: bool,
    view_rejected: bool,
    range: Option<(usize, usize)>,
    tile_width: i32,
}

pub struct App {
    p: Palette,
    settings: Settings,
    /// The theme on screen: the saved one, unless `--theme` chose another for this run.
    theme_shown: ThemeId,
    /// The Windows 11 Mica backdrop is showing (the top bar is see-through to it).
    mica_active: bool,
    loader: Loader,
    generation: u64,
    exporter: Exporter,
    cards: Arc<Mutex<Vec<CardFolder>>>,

    folder: Option<PathBuf>,
    shots: Vec<Shot>,
    /// Photos or videos: the grid shows one kind at a time.
    media: Media,
    /// The open folder's shots of the other kind, set aside until the switch flips.
    other_shots: Vec<Shot>,
    /// The clip playing in the viewer.
    playback: Option<Playback>,
    /// Clips' volume (0 to 1) and mute, kept from clip to clip until Cullery closes.
    volume: f32,
    muted: bool,
    /// The viewer fills the screen, with the bars hidden.
    fullscreen: bool,
    /// Camera details read for the details bar, by file.
    info_cache: HashMap<PathBuf, crate::info::PhotoInfo>,
    /// Photos read ahead of time, kept in memory: screen-sized, and small.
    cache_keep: HashSet<usize>,
    thumb_keep: HashSet<usize>,
    /// Where reading ahead starts from on the grid: its first photo in view.
    grid_anchor: usize,
    /// The viewer's size in pixels, which screen-sized photos are read at.
    viewer_px: [u32; 2],
    picked: HashSet<usize>,
    /// Marked to delete later (View Rejected shows them together).
    rejected: HashSet<usize>,
    selected: HashSet<usize>,
    /// Where a Shift+click range starts (the last plain or Ctrl click).
    select_anchor: Option<usize>,
    range: Option<(usize, usize)>,
    range_mode: RangeMode,

    screen: Screen,
    current: usize,
    scroll_to_current: bool,
    view_anchor: Option<ViewAnchor>,
    last_layout: Option<LayoutKey>,
    /// Scroll position set by the scroll bar, applied next frame.
    pending_scroll: Option<f32>,
    /// Where each grid tile was drawn last frame (used by tests to aim clicks).
    drawn_tiles: Vec<(Item, Rect)>,
    /// Last plain click on a tile (shot, time), for spotting double-clicks.
    last_tile_click: Option<(usize, f64)>,
    /// Last click on the viewer's photo (time, position), for double-click zoom.
    last_viewer_click: Option<(f64, Pos2)>,
    /// Shots waiting for the user to confirm deleting them.
    confirm_delete: Option<Vec<usize>>,
    /// A delete running in the background.
    deleting: Option<Deleting>,
    /// Longest gap between the clicks of a double-click, from Windows' setting.
    double_click_time: f64,
    /// The settings window, and which page it shows.
    settings_page: Option<SettingsPage>,
    /// Grid shows only picked photos.
    view_picked: bool,
    /// Grid shows only rejected photos.
    view_rejected: bool,
    /// The app window (its HWND, or NSView on macOS), for starting drags out of it
    /// (None in tests).
    hwnd: Option<isize>,
    /// A macOS drag just ended. It kept the mouse-up, so the window still thinks the
    /// button is held until it's told otherwise.
    release_pointer: bool,
    /// A newer version, once the check at startup has found one.
    new_version: Arc<Mutex<Option<String>>>,
    /// The popup offering the update is open.
    update_prompt: bool,
    /// The Keybinds page is waiting for this action's new key.
    rebinding: Option<KeyAction>,
    /// A whole drive being searched for photos.
    drive_scan: Option<DriveScan>,
    /// Remembered picks that aren't photos in the open folder right now, like another
    /// memory card's. Kept so saving this folder's picks doesn't lose them.
    other_picks: Vec<picks::Pick>,
    other_rejects: Vec<picks::Pick>,
    /// Some photo in this folder has a RAW partner (else Show Paired RAW is hidden).
    has_raw_pairs: bool,
    /// Files in the last drag out of Cullery, so dropping them back on it is ignored.
    last_drag: HashSet<PathBuf>,
    /// A drag out of the window about to start, shown by its overlay first.
    pending_drag: Option<PendingDrag>,

    thumbs: HashMap<usize, Thumb>,
    fulls: HashMap<usize, FullImage>,
    original: Option<(usize, TextureHandle)>,
    /// Loads that failed, and why.
    failed: HashMap<Key, String>,
    frame: u64,

    /// Viewer zoom in screen points per image pixel; `None` = fit to window.
    zoom: Option<f32>,
    /// Image pixel shown at the middle of the viewer while zoomed.
    zoom_center: Vec2,
    /// For the zoom indicator: current zoom in percent of actual pixels.
    zoom_percent: Option<f32>,
    /// Size of the shown photo, for the zoom indicator's 100% button.
    shown_original: Option<Vec2>,

    batches: HashMap<u64, Batch>,
    next_batch: u64,
    toast: Option<Toast>,
    content_rect: Rect,
    /// Text shown in the bottom bar; `true` marks an error.
    message: Option<(String, bool)>,
    /// The window was asked to close while photos were still copying.
    confirm_close: bool,
    /// Close as soon as the copying ends.
    close_when_done: bool,
    /// The user chose to close; let the close through.
    closing: bool,
}

impl App {
    pub fn new(cc: &eframe::CreationContext<'_>, start: crate::args::StartArgs) -> Self {
        let mut app = Self::with_context(&cc.egui_ctx);
        app.new_version = update::check(cc.egui_ctx.clone());
        use raw_window_handle::{HasWindowHandle, RawWindowHandle};
        app.hwnd = cc.window_handle().ok().and_then(|h| match h.as_raw() {
            RawWindowHandle::Win32(w) => Some(w.hwnd.get()),
            RawWindowHandle::AppKit(w) => Some(w.ns_view.as_ptr() as isize),
            _ => None,
        });
        if let Some(hwnd) = app.hwnd {
            crate::window::set_icons(hwnd);
        }
        let theme = start.theme.unwrap_or(app.settings.theme);
        app.show_theme(&cc.egui_ctx, theme);
        if let Some(path) = start.path {
            app.open_path(&cc.egui_ctx, path);
        }
        if let Some(problem) = start.problem {
            app.error(problem);
        }
        app
    }

    /// Opens what the command line named: a folder, or a photo in its folder's viewer.
    fn open_path(&mut self, ctx: &egui::Context, path: PathBuf) {
        // A relative path means relative to where Cullery was started from.
        let path = std::path::absolute(&path).unwrap_or(path);
        if path.is_dir() {
            self.open_folder(ctx, path);
        } else if !path.is_file() {
            self.error(format!("Couldn't find {}", path.display()));
        } else if let Some(folder) = path.parent().map(Path::to_path_buf) {
            if library::is_drive_root(&folder) {
                self.start_drive_scan(folder, Some(path), false);
                return;
            }
            self.open_folder(ctx, folder);
            if self.folder.is_some() {
                self.show_file(&path);
            }
        }
    }

    /// Opens the viewer at this file of the open folder.
    fn show_file(&mut self, path: &Path) {
        let name = path.file_name().unwrap_or_default().to_string_lossy().to_lowercase();
        let is_it = |s: &Shot| s.files().any(|f| f.file_name().is_some_and(|n| n.to_string_lossy().to_lowercase() == name));
        // A video named while photos are showing (or the other way round): switch first.
        if !self.shots.iter().any(is_it) && self.other_shots.iter().any(is_it) {
            self.switch_media(if self.media == Media::Photos { Media::Videos } else { Media::Photos });
        }
        let found = self.shots.iter().position(is_it);
        match found {
            Some(shot) => self.open_viewer(shot),
            None => self.error(format!("{} isn't a photo Cullery can open", path.display())),
        }
    }

    /// Switches theme for good, from Settings.
    fn set_theme(&mut self, ctx: &egui::Context, id: ThemeId) {
        if self.settings.theme != id {
            self.settings.theme = id;
            self.settings.save();
        }
        self.show_theme(ctx, id);
    }

    /// Shows a theme: colors and shapes, the title bar's light/dark mode, and the
    /// Windows 11 Mica backdrop.
    fn show_theme(&mut self, ctx: &egui::Context, id: ThemeId) {
        self.theme_shown = id;
        self.p = id.palette();
        theme::apply(ctx, &self.p);
        self.mica_active = match self.hwnd {
            Some(hwnd) => crate::window::set_backdrop(hwnd, self.p.mica, self.p.dark),
            None => false,
        };
        ctx.request_repaint();
    }

    fn with_context(ctx: &egui::Context) -> Self {
        let settings = Settings::load();
        let p = settings.theme.palette();
        theme::apply(ctx, &p);
        // egui allows only 0.3 s between the clicks of a double-click (Windows: 0.5 s by
        // default), measured end to end, so normal double-clicks often went unnoticed.
        let double_click_time = system_double_click_time();
        ctx.options_mut(|o| o.input_options.max_double_click_delay = double_click_time);
        let ctx = ctx.clone();
        Self {
            p,
            theme_shown: settings.theme,
            settings,
            mica_active: false,
            loader: Loader::new(ctx.clone()),
            generation: 0,
            exporter: Exporter::new(ctx.clone()),
            cards: devices::watch(ctx),
            folder: None,
            shots: Vec::new(),
            media: Media::Photos,
            other_shots: Vec::new(),
            playback: None,
            volume: 1.0,
            muted: false,
            fullscreen: false,
            info_cache: HashMap::new(),
            cache_keep: HashSet::new(),
            thumb_keep: HashSet::new(),
            grid_anchor: 0,
            viewer_px: [1920, 1080],
            picked: HashSet::new(),
            rejected: HashSet::new(),
            selected: HashSet::new(),
            select_anchor: None,
            range: None,
            range_mode: RangeMode::Off,
            screen: Screen::Grid,
            current: 0,
            scroll_to_current: false,
            view_anchor: None,
            last_layout: None,
            pending_scroll: None,
            drawn_tiles: Vec::new(),
            last_tile_click: None,
            last_viewer_click: None,
            confirm_delete: None,
            deleting: None,
            double_click_time,
            settings_page: None,
            view_picked: false,
            view_rejected: false,
            hwnd: None,
            release_pointer: false,
            new_version: Arc::default(),
            update_prompt: false,
            rebinding: None,
            drive_scan: None,
            other_picks: Vec::new(),
            other_rejects: Vec::new(),
            has_raw_pairs: false,
            last_drag: HashSet::new(),
            pending_drag: None,
            thumbs: HashMap::new(),
            fulls: HashMap::new(),
            original: None,
            failed: HashMap::new(),
            frame: 0,
            zoom: None,
            zoom_center: Vec2::ZERO,
            zoom_percent: None,
            shown_original: None,
            batches: HashMap::new(),
            next_batch: 0,
            toast: None,
            content_rect: Rect::NOTHING,
            message: None,
            confirm_close: false,
            close_when_done: false,
            closing: false,
        }
    }

    // ---------------------------------------------------------------- actions

    fn choose_folder(&mut self, ctx: &egui::Context) {
        let mut dialog = rfd::FileDialog::new().set_title("Open Folder");
        if let Some(start) = self.folder.as_ref().or(self.settings.recent.first()) {
            dialog = dialog.set_directory(start);
        }
        if let Some(dir) = dialog.pick_folder() {
            self.open_folder(ctx, dir);
        }
    }

    /// Opens a folder. A whole drive is searched in the background, subfolders included.
    fn open_folder(&mut self, ctx: &egui::Context, dir: PathBuf) {
        if library::is_drive_root(&dir) {
            self.start_drive_scan(dir, None, false);
        } else {
            let result = library::scan(&dir);
            self.finish_open(ctx, dir, result);
        }
    }

    /// Starts searching a whole drive; `poll_drive_scan` takes over when it's done.
    fn start_drive_scan(&mut self, dir: PathBuf, show: Option<PathBuf>, refresh: bool) {
        let (send, result) = std::sync::mpsc::channel();
        let checked = Arc::new(AtomicUsize::new(0));
        let stop = Arc::new(AtomicBool::new(false));
        let skip = self.settings.export_dir.clone();
        let (folder, seen, stopping) = (dir.clone(), checked.clone(), stop.clone());
        let started = std::thread::Builder::new().name("drive-scan".into()).spawn(move || {
            let _ = send.send(library::scan_with_subfolders(&folder, skip.as_deref(), &seen, &stopping));
        });
        match started {
            Ok(_) => self.drive_scan = Some(DriveScan { dir, result, checked, stop, show, refresh }),
            Err(e) => self.error(format!("Couldn't search {}: {e}", dir.display())),
        }
    }

    fn poll_drive_scan(&mut self, ctx: &egui::Context) {
        let Some(scan) = &self.drive_scan else { return };
        let result = match scan.result.try_recv() {
            Ok(result) => result,
            Err(std::sync::mpsc::TryRecvError::Empty) => return,
            Err(std::sync::mpsc::TryRecvError::Disconnected) => Err(std::io::Error::other("the search stopped unexpectedly")),
        };
        let Some(scan) = self.drive_scan.take() else { return };
        if scan.refresh {
            if let Ok(shots) = result {
                self.adopt_rescan(shots);
            }
            return;
        }
        self.finish_open(ctx, scan.dir, result);
        if let (Some(path), true) = (scan.show, self.folder.is_some()) {
            self.show_file(&path);
        }
    }

    /// The popup while a drive is searched, which can take a while on a big one.
    fn drive_scan_dialog(&mut self, ctx: &egui::Context) {
        let Some(scan) = &self.drive_scan else { return };
        let p = self.p;
        let checked = scan.checked.load(AtomicOrdering::Relaxed);
        let can_cancel = !scan.refresh;
        let name = scan.dir.display().to_string();
        let mut cancelled = false;
        let modal = egui::Modal::new(Id::new("drive-scan"))
            .frame(Frame::window(&ctx.style()).fill(p.card).corner_radius(p.r_card).inner_margin(Margin::same(20)))
            .show(ctx, |ui| {
                ui.set_width(400.0);
                ui.horizontal(|ui| {
                    ui.add(egui::Spinner::new().size(18.0).color(p.accent));
                    ui.label(RichText::new(format!("Looking for photos on {name}")).font(theme::font_semibold(17.0)));
                });
                ui.add_space(8.0);
                ui.label(RichText::new(format!("Checked {checked} {} so far, including subfolders.", plural(checked, "file", "files"))).color(p.label2));
                if can_cancel {
                    ui.add_space(16.0);
                    ui.with_layout(Layout::right_to_left(Align::Center), |ui| {
                        cancelled = ui.add(Button::new("Cancel").corner_radius(p.r_ctl).min_size(vec2(96.0, 32.0))).clicked();
                    });
                }
            });
        if can_cancel && (cancelled || modal.should_close()) {
            scan.stop.store(true, AtomicOrdering::Relaxed);
            self.drive_scan = None;
        }
        ctx.request_repaint_after(std::time::Duration::from_millis(100));
    }

    /// Shows the photos a search of `dir` found.
    fn finish_open(&mut self, ctx: &egui::Context, dir: PathBuf, result: std::io::Result<Vec<Shot>>) {
        match result {
            Ok(mut shots) => {
                library::sort(&mut shots, self.settings.sort_by, self.settings.sort_descending);
                // Photos first; a folder with only videos opens on its videos.
                let (photos, videos) = library::split_videos(shots);
                let (media, shots, other) =
                    if photos.is_empty() && !videos.is_empty() { (Media::Videos, videos, photos) } else { (Media::Photos, photos, videos) };
                self.media = media;
                self.other_shots = other;
                self.generation = self.loader.reset();
                self.thumbs.clear();
                self.fulls.clear();
                self.original = None;
                self.failed.clear();
                self.has_raw_pairs = has_raw_pairs(&shots);
                self.shots = shots;
                self.reset_view();
                // The memory card list is only on the start screen, which never comes
                // back once a folder is open; dropping this stops its background scans.
                self.cards = Arc::default();
                self.message = if self.shots.is_empty() {
                    Some(("No photos in this folder".into(), false))
                } else {
                    None
                };
                let title = format!("Cullery — {}", folder_name(&dir));
                ctx.send_viewport_cmd(ViewportCommand::Title(title));
                self.settings.add_recent(dir.clone());
                self.settings.save();
                self.folder = Some(dir);
                self.reload_picks();
            }
            Err(e) => self.error(format!("Couldn't open {}: {e}", dir.display())),
        }
    }

    /// Everything about what's on screen that refers to the previous folder's photos.
    fn reset_view(&mut self) {
        self.selected.clear();
        self.select_anchor = None;
        self.range = None;
        self.range_mode = RangeMode::Off;
        self.view_picked = false;
        self.view_rejected = false;
        self.confirm_delete = None;
        self.screen = Screen::Grid;
        self.current = 0;
        self.view_anchor = None;
        self.scroll_to_current = false;
        self.pending_scroll = Some(0.0);
        self.last_tile_click = None;
        self.reset_zoom();
    }

    fn choose_export(&mut self) -> bool {
        let mut dialog = rfd::FileDialog::new().set_title("Choose Export Folder");
        if let Some(start) = &self.settings.export_dir {
            dialog = dialog.set_directory(start);
        }
        let Some(dir) = dialog.pick_folder() else { return false };
        if Some(&dir) == self.folder.as_ref() {
            self.error("The export folder can't be the folder you're viewing.".into());
            return false;
        }
        self.settings.export_dir = Some(dir);
        self.settings.save();
        true
    }

    /// The export folder, asking for one first if none is set.
    fn export_dir_or_ask(&mut self) -> Option<PathBuf> {
        if self.settings.export_dir.is_none() && !self.choose_export() {
            return None;
        }
        self.settings.export_dir.clone()
    }

    /// Picks are remembered per folder, so they survive closing the app. A photo only
    /// counts as picked if it's the same size as the one picked: another memory card
    /// can have the same folder and file names.
    fn reload_picks(&mut self) {
        self.picked.clear();
        self.other_picks.clear();
        let Some(mut saved) = self.folder.as_deref().and_then(picks::load) else { return };
        for (i, shot) in self.shots.iter().enumerate() {
            let exact = saved.remove(&(shot.key.clone(), Some(shot.size)));
            let from_older_version = saved.remove(&(shot.key.clone(), None));
            if exact || from_older_version {
                self.picked.insert(i);
            }
        }
        self.other_picks = saved.into_iter().collect();
        self.reload_rejects();
    }

    /// Rejects are remembered the same way as picks.
    fn reload_rejects(&mut self) {
        self.rejected.clear();
        self.other_rejects.clear();
        let Some(mut saved) = self.folder.as_deref().and_then(|f| picks::load_list(picks::List::Rejects, f)) else { return };
        for (i, shot) in self.shots.iter().enumerate() {
            let exact = saved.remove(&(shot.key.clone(), Some(shot.size)));
            let from_older_version = saved.remove(&(shot.key.clone(), None));
            if exact || from_older_version {
                self.rejected.insert(i);
            }
        }
        self.other_rejects = saved.into_iter().collect();
    }

    fn save_picks(&self) {
        let Some(folder) = &self.folder else { return };
        let current = self.picked.iter().map(|&i| (self.shots[i].key.clone(), Some(self.shots[i].size)));
        picks::save(folder, current.chain(self.other_picks.iter().cloned()));
        let rejects = self.rejected.iter().map(|&i| (self.shots[i].key.clone(), Some(self.shots[i].size)));
        picks::save_list(picks::List::Rejects, folder, rejects.chain(self.other_rejects.iter().cloned()));
    }

    /// Rejects or un-rejects shots. A rejected photo isn't picked, and the other way round.
    fn set_rejected(&mut self, shots: &[usize], reject: bool) {
        for &shot in shots {
            if reject {
                self.rejected.insert(shot);
                self.picked.remove(&shot);
            } else {
                self.rejected.remove(&shot);
            }
        }
        if self.view_picked {
            let picked = &self.picked;
            self.selected.retain(|s| picked.contains(s));
        }
        self.save_picks();
    }

    /// Grid: rejects every selected shot, or un-rejects them if they're all rejected.
    /// Viewer: the photo on show.
    fn reject_selected(&mut self) {
        let mut shots: Vec<usize> = match self.screen {
            Screen::Viewer => vec![self.current],
            Screen::Grid => self.selected.iter().copied().collect(),
        };
        if shots.is_empty() {
            return;
        }
        shots.sort_unstable();
        let all_rejected = shots.iter().all(|s| self.rejected.contains(s));
        self.set_rejected(&shots, !all_rejected);
    }

    /// Puts the selected photos' files (grid) or the shown photo's (viewer) on the
    /// clipboard, to paste into a folder. The same files as dragging them out.
    fn copy_selected(&mut self, ctx: &egui::Context) {
        let mut shots: Vec<usize> = match self.screen {
            Screen::Viewer => vec![self.current],
            Screen::Grid => self.selected.iter().copied().collect(),
        };
        if shots.is_empty() {
            return;
        }
        shots.sort_unstable();
        let paths = self.drag_paths(&shots);
        match crate::dragout::copy_files(&paths) {
            Ok(()) => {
                let n = shots.len();
                self.show_toast(ctx, format!("Copied {n} {}  ·  paste them into a folder", plural(n, "photo", "photos")));
            }
            Err(e) => self.error(format!("Couldn't copy: {e}")),
        }
    }

    fn set_view_rejected(&mut self, on: bool) {
        self.view_rejected = on;
        if on {
            self.view_picked = false;
            self.range_mode = RangeMode::Off;
            let rejected = &self.rejected;
            self.selected.retain(|s| rejected.contains(s));
        }
    }

    /// The details bar along the top of the viewer, and the tab that hides and shows it
    /// (just under the bar, or just under the top bar when hidden). Returns the space
    /// left below.
    fn viewer_info(&mut self, ui: &mut Ui, whole: Rect) -> Rect {
        if self.fullscreen || self.shots.is_empty() {
            return whole;
        }
        const BAR_H: f32 = 30.0;
        const TAB_H: f32 = 14.0;
        let p = self.p;
        let shown = self.settings.info_bar;
        let mut top = whole.min.y;
        if shown {
            let bar = Rect::from_min_size(whole.min + vec2(0.0, 6.0), vec2(whole.width(), BAR_H));
            self.draw_info_bar(ui, bar);
            top = bar.max.y + 2.0;
        }
        let tab_rect = Rect::from_center_size(pos2(whole.center().x, top + TAB_H / 2.0), vec2(72.0, TAB_H));
        let tab = ui
            .interact(tab_rect, Id::new("info-tab"), Sense::click())
            .on_hover_text(if shown { "Hide the details bar" } else { "Show the details bar" })
            .on_hover_cursor(CursorIcon::PointingHand);
        let pill = Rect::from_center_size(tab_rect.center(), vec2(if tab.hovered() { 52.0 } else { 44.0 }, 5.0));
        ui.painter().rect_filled(pill, 3, if tab.hovered() { p.label } else { p.label2.gamma_multiply(0.7) });
        if tab.clicked() {
            self.settings.info_bar = !shown;
            self.settings.save();
        }
        Rect::from_min_max(pos2(whole.min.x, tab_rect.max.y + 8.0), whole.max)
    }

    /// One line about the photo on show: where it is (click to open the folder), its
    /// size, when it was taken, and the camera's settings. What doesn't fit is left off.
    fn draw_info_bar(&mut self, ui: &mut Ui, bar: Rect) {
        let p = self.p;
        let shot = &self.shots[self.current];
        let Some(path) = shot.display_path().cloned() else { return };
        let (video, raw, modified) = (shot.video, shot.raw.clone(), shot.modified);
        if self.info_cache.len() > 2000 {
            self.info_cache.clear();
        }
        let info = self
            .info_cache
            .entry(path.clone())
            .or_insert_with(|| {
                // A JPG or HEIC has the camera details; a RAW-only shot's RAW may too.
                let found = crate::info::read(&path);
                match raw.filter(|r| *r != path && found == crate::info::PhotoInfo::default()) {
                    Some(raw) => crate::info::read(&raw),
                    None => found,
                }
            })
            .clone();

        ui.painter().rect_filled(bar, p.r_card, p.card);
        let painter = ui.painter().with_clip_rect(bar.shrink(1.0));
        let right = bar.max.x - 12.0;
        let mut x = bar.min.x + 12.0;
        let y = bar.center().y;

        // Where it is: opens the folder with the file selected.
        let folder = path.parent().unwrap_or(&path);
        let location = painter.layout_no_wrap(short_path(folder), theme::font_semibold(12.5), p.tint);
        let loc_rect = Rect::from_min_size(pos2(x, y - location.size().y / 2.0), location.size());
        let open_in = if cfg!(target_os = "macos") { "Finder" } else { "File Explorer" };
        let link = ui
            .interact(loc_rect.intersect(bar), Id::new("info-location"), Sense::click())
            .on_hover_text(format!("{}\nClick to show it in {open_in}", path.display()))
            .on_hover_cursor(CursorIcon::PointingHand);
        if link.hovered() {
            painter.line_segment([loc_rect.left_bottom(), loc_rect.right_bottom()], Stroke::new(1.0_f32, p.tint));
        }
        if link.clicked() {
            crate::platform::reveal(&path);
        }
        x = loc_rect.max.x;
        painter.galley(loc_rect.min, location, p.tint);

        // The rest, most useful first, for as long as there's room.
        let pixels = if video {
            None
        } else {
            self.fulls.get(&self.current).map(|f| [f.original.x as u32, f.original.y as u32]).or(info.size)
        };
        let facts = [
            pixels.map(|[w, h]| format!("{w} × {h}")),
            info.taken.map(|t| format!("Taken {t}")),
            info.camera,
            info.shutter,
            info.aperture,
            info.iso,
            info.focal,
            info.lens,
            modified.and_then(crate::platform::local_time).map(|t| format!("Modified {t}")),
        ];
        for fact in facts.into_iter().flatten() {
            let g = painter.layout_no_wrap(format!("   ·   {fact}"), theme::font(12.5), p.label2);
            if x + g.size().x > right {
                break;
            }
            painter.galley(pos2(x, y - g.size().y / 2.0), g.clone(), p.label2);
            x += g.size().x;
        }
    }

    /// The strip of photos along the bottom of the viewer, and the tab that hides and
    /// shows it (at the top of the strip, or just above the bar when hidden). Returns
    /// the space left for the photo itself.
    fn viewer_strip(&mut self, ui: &mut Ui, whole: Rect) -> Rect {
        if self.fullscreen {
            return whole;
        }
        const TAB_H: f32 = 14.0;
        let p = self.p;
        let shown = self.settings.filmstrip;
        let strip = Rect::from_min_max(pos2(whole.min.x, whole.max.y - STRIP_CELL.y - 8.0), whole.max);
        let tab_bottom = if shown { strip.min.y - 2.0 } else { whole.max.y };
        let tab_rect = Rect::from_center_size(pos2(whole.center().x, tab_bottom - TAB_H / 2.0), vec2(72.0, TAB_H));
        let tab = ui
            .interact(tab_rect, Id::new("strip-tab"), Sense::click())
            .on_hover_text(if shown { "Hide the photo strip" } else { "Show the photo strip" })
            .on_hover_cursor(CursorIcon::PointingHand);
        let pill = Rect::from_center_size(tab_rect.center(), vec2(if tab.hovered() { 52.0 } else { 44.0 }, 5.0));
        ui.painter().rect_filled(pill, 3, if tab.hovered() { p.label } else { p.label2.gamma_multiply(0.7) });
        if tab.clicked() {
            self.settings.filmstrip = !shown;
            self.settings.save();
        }
        if shown {
            self.draw_strip(ui, strip);
        }
        Rect::from_min_max(whole.min, pos2(whole.max.x, tab_rect.min.y - 8.0))
    }

    /// The photos of the working set in a row, centered on the one in the viewer.
    /// Clicking one shows it.
    fn draw_strip(&mut self, ui: &mut Ui, strip: Rect) {
        const GAP: f32 = 6.0;
        let p = self.p;
        let set = self.working_set();
        if set.is_empty() {
            return;
        }
        let here = match set.binary_search(&self.current) {
            Ok(i) | Err(i) => i.min(set.len() - 1),
        };
        let pitch = STRIP_CELL.x + GAP;
        // Glides to the new photo rather than jumping.
        let offset = ui.ctx().animate_value_with_time(Id::new("strip-scroll"), here as f32 * pitch, 0.18);
        let x0 = strip.center().x - STRIP_CELL.x / 2.0 - offset;
        let first = ((strip.min.x - x0) / pitch).floor().max(0.0) as usize;
        let last = (((strip.max.x - x0) / pitch).ceil().max(0.0) as usize).min(set.len());

        let painter = ui.painter().with_clip_rect(strip);
        let mut visible = Vec::new();
        let mut clicked = None;
        for &shot in set.get(first..last).unwrap_or_default() {
            let i = set.binary_search(&shot).unwrap_or(0);
            let cell = Rect::from_min_size(pos2(x0 + i as f32 * pitch, strip.center().y - STRIP_CELL.y / 2.0), STRIP_CELL);
            visible.push(shot);
            painter.rect_filled(cell, 4, p.tile);
            if let Some(t) = self.thumbs.get_mut(&shot) {
                t.last_used = self.frame;
                let fit = fit_rect(cell, t.tex.size_vec2());
                painter.image(t.tex.id(), fit, Rect::from_min_max(pos2(0.0, 0.0), pos2(1.0, 1.0)), Color32::WHITE);
            }
            let current = shot == self.current;
            let r = ui
                .interact(cell.intersect(strip), Id::new(("strip", shot)), Sense::click())
                .on_hover_cursor(CursorIcon::PointingHand);
            if current {
                painter.rect_stroke(cell.expand(1.0), 5, Stroke::new(2.5_f32, p.accent), StrokeKind::Outside);
            } else if !r.hovered() {
                // The others a little dimmer, so the one on show stands out.
                painter.rect_filled(cell, 4, Color32::from_black_alpha(80));
            }
            if self.picked.contains(&shot) {
                let c = cell.right_top() + vec2(-9.0, 9.0);
                painter.circle_filled(c, 6.5, p.accent);
                check_mark(&painter, c, 3.0, p.on_accent);
            }
            if self.rejected.contains(&shot) {
                let c = cell.right_top() + vec2(-9.0, 9.0);
                painter.circle_filled(c, 6.5, theme::DELETE_RED);
                x_mark(&painter, c, 2.6, Color32::WHITE);
            }
            if self.shots[shot].video {
                play_triangle(&painter, cell.left_bottom() + vec2(10.0, -9.0), 4.0, Color32::WHITE);
            }
            if r.clicked() {
                clicked = Some(shot);
            }
        }
        self.request_thumbs(&visible, &[]);
        if let Some(shot) = clicked {
            if shot != self.current {
                self.current = shot;
                self.reset_zoom();
            }
        }
    }

    fn set_fullscreen(&mut self, ctx: &egui::Context, on: bool) {
        if self.fullscreen != on {
            self.fullscreen = on;
            ctx.send_viewport_cmd(ViewportCommand::Fullscreen(on));
        }
    }

    /// Jumps the playing clip by `seconds` (back when negative).
    fn skip_clip(&mut self, seconds: f64) {
        let Some(pb) = self.playback.as_ref().filter(|pb| pb.shot == self.current) else { return };
        let to = pb.player.position() + seconds;
        pb.player.seek(match pb.player.duration() {
            Some(d) => to.clamp(0.0, d),
            None => to.max(0.0),
        });
    }

    /// Plays or pauses the clip in the viewer, starting it if it isn't loaded yet.
    fn toggle_playback(&mut self) {
        if let Some(pb) = &self.playback {
            if pb.shot == self.current {
                if pb.player.is_playing() {
                    pb.player.pause();
                } else {
                    pb.player.play();
                }
                return;
            }
        }
        let Some(shot) = self.shots.get(self.current) else { return };
        let Some(path) = shot.image.clone().filter(|p| shot.video && crate::player::can_play(p)) else { return };
        match crate::player::Player::open(&path) {
            Ok(player) => {
                // Sound on, at the level chosen for the last clip.
                player.set_volume(self.volume);
                player.set_muted(self.muted);
                player.play();
                self.playback = Some(Playback { shot: self.current, player, texture: None, audio_settled: false });
            }
            Err(e) => self.error(e),
        }
    }

    /// The clip playing in the viewer, with its controls. Returns false when no clip is
    /// loaded for the photo on show, leaving the viewer to draw it as usual.
    fn draw_playback(&mut self, ui: &mut Ui, area: Rect, resp: &Response) -> bool {
        let Some(mut pb) = self.playback.take() else { return false };
        if pb.shot != self.current {
            self.playback = Some(pb);
            return false;
        }
        let p = self.p;
        let ctx = ui.ctx().clone();
        let ppp = ctx.pixels_per_point();
        let max = [(area.width() * ppp).max(1.0) as u32, (area.height() * ppp).max(1.0) as u32];
        if let Some(image) = pb.player.frame(max) {
            match &mut pb.texture {
                Some(tex) => tex.set(image, egui::TextureOptions::LINEAR),
                None => pb.texture = Some(ctx.load_texture("video-frame", image, egui::TextureOptions::LINEAR)),
            }
            // Windows' player only starts its sound once the volume changes after the
            // clip is under way: set it again (nudged, so it counts as a change).
            if !pb.audio_settled && pb.player.position() > 0.0 {
                let nudged = if self.volume > 0.5 { self.volume - 0.01 } else { self.volume + 0.01 };
                pb.player.set_volume(nudged);
                pb.player.set_volume(self.volume);
                pb.player.set_muted(!self.muted);
                pb.player.set_muted(self.muted);
                pb.audio_settled = true;
            }
        }

        // The newest frame, or the still frame until the first one arrives.
        let picture = pb
            .texture
            .as_ref()
            .map(|t| (t.id(), t.size_vec2()))
            .or_else(|| self.fulls.get(&pb.shot).map(|f| (f.tex.id(), f.tex.size_vec2())));
        if let Some((id, size)) = picture {
            let fit = fit_rect(area, size);
            Image::new((id, fit.size())).corner_radius(4).paint_at(ui, fit);
        }
        if pb.player.failed() {
            ui.painter().text(area.center(), Align2::CENTER_CENTER, "This clip can't be played here", theme::font(14.0), Color32::WHITE);
        }

        // The control bar: play / pause, time, scrubber, mute, open in the video player.
        let width = area.width().min(760.0) - 24.0;
        let bar = Rect::from_min_size(pos2(area.center().x - width / 2.0, area.max.y - 56.0), vec2(width, 42.0));
        let painter = ui.painter().clone();
        painter.rect_filled(bar, 12, Color32::from_black_alpha(190));
        let white = Color32::WHITE;
        let dim = Color32::from_white_alpha(170);
        let playing = pb.player.is_playing();
        let duration = pb.player.duration();
        let mut position = pb.player.position();

        let play_rect = Rect::from_center_size(pos2(bar.min.x + 24.0, bar.center().y), Vec2::splat(32.0));
        let play_tip = format!("{} ({})", if playing { "Pause" } else { "Play" }, self.settings.keybinds.main_text(KeyAction::PlayPause));
        let play = ui.interact(play_rect, Id::new("clip-play-pause"), Sense::click()).on_hover_text(play_tip).on_hover_cursor(CursorIcon::PointingHand);
        if playing {
            for dx in [-4.0, 4.0] {
                let r = Rect::from_center_size(play_rect.center() + vec2(dx, 0.0), vec2(4.0, 14.0));
                painter.rect_filled(r, 1, white);
            }
        } else {
            play_triangle(&painter, play_rect.center(), 7.0, white);
        }

        let open_g = painter.layout_no_wrap("Open in Player".to_string(), theme::font(12.5), dim);
        let open_rect = Rect::from_min_size(pos2(bar.max.x - open_g.size().x - 18.0, bar.min.y + 7.0), vec2(open_g.size().x + 10.0, 28.0));
        let open = ui.interact(open_rect, Id::new("clip-open"), Sense::click()).on_hover_text("Open the clip in your video player").on_hover_cursor(CursorIcon::PointingHand);
        if open.hovered() {
            painter.rect_filled(open_rect, 6, Color32::from_white_alpha(30));
        }
        painter.galley(open_rect.min + vec2(5.0, (28.0 - open_g.size().y) / 2.0), open_g, dim);

        // Full screen.
        let full_rect = Rect::from_center_size(pos2(open_rect.min.x - 20.0, bar.center().y), Vec2::splat(30.0));
        let full_tip = format!(
            "{} ({})",
            if self.fullscreen { "Exit full screen" } else { "Full screen" },
            self.settings.keybinds.main_text(KeyAction::FullScreen)
        );
        let full = ui.interact(full_rect, Id::new("clip-full"), Sense::click()).on_hover_text(full_tip).on_hover_cursor(CursorIcon::PointingHand);
        fullscreen_icon(&painter, full_rect.center(), 7.0, if full.hovered() { white } else { dim }, self.fullscreen);

        // Volume: a slider, and the speaker to mute. Both carry on to the next clip.
        let volume_track = Rect::from_min_max(
            pos2(full_rect.min.x - 10.0 - 72.0, bar.center().y - 2.0),
            pos2(full_rect.min.x - 10.0, bar.center().y + 2.0),
        );
        let volume = ui.interact(volume_track.expand2(vec2(6.0, 12.0)), Id::new("clip-volume"), Sense::click_and_drag()).on_hover_cursor(CursorIcon::PointingHand);
        if let Some(pos) = volume.interact_pointer_pos().filter(|_| volume.clicked() || volume.dragged()) {
            self.volume = ((pos.x - volume_track.min.x) / volume_track.width()).clamp(0.0, 1.0);
            self.muted = false;
            pb.player.set_volume(self.volume);
            pb.player.set_muted(false);
        }
        let shown_volume = if self.muted { 0.0 } else { self.volume };
        volume.on_hover_text(format!("Volume {}%", (shown_volume * 100.0).round()));
        painter.rect_filled(volume_track, 2, Color32::from_white_alpha(60));
        let level = Rect::from_min_max(volume_track.min, pos2(volume_track.min.x + volume_track.width() * shown_volume, volume_track.max.y));
        painter.rect_filled(level, 2, white);
        painter.circle_filled(pos2(level.max.x, volume_track.center().y), 5.0, white);

        let mute_rect = Rect::from_center_size(pos2(volume_track.min.x - 20.0, bar.center().y), Vec2::splat(30.0));
        let mute = ui.interact(mute_rect, Id::new("clip-mute"), Sense::click()).on_hover_text(if self.muted { "Unmute" } else { "Mute" }).on_hover_cursor(CursorIcon::PointingHand);
        let speaker = if self.muted || self.volume == 0.0 { "🔇" } else { "🔊" };
        painter.text(mute_rect.center(), Align2::CENTER_CENTER, speaker, theme::font(15.0), if mute.hovered() { white } else { dim });

        let time_text = |s: f64| {
            let s = s.max(0.0).round() as u64;
            format!("{}:{:02}", s / 60, s % 60)
        };
        let time = format!("{} / {}", time_text(position), duration.map_or("–:––".to_string(), time_text));
        let time_g = painter.layout_no_wrap(time, theme::font(12.5), dim);
        let time_pos = pos2(play_rect.max.x + 8.0, bar.center().y - time_g.size().y / 2.0);
        let track_left = time_pos.x + time_g.size().x + 14.0;
        painter.galley(time_pos, time_g, dim);

        let track = Rect::from_min_max(pos2(track_left, bar.center().y - 2.0), pos2(mute_rect.min.x - 12.0, bar.center().y + 2.0));
        let scrub = ui.interact(track.expand2(vec2(6.0, 12.0)), Id::new("clip-scrub"), Sense::click_and_drag()).on_hover_cursor(CursorIcon::PointingHand);
        if let (Some(d), Some(pos)) = (duration, scrub.interact_pointer_pos()) {
            if scrub.clicked() || scrub.dragged() {
                let f = ((pos.x - track.min.x) / track.width()).clamp(0.0, 1.0) as f64;
                position = f * d;
                pb.player.seek(position);
            }
        }
        let progress = duration.map_or(0.0, |d| (position / d).clamp(0.0, 1.0) as f32);
        painter.rect_filled(track, 2, Color32::from_white_alpha(60));
        let done = Rect::from_min_max(track.min, pos2(track.min.x + track.width() * progress, track.max.y));
        painter.rect_filled(done, 2, p.accent);
        painter.circle_filled(pos2(done.max.x, track.center().y), if scrub.hovered() || scrub.dragged() { 7.0 } else { 5.5 }, white);
        // The time under the cursor, above the bar.
        if let (Some(d), Some(pos)) = (duration, scrub.hover_pos().or(scrub.interact_pointer_pos())) {
            let at = ((pos.x - track.min.x) / track.width()).clamp(0.0, 1.0) as f64 * d;
            let g = painter.layout_no_wrap(time_text(at), theme::font_semibold(12.0), white);
            let x = pos.x.clamp(track.min.x, track.max.x);
            let label = Rect::from_center_size(pos2(x, bar.min.y - 16.0), g.size() + vec2(14.0, 6.0));
            painter.rect_filled(label, 6, Color32::from_black_alpha(210));
            painter.galley(label.center() - g.size() / 2.0, g, white);
        }

        // Clicking the picture (not the bar) plays or pauses.
        let on_bar = resp.interact_pointer_pos().is_some_and(|pos| bar.contains(pos));
        let toggle = play.clicked() || (resp.clicked() && !on_bar);
        if mute.clicked() {
            self.muted = !self.muted;
            pb.player.set_muted(self.muted);
        }
        let toggle_full = full.clicked();
        if open.clicked() {
            pb.player.pause();
            if let Some(path) = self.shots.get(pb.shot).and_then(|s| s.display_path()) {
                crate::window::open_link(&path.to_string_lossy());
            }
        }
        self.playback = Some(pb);
        if toggle {
            self.toggle_playback();
        }
        if toggle_full {
            self.set_fullscreen(&ctx, !self.fullscreen);
        }
        // Frames keep coming while it plays; after a seek, the new frame arrives shortly.
        if playing {
            ctx.request_repaint();
        } else {
            ctx.request_repaint_after(std::time::Duration::from_millis(60));
        }
        true
    }

    fn toggle_pick(&mut self, shot: usize) {
        let pick = !self.picked.contains(&shot);
        self.set_picked(&[shot], pick);
    }

    /// Picks or unpicks shots. Picks are always remembered. With Export on Pick on,
    /// picking also copies the chosen files to the export folder (never twice: a copy
    /// already there is reused). Unpicking only removes the mark; copies stay.
    fn set_picked(&mut self, shots: &[usize], pick: bool) {
        let dest = if pick && self.settings.export_on_pick { self.export_dir_or_ask() } else { None };
        for &shot in shots {
            if self.picked.contains(&shot) == pick {
                continue;
            }
            if pick {
                self.picked.insert(shot);
                self.rejected.remove(&shot);
                if let Some(dest) = &dest {
                    let files = self.export_files(shot);
                    self.exporter.copy(files, dest.clone(), None);
                }
            } else {
                self.picked.remove(&shot);
            }
        }
        self.save_picks();
    }

    /// The files of a shot that Export on Pick and Quick Export copy (per Settings).
    fn export_files(&self, shot: usize) -> Vec<PathBuf> {
        let s = &self.shots[shot];
        self.settings.export_files.pick(s.image.as_ref(), s.raw.as_ref()).into_iter().cloned().collect()
    }

    /// Hover text for picking: what picking does with the current settings.
    fn pick_tip(&self) -> &'static str {
        if self.settings.export_on_pick {
            "Pick: mark and copy to the export folder (Export on Pick is on)"
        } else {
            "Pick: mark as a keeper"
        }
    }

    /// Grid: picks every selected shot, or unpicks them if they're all picked already.
    fn pick_selected(&mut self) {
        let mut shots: Vec<usize> = self.selected.iter().copied().collect();
        if shots.is_empty() {
            return;
        }
        shots.sort_unstable();
        let all_picked = shots.iter().all(|s| self.picked.contains(s));
        self.set_picked(&shots, !all_picked);
    }

    /// Copies the selected photos (grid) or the shown photo (viewer): the files chosen
    /// under Settings → Export on Pick.
    fn quick_export(&mut self) {
        let mut shots: Vec<usize> = match self.screen {
            Screen::Viewer => vec![self.current],
            Screen::Grid => self.selected.iter().copied().collect(),
        };
        if shots.is_empty() {
            self.message = Some(("Select photos to export first".into(), false));
            return;
        }
        shots.sort_unstable();
        let Some(dest) = self.export_dir_or_ask() else { return };
        let batch = self.next_batch;
        self.next_batch += 1;
        self.batches.insert(batch, Batch { remaining: shots.len(), shots: shots.len(), failed: false, dest: dest.clone() });
        for shot in shots {
            let files = self.export_files(shot);
            self.exporter.copy(files, dest.clone(), Some(batch));
        }
    }

    fn error(&mut self, text: String) {
        self.message = Some((text, true));
    }

    fn open_viewer(&mut self, shot: usize) {
        self.current = shot;
        self.screen = Screen::Viewer;
        self.reset_zoom();
    }

    fn back_to_grid(&mut self) {
        self.screen = Screen::Grid;
        self.scroll_to_current = true;
        // Select the photo just viewed, if it's still on the grid (it may have been
        // deleted, or unpicked in View Picked).
        self.selected.clear();
        self.select_anchor = None;
        if self.working_set().contains(&self.current) {
            self.selected.insert(self.current);
            self.select_anchor = Some(self.current);
        }
        // Photos read ahead stay in memory for next time; the rest go.
        let keep = &self.cache_keep;
        self.fulls.retain(|s, _| keep.contains(s));
        self.original = None;
        self.reset_zoom();
        self.loader.want_full(Vec::new());
    }

    /// Reads photos ahead of time, within the memory Settings allow: small versions of
    /// every photo nearest to the one in view first (two ahead for every one behind),
    /// then screen-sized ones with the memory left over. With "Use less memory" on,
    /// only the photos right around the one on show are kept, as before.
    fn read_ahead(&mut self) {
        self.cache_keep.clear();
        self.thumb_keep.clear();
        let off = self.settings.memory_saver || self.folder.is_none() || self.drive_scan.is_some() || self.deleting.is_some();
        let total = crate::platform::total_memory();
        let (false, Some(total)) = (off, total) else {
            self.loader.want_background(Vec::new());
            if self.screen == Screen::Grid {
                self.fulls.clear();
            }
            return;
        };
        let budget = (total as f64 * self.settings.memory_percent as f64 / 100.0) as u64;
        let set = self.working_set();
        let anchor = if self.screen == Screen::Viewer { self.current } else { self.grid_anchor };
        let order = read_ahead_order(&set, anchor);

        // What a small and a screen-sized photo take (pixels are 4 bytes; most photos are 3:2).
        let thumb_bytes = THUMB_PX as u64 * THUMB_PX as u64 * 4 * 2 / 3;
        let screen_bytes = (self.viewer_px[0] as u64 * self.viewer_px[1] as u64 * 4 * 2 / 3).max(1);
        let thumbs = ((budget / thumb_bytes) as usize).min(order.len());
        let left = budget.saturating_sub(thumbs as u64 * thumb_bytes);
        let screens = ((left / screen_bytes) as usize).min(order.len());
        self.thumb_keep = order[..thumbs].iter().copied().collect();
        self.cache_keep = order[..screens].iter().copied().collect();

        // A few at a time, nearest first: the queue is renewed every frame.
        const BATCH: usize = 48;
        let mut requests = Vec::new();
        for &shot in &order[..thumbs] {
            if requests.len() >= BATCH {
                break;
            }
            let key = Key { shot, kind: Kind::Thumb };
            let have = self.thumbs.get(&shot).is_some_and(|t| t.kind == Kind::Thumb);
            if let (false, false, Some(path)) = (have, self.failed.contains_key(&key), self.shots[shot].display_path()) {
                requests.push(Request { key, path: path.clone(), max: [THUMB_PX, THUMB_PX] });
            }
        }
        for &shot in &order[..screens] {
            if requests.len() >= BATCH {
                break;
            }
            let key = Key { shot, kind: Kind::Full };
            if let (false, false, Some(path)) = (self.fulls.contains_key(&shot), self.failed.contains_key(&key), self.shots[shot].display_path()) {
                requests.push(Request { key, path: path.clone(), max: self.viewer_px });
            }
        }
        self.loader.want_background(requests);
        if self.screen == Screen::Grid {
            let keep = &self.cache_keep;
            self.fulls.retain(|s, _| keep.contains(s));
        }
    }

    fn reset_zoom(&mut self) {
        self.last_viewer_click = None;
        self.zoom = None;
        self.zoom_percent = None;
    }

    /// The shots in view, in order: picked ones (View Picked), the selected range,
    /// or the whole folder. The grid, the viewer's arrows and the counts all use it.
    fn working_set(&self) -> Vec<usize> {
        if self.view_picked {
            let mut picked: Vec<usize> = self.picked.iter().copied().collect();
            picked.sort_unstable();
            return picked;
        }
        if self.view_rejected {
            let mut rejected: Vec<usize> = self.rejected.iter().copied().collect();
            rejected.sort_unstable();
            return rejected;
        }
        match (self.range_mode, self.range) {
            (RangeMode::Off, Some((from, to))) => (from..=to).collect(),
            _ => (0..self.shots.len()).collect(),
        }
    }

    fn items(&self) -> Vec<Item> {
        let mut items = Vec::new();
        for shot in self.working_set() {
            let s = &self.shots[shot];
            if s.raw.is_some() && (self.settings.show_raw || s.image.is_none()) {
                items.push(Item { shot, raw: true });
            }
            if s.image.is_some() {
                items.push(Item { shot, raw: false });
            }
        }
        items
    }

    /// Moves the viewer `delta` photos through the working set.
    fn step(&mut self, delta: isize) {
        let set = self.working_set();
        if set.is_empty() {
            return;
        }
        // Where the current photo is (or would be, if it just left the set, e.g. unpicked).
        let pos = match set.binary_search(&self.current) {
            Ok(i) => i as isize,
            Err(i) => i as isize - if delta > 0 { 1 } else { 0 },
        };
        let next = set[(pos + delta).clamp(0, set.len() as isize - 1) as usize];
        if next != self.current {
            self.current = next;
            self.reset_zoom();
        }
    }

    /// "3 of 120"-style position of the viewer's photo in the working set.
    fn position_label(&self) -> String {
        let set = self.working_set();
        match set.binary_search(&self.current) {
            Ok(i) => format!("{} of {}", i + 1, set.len()),
            Err(_) => format!("– of {}", set.len()),
        }
    }

    /// Windows Explorer rules: click selects one, Ctrl+click toggles, Shift+click
    /// selects everything from the last clicked photo (Ctrl+Shift adds to the selection).
    fn click_select(&mut self, shot: usize, items: &[Item], mods: Modifiers) {
        if mods.shift {
            let anchor = self.select_anchor.unwrap_or(shot);
            if !mods.command {
                self.selected.clear();
            }
            let a = items.iter().position(|it| it.shot == anchor);
            let b = items.iter().position(|it| it.shot == shot);
            if let (Some(a), Some(b)) = (a, b) {
                for it in &items[a.min(b)..=a.max(b)] {
                    self.selected.insert(it.shot);
                }
            } else {
                self.selected.insert(shot);
            }
            if self.select_anchor.is_none() {
                self.select_anchor = Some(shot);
            }
        } else if mods.command {
            if !self.selected.remove(&shot) {
                self.selected.insert(shot);
            }
            self.select_anchor = Some(shot);
        } else {
            self.selected.clear();
            self.selected.insert(shot);
            self.select_anchor = Some(shot);
        }
    }

    fn show_toast(&mut self, ctx: &egui::Context, text: String) {
        self.toast = Some(Toast { text, started: ctx.input(|i| i.time) });
    }

    // ---------------------------------------------------------------- background results

    fn receive(&mut self, ctx: &egui::Context) {
        while let Some(loaded) = self.loader.try_recv() {
            self.loader.finished(loaded.key, loaded.generation);
            if loaded.generation != self.generation {
                continue;
            }
            let key = loaded.key;
            let animation = loaded.animation;
            let (image, original) = match loaded.image {
                Ok(v) => v,
                Err(reason) => {
                    self.failed.insert(key, reason);
                    continue;
                }
            };
            match key.kind {
                Kind::Full => {
                    let tex = ctx.load_texture(format!("full-{}", key.shot), image, TextureOptions::LINEAR);
                    let original = vec2(original[0] as f32, original[1] as f32);
                    let frames = animation
                        .into_iter()
                        .enumerate()
                        .map(|(i, (img, delay))| {
                            (ctx.load_texture(format!("frame-{}-{i}", key.shot), img, TextureOptions::LINEAR), delay)
                        })
                        .collect();
                    self.fulls.insert(key.shot, FullImage { tex, original, frames });
                }
                Kind::Original => {
                    if key.shot == self.current && self.screen == Screen::Viewer {
                        let tex = ctx.load_texture(format!("orig-{}", key.shot), image, TextureOptions::LINEAR);
                        self.original = Some((key.shot, tex));
                    }
                }
                Kind::Preview | Kind::Thumb => {
                    let better_exists = key.kind == Kind::Preview
                        && self.thumbs.get(&key.shot).is_some_and(|t| t.kind == Kind::Thumb);
                    if !better_exists {
                        let tex = ctx.load_texture(format!("thumb-{}", key.shot), image, TextureOptions::LINEAR);
                        self.thumbs.insert(key.shot, Thumb { tex, kind: key.kind, last_used: self.frame });
                    }
                }
            }
        }

        while let Some(Done { batch, result }) = self.exporter.try_recv() {
            // A failed copy is reported; the pick itself stays marked.
            if let Err(e) = &result {
                self.error(e.clone());
            }
            let Some(id) = batch else { continue };
            let Some(b) = self.batches.get_mut(&id) else { continue };
            b.remaining -= 1;
            b.failed |= result.is_err();
            if b.remaining == 0 {
                let b = self.batches.remove(&id).unwrap();
                if !b.failed {
                    let what = if b.shots == 1 { "1 photo".to_string() } else { format!("{} photos", b.shots) };
                    self.show_toast(ctx, format!("Exported {what} to {}", folder_name(&b.dest)));
                }
            }
        }

        // Small versions read ahead are kept; of the rest, the least recently shown go.
        let cap = MAX_THUMBS + self.thumb_keep.len();
        if self.thumbs.len() > cap {
            let keep = &self.thumb_keep;
            let mut by_age: Vec<(u64, usize)> =
                self.thumbs.iter().filter(|(k, _)| !keep.contains(k)).map(|(k, t)| (t.last_used, *k)).collect();
            by_age.sort_unstable();
            let extra = self.thumbs.len() - (MAX_THUMBS * 4 / 5 + self.thumb_keep.len());
            for (_, shot) in by_age.iter().take(extra) {
                self.thumbs.remove(shot);
            }
        }
    }

    fn request_thumbs(&self, visible: &[usize], ahead: &[usize]) {
        let mut previews = Vec::new();
        let mut sharp = Vec::new();
        for (i, &shot) in visible.iter().chain(ahead).enumerate() {
            let Some(path) = self.shots[shot].display_path() else { continue };
            let have = self.thumbs.get(&shot).map(|t| t.kind);
            let is_visible = i < visible.len();
            let preview = Key { shot, kind: Kind::Preview };
            if have.is_none() && is_visible && !self.failed.contains_key(&preview) {
                previews.push(Request { key: preview, path: path.clone(), max: [THUMB_PX, THUMB_PX] });
            }
            let thumb = Key { shot, kind: Kind::Thumb };
            if have != Some(Kind::Thumb) && !self.failed.contains_key(&thumb) {
                sharp.push(Request { key: thumb, path: path.clone(), max: [THUMB_PX, THUMB_PX] });
            }
        }
        previews.extend(sharp);
        self.loader.want_thumbs(previews);
    }

    // ---------------------------------------------------------------- bars

    fn top_bar(&mut self, ctx: &egui::Context) {
        let p = self.p;
        egui::TopBottomPanel::top("top")
            // With Mica, the top bar is left see-through so Windows draws the wallpaper tint.
            .frame(Frame::new().fill(if self.mica_active { Color32::TRANSPARENT } else { p.bar }).inner_margin(Margin::symmetric(14, 9)))
            .show(ctx, |ui| {
                ui.horizontal(|ui| {
                    ui.set_min_height(32.0);
                    if self.screen == Screen::Viewer {
                        let name = self.folder.as_deref().map(folder_name).unwrap_or_default();
                        if back_button(ui, &p, &name).on_hover_text("Back to grid (Esc)").clicked() {
                            self.back_to_grid();
                        }
                    } else {
                        let label = if self.folder.is_some() { "🗁" } else { "🗁  Open Folder" };
                        if ui.add(Button::new(label)).on_hover_text(format!("Open folder ({})", self.settings.keybinds.main_text(KeyAction::OpenFolder))).clicked() {
                            self.choose_folder(ctx);
                        }
                        self.recent_menu(ui, ctx);
                        if self.folder.is_some() {
                            // Photos / Videos, when the folder has both.
                            if !self.other_shots.is_empty() {
                                ui.add_space(14.0);
                                let mut switch_to = None;
                                ui.scope(|ui| {
                                    ui.spacing_mut().item_spacing.x = 2.0;
                                    for (media, label) in [(Media::Photos, "Photos"), (Media::Videos, "Videos")] {
                                        let on = self.media == media;
                                        let mut b = Button::new(RichText::new(label).color(if on { p.on_accent } else { p.label }));
                                        if on {
                                            b = b.fill(p.accent).stroke(Stroke::new(1.0_f32, p.accent));
                                        }
                                        if ui.add(b).clicked() {
                                            switch_to = Some(media);
                                        }
                                    }
                                });
                                if let Some(media) = switch_to {
                                    self.switch_media(media);
                                }
                            }
                            if self.has_raw_pairs {
                                ui.add_space(14.0);
                                let on = self.settings.show_raw;
                                let mut b = Button::new(RichText::new("Show Paired RAW").color(if on { p.on_accent } else { p.label }));
                                if on {
                                    b = b.fill(p.accent).stroke(Stroke::new(1.0_f32, p.accent));
                                }
                                let tip = "Give the RAW file of each pair its own tile. RAWs with nothing to pair with always show.";
                                if ui.add(b).on_hover_text(tip).clicked() {
                                    self.settings.show_raw = !on;
                                    self.settings.save();
                                }
                            }
                            ui.add_space(8.0);
                            self.sort_menu(ui);
                        }
                    }

                    ui.with_layout(Layout::right_to_left(Align::Center), |ui| {
                        self.export_button(ui);
                        // Right-to-left: the cog lands just left of "Export to".
                        let open = self.settings_page.is_some();
                        if cog_button(ui, &p, open).on_hover_text("Settings").clicked() {
                            self.settings_page = if open { None } else { Some(SettingsPage::DragDrop) };
                        }
                        let new_version = self.new_version.lock().unwrap().clone();
                        if let Some(version) = new_version {
                            if update_button(ui, &p).on_hover_text(format!("Update available: version {version}")).clicked() {
                                self.update_prompt = true;
                            }
                        }
                    });
                });

                // Centered title, drawn over the bar so it stays centered in the window.
                let center = ui.max_rect().center();
                let painter = ui.painter();
                match self.screen {
                    Screen::Viewer if !self.shots.is_empty() => {
                        let shot = &self.shots[self.current];
                        let title = if shot.image.is_some() { shot.image_name() } else { shot.raw_name() };
                        painter.text(center - vec2(0.0, 7.0), Align2::CENTER_CENTER, title, theme::font_semibold(13.5), p.label);
                        painter.text(center + vec2(0.0, 10.0), Align2::CENTER_CENTER, shot.summary(), theme::font(11.0), p.label2);
                    }
                    _ if self.folder.is_none() => {
                        painter.text(center, Align2::CENTER_CENTER, "Cullery", theme::font_semibold(13.5), p.label2);
                    }
                    _ => {}
                }
            });
    }

    fn recent_menu(&mut self, ui: &mut Ui, ctx: &egui::Context) {
        let p = self.p;
        let title = match &self.folder {
            Some(dir) => format!("{}     ", short_path(dir)),
            None => "Recent     ".to_string(),
        };
        let mut open = None;
        let menu = ui.menu_button(RichText::new(title).color(p.label), |ui| {
            if self.settings.recent.is_empty() {
                ui.label(RichText::new("No recent folders").color(p.label2));
            }
            for dir in &self.settings.recent {
                if ui.button(dir.display().to_string()).clicked() {
                    open = Some(dir.clone());
                    ui.close();
                }
            }
        });
        let r = menu.response.rect;
        chevron_down(ui.painter(), pos2(r.max.x - 16.0, r.center().y), 4.0, p.label2);
        // A shortened path shows in full on hover.
        if let Some(dir) = self.folder.as_ref().filter(|d| short_path(d) != d.display().to_string()) {
            menu.response.on_hover_text(dir.display().to_string());
        }
        if let Some(dir) = open {
            self.open_folder(ctx, dir);
        }
    }

    /// "Sort: Name", with the same choices as File Explorer's Sort by menu.
    fn sort_menu(&mut self, ui: &mut Ui) {
        let p = self.p;
        let (by, descending) = (self.settings.sort_by, self.settings.sort_descending);
        let mut chosen = None;
        let title = format!("Sort: {}     ", by.label());
        let menu = ui.menu_button(RichText::new(title).color(p.label), |ui| {
            ui.set_min_width(130.0);
            for option in SortBy::ALL {
                if ui.radio(option == by, option.label()).clicked() {
                    chosen = Some((option, descending));
                    ui.close();
                }
            }
            ui.separator();
            for (label, direction) in [("Ascending", false), ("Descending", true)] {
                if ui.radio(direction == descending, label).clicked() {
                    chosen = Some((by, direction));
                    ui.close();
                }
            }
        });
        let r = menu.response.rect;
        chevron_down(ui.painter(), pos2(r.max.x - 16.0, r.center().y), 4.0, p.label2);
        if let Some((new_by, new_descending)) = chosen.filter(|c| *c != (by, descending)) {
            self.settings.sort_by = new_by;
            self.settings.sort_descending = new_descending;
            self.settings.save();
            self.resort();
        }
    }

    /// Puts the open folder in the chosen order, keeping picks, selection and
    /// thumbnails with their photos. A range is a run of neighbours, so it ends here.
    fn resort(&mut self) {
        let mut sorted = self.shots.clone();
        library::sort(&mut sorted, self.settings.sort_by, self.settings.sort_descending);
        library::sort(&mut self.other_shots, self.settings.sort_by, self.settings.sort_descending);
        self.range = None;
        self.range_mode = RangeMode::Off;
        self.adopt_shots(sorted);
        self.view_anchor = None;
        self.pending_scroll = Some(0.0);
    }

    fn export_button(&mut self, ui: &mut Ui) {
        let p = self.p;
        let mut job = LayoutJob::default();
        let plain = TextFormat { font_id: theme::font(13.5), color: p.label, ..Default::default() };
        job.append("Export to: ", 0.0, plain.clone());
        let tip = match &self.settings.export_dir {
            Some(dir) => {
                let folder = TextFormat { font_id: theme::font_semibold(13.5), color: p.tint, ..Default::default() };
                job.append(&folder_name(dir), 0.0, folder);
                format!("Exports go to {}\nClick to change", dir.display())
            }
            None => {
                job.append("Not Set", 0.0, TextFormat { color: p.label2, ..plain });
                "Choose where photos are exported".to_string()
            }
        };
        let button = Button::new(job)
            .corner_radius(p.r_pill)
            .fill(p.control)
            .stroke(Stroke::new(1.0_f32, p.control_border));
        if ui.add(button).on_hover_text(tip).clicked() {
            self.choose_export();
        }
    }

    fn bottom_bar(&mut self, ctx: &egui::Context) {
        let p = self.p;
        egui::TopBottomPanel::bottom("bottom")
            .frame(Frame::new().fill(p.bar).inner_margin(Margin::symmetric(16, 10)))
            .show(ctx, |ui| {
                ui.horizontal(|ui| {
                    ui.set_min_height(44.0);
                    if self.folder.is_none() {
                        let center = ui.max_rect().center();
                        let (text, color) = match &self.message {
                            Some((text, true)) => (text.as_str(), p.danger),
                            _ => ("Open a folder to start", p.label2),
                        };
                        ui.painter().text(center, Align2::CENTER_CENTER, text, theme::font(13.5), color);
                        return;
                    }
                    match self.screen {
                        Screen::Grid => self.grid_controls(ui),
                        Screen::Viewer => self.viewer_controls(ui),
                    }
                });
            });
    }

    fn quick_export_button(&mut self, ui: &mut Ui, enabled: bool) {
        let p = self.p;
        let mut job = LayoutJob::default();
        job.append("Quick Export", 0.0, TextFormat { font_id: theme::font(13.5), color: p.label, ..Default::default() });
        job.append(&format!("   {}", self.settings.keybinds.hint(KeyAction::QuickExport)), 0.0, key_hint(12.0, p.label2));
        let tip = format!("Copy {} to the export folder", self.settings.export_files.label());
        if ui.add_enabled(enabled, Button::new(job)).on_hover_text(tip).clicked() {
            self.quick_export();
        }
    }

    fn grid_controls(&mut self, ui: &mut Ui) {
        let p = self.p;
        let bar = ui.max_rect();
        // While choosing a range, the grid doesn't show the selection, so nothing acts on it.
        let selecting = self.range_mode != RangeMode::Off;
        let can_act = !self.selected.is_empty() && !selecting;
        self.quick_export_button(ui, can_act);

        let mut range_btn = Button::new(RichText::new("Select Range").color(if selecting { p.on_accent } else { p.label }));
        if selecting {
            range_btn = range_btn.fill(p.accent).stroke(Stroke::new(1.0_f32, p.accent));
        }
        if ui.add(range_btn).on_hover_text("Click the first and last shot of a set to work on just those").clicked() {
            self.range_mode = if selecting { RangeMode::Off } else { RangeMode::PickStart };
            if !selecting {
                self.view_picked = false;
                self.view_rejected = false;
            }
        }

        ui.add_space(6.0);
        size_icon(ui, &p, 9.0);
        ui.spacing_mut().slider_width = 110.0;
        let size = ui.add(Slider::new(&mut self.settings.tile_width, 120.0..=360.0).show_value(false));
        if self.settings.tile_width_changed() && !size.is_pointer_button_down_on() {
            self.settings.save();
        }
        size_icon(ui, &p, 15.0);

        // Delete, then the Pick button centered in the bar, for everything selected.
        let pad = bar.center().x - PICK_W / 2.0 - TRASH_GAP - TRASH_W - ui.cursor().min.x;
        if pad > 0.0 {
            ui.add_space(pad);
        }
        let tip = "Delete the selected photos and their RAW files (Delete)";
        if trash_button(ui, &p, can_act).on_hover_text(tip).clicked() {
            self.request_delete();
        }
        ui.add_space(TRASH_GAP - ui.spacing().item_spacing.x);
        let n = self.selected.len();
        let all_picked = n > 0 && self.selected.iter().all(|s| self.picked.contains(s));
        let label = match (n, all_picked) {
            (_, true) => "Picked".to_string(),
            (0 | 1, _) => "Pick".to_string(),
            (n, _) => format!("Pick {n}"),
        };
        let tip = if all_picked { "Unpick the selected photos" } else { self.pick_tip() };
        if pick_button(ui, &p, &label, all_picked, can_act).on_hover_text(tip).clicked() {
            self.pick_selected();
        }

        ui.with_layout(Layout::right_to_left(Align::Center), |ui| {
            // View Picked (rightmost): show only picked photos.
            let on = self.view_picked;
            let mut b = Button::new(RichText::new("View Picked").color(if on { p.on_accent } else { p.label }));
            if on {
                b = b.fill(p.accent).stroke(Stroke::new(1.0_f32, p.accent));
            }
            let tip = if on { "Show the whole folder again" } else { "Show only picked photos, ready to drag into Lightroom or LightCraft" };
            if ui.add(b).on_hover_text(tip).clicked() {
                self.set_view_picked(!on);
            }
            // View Rejected: the rejects together, to check them and delete them in one go.
            let on = self.view_rejected;
            let red = theme::DELETE_RED;
            let mut b = Button::new(RichText::new("View Rejected").color(if on { Color32::WHITE } else { p.label }));
            // Red only while on, so it's clear the grid is showing just the rejects.
            if on {
                b = b.fill(red).stroke(Stroke::new(1.0_f32, red));
            }
            let tip = if on {
                "Show the whole folder again".to_string()
            } else {
                format!("Show only rejected photos ({} to reject)", self.settings.keybinds.main_text(KeyAction::Reject))
            };
            if ui.add(b).on_hover_text(tip).clicked() {
                self.set_view_rejected(!on);
            }
            self.picked_badge(ui);
            if !self.shots.is_empty() {
                let mut text = format!("{} shots", self.working_set().len());
                if self.selected.len() > 1 {
                    text = format!("{} selected · {text}", self.selected.len());
                }
                ui.label(RichText::new(text).color(p.label2));
            }
            self.message_label(ui);
        });
    }

    /// Drags photos out of the window: the whole selection if `shot` is in it,
    /// otherwise just `shot` (which becomes the selection, like Explorer).
    fn drag_out(&mut self, shot: usize, items: &[Item]) {
        if !self.selected.contains(&shot) {
            self.click_select(shot, items, Modifiers::NONE);
        }
        let mut shots: Vec<usize> = self.selected.iter().copied().collect();
        shots.sort_unstable();
        let paths = self.drag_paths(&shots);
        self.last_drag = paths.iter().cloned().collect();
        let (title, count, note) = drag_out_hint(&shots.iter().map(|&s| &self.shots[s]).collect::<Vec<_>>(), self.settings.drag_files);
        // The stack under the cursor: the photo grabbed in front, then the rest.
        let order: Vec<usize> = std::iter::once(shot).chain(shots.iter().copied().filter(|&s| s != shot)).collect();
        let stack: Vec<PathBuf> = order.iter().filter_map(|&s| self.shots[s].display_path().cloned()).take(3).collect();
        // The window can't redraw while Windows runs the drag, so this frame shows the
        // overlay and the next one hands the drag to Windows (the overlay stays on screen
        // until the drop).
        self.pending_drag = Some(PendingDrag { paths, stack, photos: shots.len(), title, count, note, shown: false, ended: None });
        self.last_tile_click = None;
    }

    /// Starts the drag set up by `drag_out`, once its overlay has been on screen.
    fn start_pending_drag(&mut self, ctx: &egui::Context) {
        // A drag macOS is running: the overlay stays up until it's over.
        if let Some(ended) = self.pending_drag.as_ref().and_then(|d| d.ended.as_ref()) {
            if ended.load(AtomicOrdering::SeqCst) {
                self.pending_drag = None;
                self.release_pointer = true;
                ctx.request_repaint();
            }
            return;
        }
        if !self.pending_drag.as_ref().is_some_and(|d| d.shown) {
            return;
        }
        let Some(mut drag) = self.pending_drag.take() else { return };
        let Some(hwnd) = self.hwnd else { return };
        // macOS sizes the picture in points and adds its own count, and there's no text
        // drawing for a badge here.
        let (scale, badge) = if cfg!(target_os = "macos") { (1.0, 1) } else { (ctx.pixels_per_point(), drag.photos) };
        let px = (110.0 * scale).round() as u32;
        let pictures: Vec<egui::ColorImage> =
            drag.stack.iter().filter_map(|p| crate::loader::quick_picture(p, [px, px])).collect();
        let image = crate::dragimage::stack(&pictures, badge, self.p.accent, scale);
        let ended = Arc::new(AtomicBool::new(false));
        let on_end = {
            let (ended, ctx) = (ended.clone(), ctx.clone());
            move || {
                ended.store(true, AtomicOrdering::SeqCst);
                ctx.request_repaint();
            }
        };
        if let Err(e) = crate::dragout::drag_files(hwnd, &drag.paths, Some(&image), on_end) {
            self.error(format!("Couldn't start the drag: {e}"));
        } else if !ended.load(AtomicOrdering::SeqCst) {
            drag.ended = Some(ended);
            self.pending_drag = Some(drag);
        }
    }

    /// While photos are dragged out: dim the window and say what's being sent.
    fn draw_drag_out_overlay(&mut self, ctx: &egui::Context) {
        let Some(drag) = &mut self.pending_drag else { return };
        drag.shown = true;
        let p = self.p;
        let screen = ctx.screen_rect();
        let painter = ctx.layer_painter(LayerId::new(Order::Foreground, Id::new("drag-out-overlay")));
        painter.rect_filled(screen, 0, Color32::from_black_alpha(170));

        let lines = vec![
            painter.layout_no_wrap(drag.title.clone(), theme::font_semibold(18.0), p.label),
            painter.layout_no_wrap(drag.count.clone(), theme::font_semibold(14.0), p.tint),
            painter.layout_no_wrap(drag.note.clone(), theme::font(13.0), p.label2),
        ];
        center_card(&painter, screen.center(), &p, lines, &[10.0, 4.0], vec2(32.0, 28.0), p.separator);
        // The next frame starts the drag; once it's running, its end asks for a repaint.
        if drag.ended.is_none() {
            ctx.request_repaint();
        }
    }

    /// The files to drag for these shots, per the Settings choice. A file already copied
    /// to the export folder is dragged from there, so editors import from this computer
    /// rather than the memory card.
    fn drag_paths(&self, shots: &[usize]) -> Vec<PathBuf> {
        let mut paths = Vec::new();
        for &s in shots {
            let shot = &self.shots[s];
            for file in self.settings.drag_files.pick(shot.image.as_ref(), shot.raw.as_ref()) {
                let copy = self.settings.export_dir.as_ref().and_then(|dir| export::existing_copy(file, dir));
                paths.push(copy.unwrap_or_else(|| file.clone()));
            }
        }
        paths
    }

    /// Something dropped onto the window. The open folder only changes when a folder is
    /// dropped (or through the buttons): dropped files never switch it. Photos dragged
    /// out of Cullery that land back on it are ignored.
    fn handle_drops(&mut self, ctx: &egui::Context) {
        let dropped: Vec<PathBuf> = ctx.input(|i| i.raw.dropped_files.iter().filter_map(|f| f.path.clone()).collect());
        if dropped.is_empty() || self.deleting.is_some() || self.drive_scan.is_some() {
            return;
        }
        if dropped.iter().all(|p| self.last_drag.contains(p)) {
            return;
        }
        match dropped.iter().find(|p| p.is_dir()) {
            Some(dir) => self.open_folder(ctx, dir.clone()),
            None => self.message = Some(("To open photos, drop their folder onto Cullery".into(), false)),
        }
    }

    /// Asks to delete the selected photos (grid) or the shown one (viewer).
    fn request_delete(&mut self) {
        let mut shots: Vec<usize> = match self.screen {
            Screen::Viewer if !self.shots.is_empty() => vec![self.current],
            _ => self.selected.iter().copied().collect(),
        };
        shots.sort_unstable();
        if shots.is_empty() {
            self.message = Some(("Select photos to delete first".into(), false));
            return;
        }
        self.confirm_delete = Some(shots);
    }

    /// The "Are you sure?" dialog for deleting photos.
    fn delete_dialog(&mut self, ctx: &egui::Context) {
        let Some(shots) = self.confirm_delete.clone() else { return };
        let p = self.p;
        let images: Vec<&Shot> = shots.iter().map(|&s| &self.shots[s]).filter(|s| s.image.is_some()).collect();
        let jpgs = images.len();
        let raws = shots.iter().filter(|&&s| self.shots[s].raw.is_some()).count();
        let n = shots.len();
        let permanent = self.folder.as_deref().is_some_and(export::has_no_recycle_bin);
        // "JPG"/"PNG"/… when they're all one type, else "image".
        let kind = match images.first().map(|s| s.image_kind()) {
            Some(k) if images.iter().all(|s| s.image_kind() == k) => k,
            _ => "image".to_string(),
        };
        let kinds = |count: usize| if count == 1 { kind.clone() } else { format!("{kind}s") };

        let title = if n == 1 { "Delete this photo?".to_string() } else { format!("Delete {n} photos?") };
        let what = match (jpgs, raws) {
            (0, r) => format!("This deletes {r} RAW {}.", plural(r, "file", "files")),
            (j, 0) if images.iter().all(|s| s.is_camera_image()) => {
                format!("This deletes {j} {} (no RAW files).", kinds(j))
            }
            (j, 0) => format!("This deletes {j} {}.", kinds(j)),
            (j, r) if j == r => format!(
                "This deletes {j} {} and {} {r} RAW {} ({} files).",
                kinds(j),
                if r == 1 { "its" } else { "their" },
                plural(r, "file", "files"),
                j + r
            ),
            (j, r) => format!(
                "This deletes {j} {} and {r} RAW {} ({} files).",
                kinds(j),
                plural(r, "file", "files"),
                j + r
            ),
        };
        let where_to = if permanent {
            format!("Memory cards and network drives have no {}: the files will be deleted permanently. This can't be undone.", platform::TRASH)
        } else {
            format!("They'll go to the {}, where you can restore them.", platform::TRASH)
        };

        let mut confirmed = false;
        let mut cancelled = false;
        let modal = egui::Modal::new(Id::new("confirm-delete"))
            .frame(Frame::window(&ctx.style()).fill(p.card).corner_radius(p.r_card).inner_margin(Margin::same(20)))
            .show(ctx, |ui| {
                ui.set_width(400.0);
                ui.label(RichText::new(title).font(theme::font_semibold(17.0)));
                ui.add_space(6.0);
                ui.label(RichText::new(what).color(p.label));
                ui.add_space(4.0);
                ui.label(RichText::new(where_to).color(if permanent { p.danger } else { p.label2 }));
                if shots.iter().any(|s| self.picked.contains(s)) {
                    ui.add_space(4.0);
                    ui.label(RichText::new("Copies already in your export folder are kept.").color(p.label2));
                }
                ui.add_space(16.0);
                ui.with_layout(Layout::right_to_left(Align::Center), |ui| {
                    let delete = Button::new(RichText::new("Delete").color(Color32::WHITE).font(theme::font_semibold(13.5)))
                        .fill(theme::DELETE_RED)
                        .stroke(Stroke::NONE)
                        .corner_radius(p.r_ctl)
                        .min_size(vec2(96.0, 32.0));
                    if ui.add(delete).clicked() {
                        confirmed = true;
                    }
                    let cancel = Button::new("Cancel").corner_radius(p.r_ctl).min_size(vec2(96.0, 32.0));
                    if ui.add(cancel).clicked() {
                        cancelled = true;
                    }
                });
            });
        if confirmed {
            self.confirm_delete = None;
            self.delete_shots(ctx, &shots, permanent);
        } else if cancelled || modal.should_close() {
            self.confirm_delete = None;
        }
    }

    /// While the Keybinds page waits for a new key: the next key pressed becomes the
    /// shortcut (Esc cancels), and no key reaches anything else. Returns whether it's
    /// waiting.
    fn capture_keybind(&mut self, ctx: &egui::Context) -> bool {
        if self.settings_page != Some(SettingsPage::Keybinds) {
            self.rebinding = None;
        }
        let Some(action) = self.rebinding else { return false };
        let press = ctx.input(|i| {
            i.events.iter().find_map(|e| match e {
                egui::Event::Key { key, pressed: true, repeat: false, modifiers, .. } => Some((*key, *modifiers)),
                _ => None,
            })
        });
        ctx.input_mut(|i| i.events.retain(|e| !matches!(e, egui::Event::Key { .. })));
        if let Some((key, modifiers)) = press {
            self.rebinding = None;
            if key != KeyCode::Escape {
                self.settings.keybinds.assign(action, keybinds::Shortcut::from_press(key, modifiers));
                self.settings.save();
            }
        }
        true
    }

    /// The popup from the "!" button: says there's an update and offers its download page.
    fn update_dialog(&mut self, ctx: &egui::Context) {
        if !self.update_prompt {
            return;
        }
        let Some(version) = self.new_version.lock().unwrap().clone() else {
            self.update_prompt = false;
            return;
        };
        let p = self.p;
        let (mut yes, mut no) = (false, false);
        let modal = egui::Modal::new(Id::new("update-available"))
            .frame(Frame::window(&ctx.style()).fill(p.card).corner_radius(p.r_card).inner_margin(Margin::same(20)))
            .show(ctx, |ui| {
                ui.set_width(400.0);
                ui.label(RichText::new("Update available").font(theme::font_semibold(17.0)));
                ui.add_space(6.0);
                ui.label(RichText::new(format!("Cullery {version} is out. You have version {}.", update::CURRENT_VERSION)).color(p.label));
                ui.add_space(4.0);
                ui.label(RichText::new("Open the download page on GitHub?").color(p.label2));
                ui.add_space(16.0);
                ui.with_layout(Layout::right_to_left(Align::Center), |ui| {
                    let open = Button::new(RichText::new("Yes").color(p.on_accent).font(theme::font_semibold(13.5)))
                        .fill(p.accent)
                        .stroke(Stroke::NONE)
                        .corner_radius(p.r_ctl)
                        .min_size(vec2(96.0, 32.0));
                    yes = ui.add(open).clicked();
                    no = ui.add(Button::new("No").corner_radius(p.r_ctl).min_size(vec2(96.0, 32.0))).clicked();
                });
            });
        if yes {
            crate::window::open_link(update::DOWNLOAD_PAGE);
        }
        if yes || no || modal.should_close() {
            self.update_prompt = false;
        }
    }

    /// A small card in the top-right corner while photos copy: where they're going, a
    /// bar, and how many files are done.
    fn export_progress(&mut self, ctx: &egui::Context) {
        let Some((done, total, dest)) = self.exporter.progress() else { return };
        let p = self.p;
        let title = format!("Exporting to {}", folder_name(dest));
        let mut keep_open = false;
        let anchor = pos2(self.content_rect.max.x - 16.0, self.content_rect.min.y + 12.0);
        egui::Area::new(Id::new("export-progress"))
            .order(Order::Foreground)
            .pivot(Align2::RIGHT_TOP)
            .fixed_pos(anchor)
            .interactable(true)
            .show(ctx, |ui| {
                Frame::window(&ctx.style()).fill(p.card).corner_radius(p.r_card).inner_margin(Margin::same(14)).show(ui, |ui| {
                    const W: f32 = 260.0;
                    ui.set_width(W);
                    ui.add(egui::Label::new(RichText::new(title).font(theme::font_semibold(13.5)).color(p.label)).truncate());
                    ui.add_space(8.0);
                    let fraction = done as f32 / total.max(1) as f32;
                    ui.add(egui::ProgressBar::new(fraction).desired_width(W).desired_height(6.0).fill(p.accent).corner_radius(p.r_pill));
                    ui.add_space(6.0);
                    ui.horizontal(|ui| {
                        ui.label(RichText::new(format!("{done} of {total} {}", plural(total, "file", "files"))).color(p.label2).size(12.5));
                        if self.close_when_done {
                            ui.with_layout(Layout::right_to_left(Align::Center), |ui| {
                                let r = ui.add(egui::Label::new(RichText::new("Closes when done · Keep open").color(p.tint).size(12.5)).sense(Sense::click()));
                                keep_open = r.on_hover_cursor(egui::CursorIcon::PointingHand).clicked();
                            });
                        }
                    });
                });
            });
        if keep_open {
            self.close_when_done = false;
        }
    }

    /// Holds the window open while photos are copying, and asks first. Closes it once
    /// they're done, if the user chose Close When Done.
    fn guard_close(&mut self, ctx: &egui::Context) {
        let copying = self.exporter.pending() > 0;
        if ctx.input(|i| i.viewport().close_requested()) && copying && !self.closing {
            ctx.send_viewport_cmd(ViewportCommand::CancelClose);
            if !self.close_when_done {
                self.confirm_close = true;
            }
        }
        if self.close_when_done && !copying {
            self.close_when_done = false;
            self.closing = true;
            ctx.send_viewport_cmd(ViewportCommand::Close);
        }
    }

    /// "Photos are still exporting": wait and close, stop and close, or keep going.
    fn close_dialog(&mut self, ctx: &egui::Context) {
        if !self.confirm_close {
            return;
        }
        let Some((done, total, dest)) = self.exporter.progress() else {
            // Finished while the question was up: nothing left to lose.
            self.confirm_close = false;
            self.closing = true;
            ctx.send_viewport_cmd(ViewportCommand::Close);
            return;
        };
        let p = self.p;
        let left = total - done;
        let (mut wait, mut stop, mut cancel) = (false, false, false);
        let modal = egui::Modal::new(Id::new("confirm-close"))
            .frame(Frame::window(&ctx.style()).fill(p.card).corner_radius(p.r_card).inner_margin(Margin::same(20)))
            .show(ctx, |ui| {
                ui.set_width(440.0);
                ui.label(RichText::new("Photos are still exporting").font(theme::font_semibold(17.0)));
                ui.add_space(6.0);
                ui.label(
                    RichText::new(format!(
                        "{left} of {total} {} still to copy to {}.",
                        plural(total, "file", "files"),
                        folder_name(dest)
                    ))
                    .color(p.label),
                );
                ui.add_space(4.0);
                ui.label(RichText::new("Stopping now leaves those files uncopied. Files already copied are kept.").color(p.label2));
                ui.add_space(16.0);
                ui.with_layout(Layout::right_to_left(Align::Center), |ui| {
                    let primary = Button::new(RichText::new("Close When Done").color(p.on_accent).font(theme::font_semibold(13.5)))
                        .fill(p.accent)
                        .stroke(Stroke::NONE)
                        .corner_radius(p.r_ctl)
                        .min_size(vec2(130.0, 32.0));
                    wait = ui.add(primary).clicked();
                    stop = ui.add(Button::new("Stop and Close").corner_radius(p.r_ctl).min_size(vec2(110.0, 32.0))).clicked();
                    cancel = ui.add(Button::new("Keep Open").corner_radius(p.r_ctl).min_size(vec2(96.0, 32.0))).clicked();
                });
            });
        if wait {
            self.close_when_done = true;
        }
        if stop {
            // The file being copied finishes (no half files); the rest are skipped.
            self.exporter.stop();
            self.closing = true;
            ctx.send_viewport_cmd(ViewportCommand::Close);
        }
        if wait || stop || cancel || modal.should_close() {
            self.confirm_close = false;
        }
    }

    /// Starts deleting these shots' files in the background. `finish_delete` takes over
    /// when they're gone.
    fn delete_shots(&mut self, ctx: &egui::Context, shots: &[usize], permanent: bool) {
        // A playing clip holds its file open, which would stop it being deleted.
        self.playback = None;
        let files: Vec<PathBuf> = shots.iter().flat_map(|&s| self.shots[s].files().cloned()).collect();
        let job = export::Deletion::start(files, permanent, ctx.clone());
        self.deleting = Some(Deleting { job, shots: shots.to_vec(), permanent, started: ctx.input(|i| i.time) });
    }

    /// Once a delete has ended: refreshes the folder without losing the view.
    fn finish_delete(&mut self, ctx: &egui::Context) {
        let Some(result) = self.deleting.as_ref().and_then(|d| d.job.try_finish()) else { return };
        let Some(Deleting { shots, permanent, .. }) = self.deleting.take() else { return };
        let shots = &shots[..];

        // In the viewer, move on to the next photo that's staying (or the one before).
        if self.screen == Screen::Viewer {
            let set = self.working_set();
            let pos = set.iter().position(|&s| s == self.current).unwrap_or(0);
            let staying = |s: &usize| !shots.contains(s);
            if let Some(&next) = set[pos..].iter().find(|s| staying(s)).or_else(|| set[..pos].iter().rev().find(|s| staying(s))) {
                self.current = next;
            }
        }
        self.refresh_folder();
        if self.screen == Screen::Viewer && self.working_set().is_empty() {
            self.back_to_grid();
        }

        match result {
            Ok(()) => {
                let n = shots.len();
                let verb = if permanent { "Deleted".to_string() } else { format!("Moved to the {}:", platform::TRASH) };
                self.show_toast(ctx, format!("{verb} {n} {}", plural(n, "photo", "photos")));
            }
            Err(e) => self.error(e),
        }
    }

    /// A progress bar over the window while a delete runs. It also holds the mouse off
    /// everything else, since photo numbers change when the delete ends.
    fn delete_progress(&self, ctx: &egui::Context) {
        let Some(d) = &self.deleting else { return };
        let p = self.p;
        let (done, total) = d.job.progress();
        let n = d.shots.len();
        // Small deletes are over before a bar would be readable; don't flash one.
        let show = ctx.input(|i| i.time) - d.started >= DELETE_BAR_DELAY;
        let frame = if show {
            Frame::window(&ctx.style()).fill(p.card).corner_radius(p.r_card).inner_margin(Margin::same(20))
        } else {
            Frame::new()
        };
        let mut modal = egui::Modal::new(Id::new("deleting")).frame(frame);
        if !show {
            modal = modal.backdrop_color(Color32::TRANSPARENT);
        }
        modal.show(ctx, |ui| {
            if !show {
                return;
            }
            ui.set_width(400.0);
            ui.label(RichText::new(format!("Deleting {n} {}…", plural(n, "photo", "photos"))).font(theme::font_semibold(17.0)));
            ui.add_space(12.0);
            ui.add(egui::ProgressBar::new(done as f32 / total.max(1) as f32).desired_width(400.0).fill(p.accent).corner_radius(p.r_pill));
            ui.add_space(8.0);
            let place = if d.permanent { "Deleting permanently".to_string() } else { format!("Moving to the {}", platform::TRASH) };
            ui.label(RichText::new(format!("{place}: {done} of {total} files")).color(p.label2));
        });
        ctx.request_repaint_after(std::time::Duration::from_millis(50));
    }

    /// Rescans the open folder after files changed, keeping thumbnails, picks,
    /// selection, the viewed photo and the range (all matched up by shot name).
    fn refresh_folder(&mut self) {
        let Some(dir) = self.folder.clone() else { return };
        if library::is_drive_root(&dir) {
            self.start_drive_scan(dir, None, true);
            return;
        }
        let Ok(new_shots) = library::scan(&dir) else { return };
        self.adopt_rescan(new_shots);
    }

    /// Takes in a fresh scan of the open folder: the kind on show replaces the current
    /// shots (keeping marks and thumbnails), the other kind is set aside.
    fn adopt_rescan(&mut self, mut shots: Vec<Shot>) {
        library::sort(&mut shots, self.settings.sort_by, self.settings.sort_descending);
        let (photos, videos) = library::split_videos(shots);
        let (current, other) = match self.media {
            Media::Photos => (photos, videos),
            Media::Videos => (videos, photos),
        };
        self.other_shots = other;
        self.adopt_shots(current);
    }

    /// Flips the grid between photos and videos.
    fn switch_media(&mut self, media: Media) {
        if self.media == media {
            return;
        }
        self.media = media;
        std::mem::swap(&mut self.shots, &mut self.other_shots);
        self.generation = self.loader.reset();
        self.thumbs.clear();
        self.fulls.clear();
        self.original = None;
        self.failed.clear();
        self.has_raw_pairs = has_raw_pairs(&self.shots);
        self.reset_view();
        // The other kind's picks were kept aside; this brings this kind's back.
        self.reload_picks();
    }

    /// Switches to a new list of the folder's shots, matching everything that refers
    /// to a shot's position up with its new one.
    fn adopt_shots(&mut self, new_shots: Vec<Shot>) {
        let index: HashMap<String, usize> =
            new_shots.iter().enumerate().map(|(i, s)| (s.key.clone(), i)).collect();
        self.has_raw_pairs = has_raw_pairs(&new_shots);
        let old = std::mem::replace(&mut self.shots, new_shots);
        let remap = |i: usize| old.get(i).and_then(|s| index.get(&s.key)).copied();

        self.thumbs = std::mem::take(&mut self.thumbs).into_iter().filter_map(|(i, t)| Some((remap(i)?, t))).collect();
        self.picked = self.picked.iter().filter_map(|&i| remap(i)).collect();
        self.rejected = self.rejected.iter().filter_map(|&i| remap(i)).collect();
        self.selected = self.selected.iter().filter_map(|&i| remap(i)).collect();
        self.select_anchor = self.select_anchor.and_then(remap);
        // The viewed photo: itself, or the nearest remaining one.
        self.current = remap(self.current)
            .or_else(|| (self.current..old.len()).find_map(remap))
            .or_else(|| (0..self.current).rev().find_map(remap))
            .unwrap_or(0);
        self.range = self.range.and_then(|(from, to)| {
            let kept: Vec<usize> = (from..=to).filter_map(remap).collect();
            Some((*kept.first()?, *kept.last()?))
        });
        if let RangeMode::PickEnd(s) = self.range_mode {
            self.range_mode = remap(s).map_or(RangeMode::PickStart, RangeMode::PickEnd);
        }

        // Anything loading was for the old numbering.
        self.generation = self.loader.reset();
        self.fulls.clear();
        self.original = None;
        self.failed.clear();
        self.last_tile_click = None;
        self.reset_zoom();
        self.save_picks();
        if self.shots.is_empty() {
            self.message = Some(("No photos left in this folder".into(), false));
        }
    }

    fn set_view_picked(&mut self, on: bool) {
        self.view_picked = on;
        if on {
            self.view_rejected = false;
            self.range_mode = RangeMode::Off;
            // Keep only selections that are still visible.
            let picked = &self.picked;
            self.selected.retain(|s| picked.contains(s));
        }
    }

    fn picked_badge(&self, ui: &mut Ui) {
        let p = self.p;
        let files: usize = self.picked.iter().map(|&s| self.shots[s].files().count()).sum();
        let text = format!("✔  {} Picked · {} files", self.picked.len(), files);
        Frame::new()
            .fill(p.tint_bg)
            .corner_radius(p.r_pill)
            .inner_margin(Margin::symmetric(12, 6))
            .show(ui, |ui| ui.label(RichText::new(text).color(p.tint).font(theme::font_semibold(13.5))));
    }

    fn message_label(&mut self, ui: &mut Ui) {
        let p = self.p;
        if let Some((text, is_error)) = &self.message {
            let color = if *is_error { p.danger } else { p.label2 };
            let r = ui.add(egui::Label::new(RichText::new(text).color(color)).sense(Sense::click()).truncate());
            if r.on_hover_text("Click to dismiss").clicked() {
                self.message = None;
            }
        }
    }

    fn viewer_controls(&mut self, ui: &mut Ui) {
        let p = self.p;
        let bar = ui.max_rect();
        self.quick_export_button(ui, true);
        ui.add_space(TRASH_GAP - ui.spacing().item_spacing.x);
        if trash_button(ui, &p, true).on_hover_text("Delete this photo and its RAW file (Delete)").clicked() {
            self.request_delete();
        }

        // Zoom indicator, then prev / pick / next, with the pick button centered in the window.
        const ZOOM_W: f32 = 64.0;
        let gap = ui.spacing().item_spacing.x;
        let left_edge = bar.center().x - PICK_W / 2.0 - gap - 44.0 - gap - ZOOM_W;
        let pad = left_edge - ui.cursor().min.x;
        if pad > 0.0 {
            ui.add_space(pad);
        }
        let zoom_text = match self.zoom_percent {
            Some(pct) => format!("{pct:.0}%"),
            None => "Fit".to_string(),
        };
        let zoom_btn = Button::new(RichText::new(zoom_text).color(p.label2))
            .fill(Color32::TRANSPARENT)
            .stroke(Stroke::NONE)
            .min_size(vec2(ZOOM_W, 32.0));
        let zoom_tip = "Zoom: scroll wheel or double-click the photo. Click to switch between Fit and 100%";
        if ui.add(zoom_btn).on_hover_text(zoom_tip).clicked() {
            match (self.zoom, self.shown_original) {
                (Some(_), _) => self.reset_zoom(),
                (None, Some(orig)) => {
                    self.zoom = Some(1.0 / ui.ctx().pixels_per_point());
                    self.zoom_center = orig / 2.0;
                }
                _ => {}
            }
        }

        if chevron_button(ui, &p, true).on_hover_text("Previous (←)").clicked() {
            self.step(-1);
        }
        let picked = self.picked.contains(&self.current);
        let label = if picked { "Picked" } else { "Pick" };
        if pick_button(ui, &p, label, picked, true).on_hover_text(self.pick_tip()).clicked() {
            self.toggle_pick(self.current);
        }
        if chevron_button(ui, &p, false).on_hover_text("Next (→)").clicked() {
            self.step(1);
        }

        ui.with_layout(Layout::right_to_left(Align::Center), |ui| {
            if !self.shots.is_empty() {
                let text = format!("{} · {} Picked", self.position_label(), self.picked.len());
                ui.label(RichText::new(text).color(p.label2));
            }
            self.message_label(ui);
        });
    }

    // ---------------------------------------------------------------- screens

    fn empty_screen(&mut self, ui: &mut Ui, ctx: &egui::Context) {
        let p = self.p;
        let cards = self.cards.lock().unwrap().clone();
        let content_h = 230.0 + if cards.is_empty() { 0.0 } else { 30.0 + 56.0 * cards.len() as f32 }
            + if self.settings.recent.is_empty() { 0.0 } else { 30.0 + 44.0 * self.settings.recent.len().min(5) as f32 };
        let mut open = None;
        ScrollArea::vertical().auto_shrink(false).show(ui, |ui| {
            ui.add_space(((ui.available_height() - content_h) / 2.0).max(16.0));
            ui.vertical_centered(|ui| {
                ui.set_max_width(480.0);
                ui.label(RichText::new("🗁").size(56.0).color(p.label2));
                ui.label(RichText::new("No Folder Open").font(theme::font_semibold(22.0)));
                ui.label(RichText::new("Drop a folder or SD card here, or open one to start culling.").color(p.label2).size(14.0));
                ui.add_space(8.0);
                let open_btn = Button::new(RichText::new(format!("Open Folder…    {}", self.settings.keybinds.hint(KeyAction::OpenFolder))).color(p.on_accent).font(theme::font_semibold(14.0)))
                    .fill(p.accent)
                    .stroke(Stroke::NONE)
                    .corner_radius(p.r_pill)
                    .min_size(vec2(0.0, 36.0));
                if ui.add(open_btn).clicked() {
                    self.choose_folder(ctx);
                }

                if !cards.is_empty() {
                    ui.add_space(24.0);
                    section_label(ui, &p, "Drives");
                    for card in &cards {
                        let heics = if card.heics > 0 { format!("{} HEIC · ", card.heics) } else { String::new() };
                        let sub = if card.jpgs + card.heics + card.raws == 0 {
                            "All photos, including subfolders".to_string()
                        } else {
                            format!("{} JPG · {heics}{} RAW", card.jpgs, card.raws)
                        };
                        if list_row(ui, &p, &card.path.display().to_string(), Some(&sub), None).clicked() {
                            open = Some(card.path.clone());
                        }
                    }
                }
                if !self.settings.recent.is_empty() {
                    ui.add_space(20.0);
                    section_label(ui, &p, "Recent");
                    for dir in self.settings.recent.iter().take(5) {
                        if list_row(ui, &p, &dir.display().to_string(), None, Some("›")).clicked() {
                            open = Some(dir.clone());
                        }
                    }
                }
            });
        });
        if let Some(dir) = open {
            self.open_folder(ctx, dir);
        }
    }

    fn grid_screen(&mut self, ui: &mut Ui) {
        // The grid gets everything except a strip on the right for its scroll bar,
        // so the bar never covers photos.
        let full = ui.available_rect_before_wrap();
        let gutter = Rect::from_min_max(pos2(full.max.x - SCROLLBAR_W, full.min.y), full.max);
        let grid_rect = Rect::from_min_max(full.min, pos2(gutter.min.x - 6.0, full.max.y));
        let mut grid_ui = ui.new_child(UiBuilder::new().max_rect(grid_rect).layout(Layout::top_down(Align::Min)));
        let scroll = self.grid_contents(&mut grid_ui);
        ui.advance_cursor_after_rect(full);

        if let Some((view, offset, content_h, step)) = scroll {
            let bar = Rect::from_min_max(pos2(gutter.min.x, view.min.y), pos2(gutter.max.x, view.max.y));
            if let Some(new) = scrollbar(ui, &self.p, bar, offset, content_h, view.height(), step) {
                self.pending_scroll = Some(new);
                ui.ctx().request_repaint();
            }
        }
    }

    /// Returns the scroll view's rect, offset, content height and row height.
    fn grid_contents(&mut self, ui: &mut Ui) -> Option<(Rect, f32, f32, f32)> {
        let p = self.p;
        if self.view_picked || self.view_rejected || self.range_mode != RangeMode::Off || self.range.is_some() {
            self.range_banner(ui);
            ui.add_space(10.0);
        }

        let items = self.items();
        let avail_w = ui.available_width();
        let cols = (((avail_w + GAP_X) / (self.settings.tile_width + GAP_X)).floor() as usize).max(1);
        let tile_w = (avail_w - GAP_X * (cols - 1) as f32) / cols as f32;
        let thumb_h = (tile_w * 2.0 / 3.0).round();
        let row_h = thumb_h + NAME_AREA;
        let row_pitch = row_h + ROW_GAP;
        let n_rows = items.len().div_ceil(cols);
        let viewport_top = ui.cursor().top();

        // Decide where to scroll: back to the photo just viewed, or keep the photo in the
        // middle of the grid in place after a layout change (RAW toggle, range, tile size).
        let layout = LayoutKey {
            show_raw: self.settings.show_raw,
            range_mode: self.range_mode != RangeMode::Off,
            view_picked: self.view_picked,
            view_rejected: self.view_rejected,
            range: self.range,
            tile_width: self.settings.tile_width.round() as i32,
        };
        let layout_changed = self.last_layout.is_some_and(|l| l != layout);
        self.last_layout = Some(layout);

        let mut scroll = ScrollArea::vertical()
            .auto_shrink(false)
            .scroll_bar_visibility(ScrollBarVisibility::AlwaysHidden);
        if let Some(offset) = self.pending_scroll.take() {
            scroll = scroll.vertical_scroll_offset(offset);
        } else if std::mem::take(&mut self.scroll_to_current) {
            if let Some(i) = items.iter().position(|it| it.shot == self.current) {
                let offset = (i / cols) as f32 * row_pitch - (ui.available_height() - row_h) / 2.0;
                scroll = scroll.vertical_scroll_offset(offset.max(0.0));
            }
        } else if layout_changed {
            if let Some(a) = self.view_anchor {
                let i = items
                    .iter()
                    .position(|it| it.shot == a.shot && it.raw == a.raw)
                    .or_else(|| items.iter().position(|it| it.shot == a.shot));
                if let Some(i) = i {
                    let offset = (i / cols) as f32 * row_pitch - (a.screen_y - viewport_top);
                    scroll = scroll.vertical_scroll_offset(offset.max(0.0));
                }
            }
        }

        let mods = ui.input(|i| i.modifiers);
        let mut actions = Vec::new();
        let mut visible_rows = 0..0;
        let mut drawn: Vec<(Item, Rect)> = Vec::new();
        ui.spacing_mut().item_spacing.y = ROW_GAP;
        let output = scroll.show_rows(ui, row_h, n_rows, |ui, rows| {
            visible_rows = rows.clone();
            for row in rows {
                ui.horizontal(|ui| {
                    ui.spacing_mut().item_spacing.x = GAP_X;
                    for item in items.iter().skip(row * cols).take(cols) {
                        let (action, rect) = self.tile(ui, *item, tile_w, thumb_h);
                        if let Some(a) = action {
                            actions.push(a);
                        }
                        drawn.push((*item, rect));
                    }
                });
            }
        });

        // Remember the tile nearest the middle of the view.
        let view = output.inner_rect;
        self.view_anchor = drawn
            .iter()
            .filter(|(_, r)| r.intersects(view))
            .min_by(|(_, a), (_, b)| {
                let da = a.center().distance_sq(view.center());
                let db = b.center().distance_sq(view.center());
                da.total_cmp(&db)
            })
            .map(|(it, r)| ViewAnchor { shot: it.shot, raw: it.raw, screen_y: r.top() });

        // Clicking empty space between photos clears the selection, like Explorer.
        let background_click = ui.input(|i| {
            i.pointer.primary_clicked()
                && i.pointer.interact_pos().is_some_and(|pos| {
                    view.contains(pos) && !drawn.iter().any(|(_, r)| r.contains(pos))
                })
        }) && {
            // Not when the click landed on something floating over the grid (Settings, a menu).
            let top = ui.input(|i| i.pointer.interact_pos()).and_then(|pos| ui.ctx().layer_id_at(pos));
            top.map_or(true, |layer| layer == ui.layer_id())
        };
        if background_click && actions.is_empty() && self.range_mode == RangeMode::Off {
            self.selected.clear();
        }
        self.drawn_tiles = drawn;

        // Ask for thumbnails: visible rows first, then a few rows either side.
        let shots_in = |rows: std::ops::Range<usize>| -> Vec<usize> {
            let mut v: Vec<usize> = items
                .iter()
                .skip(rows.start * cols)
                .take(rows.len() * cols)
                .map(|it| it.shot)
                .collect();
            v.dedup();
            v
        };
        let visible = shots_in(visible_rows.clone());
        if let Some(&first) = visible.first() {
            self.grid_anchor = first;
        }
        let mut ahead = shots_in(visible_rows.end..(visible_rows.end + LOOKAHEAD_ROWS).min(n_rows));
        ahead.extend(shots_in(visible_rows.start.saturating_sub(LOOKAHEAD_ROWS)..visible_rows.start));
        self.request_thumbs(&visible, &ahead);

        if items.is_empty() && !self.shots.is_empty() {
            ui.label(RichText::new("Nothing to show").color(p.label2));
        }

        for action in actions {
            match action {
                Action::Click(shot) => {
                    // Second plain click on the same photo soon after the first: open it.
                    let now = ui.input(|i| i.time);
                    let double = mods.is_none()
                        && self.last_tile_click.is_some_and(|(s, t)| s == shot && now - t <= self.double_click_time);
                    if double {
                        self.last_tile_click = None;
                        self.open_viewer(shot);
                    } else {
                        self.click_select(shot, &items, mods);
                        self.last_tile_click = mods.is_none().then_some((shot, now));
                    }
                }
                Action::DragOut(shot) => self.drag_out(shot, &items),
                Action::TogglePick(shot) => self.toggle_pick(shot),
                Action::Mark(shot) => match self.range_mode {
                    RangeMode::PickEnd(start) => self.finish_range(start, shot),
                    _ => self.range_mode = RangeMode::PickEnd(shot),
                },
            }
        }
        Some((view, output.state.offset.y, output.content_size.y, row_pitch))
    }

    /// The range's two ends are chosen: show just those. Photos outside it leave the
    /// selection, so nothing off-screen gets picked or deleted.
    fn finish_range(&mut self, a: usize, b: usize) {
        let (from, to) = (a.min(b), a.max(b));
        self.range = Some((from, to));
        self.range_mode = RangeMode::Off;
        self.selected.retain(|s| (from..=to).contains(s));
    }

    fn range_banner(&mut self, ui: &mut Ui) {
        let p = self.p;
        let (text, button) = match (self.range_mode, self.range) {
            _ if self.view_rejected => (
                if self.rejected.is_empty() {
                    "No rejected photos.".to_string()
                } else {
                    format!(
                        "Showing your {} rejected photos  ·  to delete them all, select all ({}) and press Delete",
                        self.rejected.len(),
                        self.settings.keybinds.main_text(KeyAction::SelectAll)
                    )
                },
                "Show All",
            ),
            _ if self.view_picked => (
                if self.picked.is_empty() {
                    "No picked photos yet.".to_string()
                } else {
                    format!(
                        "Showing your {} picked photos  ·  drag a selection into Lightroom, LightCraft or a folder",
                        self.picked.len()
                    )
                },
                "Show All",
            ),
            (RangeMode::PickStart, _) => ("Click the first shot of this set.".to_string(), "Cancel"),
            (RangeMode::PickEnd(_), _) => ("Now click the last shot. Everything between becomes your working set.".to_string(), "Cancel"),
            (RangeMode::Off, Some((from, to))) => (
                format!(
                    "Showing {} – {}  ·  {} of {} shots",
                    self.shots[from].name,
                    self.shots[to].name,
                    to - from + 1,
                    self.shots.len()
                ),
                "Show All",
            ),
            _ => return,
        };
        Frame::new()
            .fill(p.card)
            .corner_radius(p.r_card)
            .inner_margin(Margin { left: 14, right: 6, top: 6, bottom: 6 })
            .show(ui, |ui| {
                ui.horizontal(|ui| {
                    ui.label(text);
                    ui.with_layout(Layout::right_to_left(Align::Center), |ui| {
                        let b = Button::new(RichText::new(button).color(p.tint).font(theme::font_semibold(13.5)))
                            .fill(Color32::TRANSPARENT)
                            .stroke(Stroke::NONE);
                        if ui.add(b).clicked() {
                            if self.view_picked || self.view_rejected {
                                self.view_picked = false;
                                self.view_rejected = false;
                            } else if self.range_mode != RangeMode::Off {
                                self.range_mode = RangeMode::Off;
                            } else {
                                self.range = None;
                            }
                        }
                    });
                });
            });
    }

    fn tile(&mut self, ui: &mut Ui, item: Item, w: f32, thumb_h: f32) -> (Option<Action>, Rect) {
        let p = self.p;
        // Tiles also sense drags, to drag photos out into other apps (not while picking a range).
        let sense = if self.range_mode == RangeMode::Off { Sense::click_and_drag() } else { Sense::click() };
        let (rect, resp) = ui.allocate_exact_size(vec2(w, thumb_h + NAME_AREA), sense);
        if !ui.is_rect_visible(rect) {
            return (None, rect);
        }
        let selecting = self.range_mode != RangeMode::Off;
        let thumb_rect = Rect::from_min_size(rect.min, vec2(w, thumb_h));
        let shot = &self.shots[item.shot];
        let picked = self.picked.contains(&item.shot) && !selecting;
        let selected = self.selected.contains(&item.shot) && !selecting;
        let is_start = self.range_mode == RangeMode::PickEnd(item.shot);

        // Pick circle (top-right), registered after the tile so it takes the click.
        let circle_rect = Rect::from_min_size(pos2(thumb_rect.max.x - 44.0, thumb_rect.min.y), Vec2::splat(44.0));
        let tip = if picked { "Unpick (any copies stay in the export folder)" } else { self.pick_tip() };
        let circle = (!selecting).then(|| ui.interact(circle_rect, ui.id().with(("pick", item.shot, item.raw)), Sense::click()).on_hover_text(tip));

        let painter = ui.painter();
        // Selection: a soft box around the whole tile, like Finder / Explorer.
        if selected {
            let sel = Rect::from_min_max(thumb_rect.min - vec2(6.0, 6.0), pos2(thumb_rect.max.x + 6.0, rect.max.y + 2.0));
            let r = theme::around(p.r_thumb, 3);
            painter.rect_filled(sel, r, p.select_bg);
            painter.rect_stroke(sel, r, Stroke::new(1.0_f32, p.select_border), StrokeKind::Inside);
        }
        painter.rect_filled(thumb_rect, p.r_thumb, p.tile);
        if let Some(t) = self.thumbs.get_mut(&item.shot) {
            t.last_used = self.frame;
            let fit = fit_rect(thumb_rect, t.tex.size_vec2());
            Image::new((t.tex.id(), fit.size())).corner_radius(p.r_thumb.saturating_sub(2)).paint_at(ui, fit);
        } else if let Some(reason) = self.failed.get(&Key { shot: item.shot, kind: Kind::Thumb }) {
            let help = shot.display_path().and_then(|path| codec_help(path, reason));
            let text = help.map_or("No preview", |h| h.short);
            painter.text(thumb_rect.center(), Align2::CENTER_CENTER, text, theme::font(12.0), p.label2);
        }
        if shot.video {
            // A small play mark in the corner, so clips read as clips.
            let badge = Rect::from_min_size(thumb_rect.left_bottom() + vec2(8.0, -26.0), vec2(28.0, 18.0));
            painter.rect_filled(badge, 4, Color32::from_black_alpha(170));
            play_triangle(painter, badge.center(), 5.0, Color32::WHITE);
        }
        if self.rejected.contains(&item.shot) && !selecting {
            // Rejected: dimmed, with a red ✕.
            painter.rect_filled(thumb_rect, p.r_thumb, Color32::from_black_alpha(140));
            let c = thumb_rect.right_bottom() + vec2(-16.0, -16.0);
            painter.circle_filled(c, 10.0, theme::DELETE_RED);
            x_mark(painter, c, 4.0, Color32::WHITE);
        }
        let hovered = resp.hovered() || circle.as_ref().is_some_and(|c| c.hovered());
        if resp.hovered() {
            painter.rect_filled(thumb_rect, p.r_thumb, Color32::from_white_alpha(12));
        }
        if picked || is_start {
            painter.rect_stroke(thumb_rect, theme::around(p.r_thumb, 1), Stroke::new(3.0_f32, p.accent), StrokeKind::Outside);
        }

        // File type badge (top-left).
        let (badge, fg, bg) = if item.raw {
            ("RAW".to_string(), Color32::from_rgb(0x1D, 0x1D, 0x1F), Color32::from_white_alpha(235))
        } else {
            (shot.image_kind(), Color32::WHITE, Color32::from_black_alpha(140))
        };
        let galley = painter.layout_no_wrap(badge, theme::font_semibold(10.0), fg);
        let badge_rect = Rect::from_min_size(thumb_rect.min + vec2(8.0, 8.0), galley.size() + vec2(10.0, 4.0));
        painter.rect_filled(badge_rect, p.r_ctl.min(4), bg);
        painter.galley(badge_rect.min + vec2(5.0, 2.0), galley, fg);

        if circle.is_some() && (picked || hovered) {
            let c = circle_rect.center();
            if picked {
                painter.circle_filled(c, 12.0, p.accent);
                painter.circle_stroke(c, 12.0, Stroke::new(2.0_f32, Color32::WHITE));
                check_mark(painter, c, 5.5, p.on_accent);
            } else {
                painter.circle_filled(c, 12.0, Color32::from_black_alpha(80));
                painter.circle_stroke(c, 12.0, Stroke::new(1.5_f32, Color32::from_white_alpha(230)));
            }
        }

        if selecting && hovered {
            let text = if is_start { "Start" } else if matches!(self.range_mode, RangeMode::PickStart) { "Set Start" } else { "Set End" };
            let mark_text = if is_start { p.on_accent } else { Color32::WHITE };
            let g = painter.layout_no_wrap(text.into(), theme::font_semibold(13.0), mark_text);
            let r = Rect::from_center_size(thumb_rect.center(), g.size() + vec2(24.0, 12.0));
            painter.rect_filled(r, p.r_pill.min(14), if is_start { p.accent } else { Color32::from_black_alpha(160) });
            painter.galley(r.min + vec2(12.0, 6.0), g, mark_text);
        }

        // File name, with a blue highlight when selected (like Finder).
        let name = if item.raw { shot.raw_name() } else { shot.image_name() };
        let name_color = if selected { p.on_accent } else { p.label };
        // Long names end in "…" at the tile's edge rather than running into the next tile.
        let mut name_job = LayoutJob::simple_singleline(name, theme::font(12.5), name_color);
        name_job.wrap = egui::text::TextWrapping::truncate_at_width(w - 12.0);
        let g = painter.layout_job(name_job);
        let name_rect = Rect::from_center_size(
            pos2(thumb_rect.center().x, thumb_rect.max.y + 16.0),
            g.size() + vec2(12.0, 2.0),
        );
        if selected {
            painter.rect_filled(name_rect, p.r_ctl.min(5), p.accent);
        }
        painter.galley(name_rect.min + vec2(6.0, 1.0), g, name_color);
        let sub = match (item.raw, shot.image.is_some(), shot.raw.is_some()) {
            (true, true, _) => format!("Paired with {}", shot.image_kind()),
            (true, false, _) => "RAW only".to_string(),
            (false, _, true) => format!("+ {}", shot.raw_kind()),
            (false, _, false) if shot.is_camera_image() => "No RAW".to_string(),
            // PNG, GIF, … never have a RAW; nothing to point out.
            (false, _, false) => String::new(),
        };
        painter.text(pos2(thumb_rect.center().x, name_rect.max.y + 8.0), Align2::CENTER_CENTER, sub, theme::font(11.0), p.label2);

        if circle.is_some_and(|c| c.clicked()) {
            return (Some(Action::TogglePick(item.shot)), rect);
        }
        let resp = resp.on_hover_cursor(if selecting { CursorIcon::Crosshair } else { CursorIcon::Default });
        // Double-clicks are spotted in grid_contents from two clicks in a row.
        let action = if selecting {
            resp.clicked().then_some(Action::Mark(item.shot))
        } else if resp.drag_started() {
            Some(Action::DragOut(item.shot))
        } else {
            resp.clicked().then_some(Action::Click(item.shot))
        };
        (action, rect)
    }

    fn viewer_screen(&mut self, ui: &mut Ui) {
        let p = self.p;
        let whole = ui.available_rect_before_wrap();
        let below_info = self.viewer_info(ui, whole);
        let area = self.viewer_strip(ui, below_info);
        let resp = ui.allocate_rect(area, Sense::click_and_drag());
        let area_clicked = resp.clicked();
        if self.draw_playback(ui, area, &resp) {
            return;
        }
        let shot = self.current;
        let ppp = ui.ctx().pixels_per_point();
        let max_zoom = MAX_ZOOM / ppp;

        let full = self.fulls.get(&shot);
        let original_size = full.map(|f| f.original);
        self.shown_original = original_size;

        match (full, original_size) {
            (Some(full), Some(orig)) => {
                let fit = (area.width() / orig.x).min(area.height() / orig.y);

                // Double-click: zoom to 100% at that spot, or back to fit. Spotted here from
                // two clicks in a row: egui's own detection counts a third click within
                // about a second as a triple-click, so a quick second zoom was ignored.
                let mut double = false;
                if resp.clicked() {
                    let now = ui.input(|i| i.time);
                    let pos = resp.interact_pointer_pos().unwrap_or(area.center());
                    double = self
                        .last_viewer_click
                        .is_some_and(|(t, p)| now - t <= self.double_click_time && p.distance(pos) <= 12.0);
                    self.last_viewer_click = if double { None } else { Some((now, pos)) };
                }
                if double {
                    if self.zoom.is_some() {
                        self.zoom = None;
                    } else if let Some(pos) = resp.interact_pointer_pos() {
                        let target = (1.0 / ppp).max(fit * 2.0).min(max_zoom);
                        self.zoom_center = orig / 2.0 + (pos - area.center()) / fit;
                        self.zoom = Some(target);
                    }
                }

                // Scroll wheel (or pinch / Ctrl+scroll): zoom around the cursor.
                if let Some(pos) = resp.hover_pos() {
                    let (scroll, pinch) = ui.input(|i| (i.raw_scroll_delta.y, i.zoom_delta()));
                    let factor = (scroll * 0.003).exp() * pinch;
                    if (factor - 1.0).abs() > 1e-4 {
                        let scale = self.zoom.unwrap_or(fit);
                        let center = if self.zoom.is_some() { self.zoom_center } else { orig / 2.0 };
                        let under_cursor = center + (pos - area.center()) / scale;
                        let new_scale = (scale * factor).clamp(fit, max_zoom.max(fit));
                        if new_scale <= fit * 1.001 {
                            self.zoom = None;
                        } else {
                            self.zoom = Some(new_scale);
                            self.zoom_center = under_cursor - (pos - area.center()) / new_scale;
                        }
                    }
                }

                // Drag to move around while zoomed.
                if self.zoom.is_some() && resp.dragged() {
                    self.zoom_center -= resp.drag_delta() / self.zoom.unwrap();
                }

                let scale = self.zoom.unwrap_or(fit);
                let center = match self.zoom {
                    Some(_) => {
                        // Keep the photo filling the view: no panning past its edges.
                        let half = area.size() / 2.0 / scale;
                        let clamp_axis = |c: f32, size: f32, half: f32| if size <= half * 2.0 { size / 2.0 } else { c.clamp(half, size - half) };
                        let c = vec2(clamp_axis(self.zoom_center.x, orig.x, half.x), clamp_axis(self.zoom_center.y, orig.y, half.y));
                        self.zoom_center = c;
                        c
                    }
                    None => orig / 2.0,
                };
                self.zoom_percent = self.zoom.map(|z| z * ppp * 100.0);

                // Sharpest available texture: all pixels once zoomed past the screen-sized copy.
                let needs_original = self.zoom.is_some_and(|z| z * ppp * orig.x > full.tex.size_vec2().x * 1.05);
                // GIFs play at their own frame timing; repaint when the next frame is due.
                let (frame_tex, next_frame) = full.frame_at(ui.input(|i| i.time));
                if let Some(wait) = next_frame {
                    ui.ctx().request_repaint_after(std::time::Duration::from_secs_f64(wait.max(0.005)));
                }
                let tex = match &self.original {
                    Some((s, t)) if *s == shot && needs_original && full.frames.is_empty() => t,
                    _ => frame_tex,
                };
                let image_rect = Rect::from_min_size(area.center() - center * scale, orig * scale);
                let radius = if self.zoom.is_some() { 0 } else { 4 };
                let mut clipped = ui.new_child(egui::UiBuilder::new().max_rect(area));
                clipped.set_clip_rect(area);
                Image::new((tex.id(), image_rect.size())).corner_radius(radius).paint_at(&clipped, image_rect);

                if self.zoom.is_some() {
                    resp.on_hover_cursor(if ui.input(|i| i.pointer.primary_down()) { CursorIcon::Grabbing } else { CursorIcon::Grab });
                }
            }
            _ => {
                if let Some(t) = self.thumbs.get(&shot) {
                    let fit = fit_rect(area, t.tex.size_vec2());
                    Image::new((t.tex.id(), fit.size())).corner_radius(4).paint_at(ui, fit);
                } else {
                    let reason = self.failed.get(&Key { shot, kind: Kind::Full });
                    let help = reason.and_then(|r| self.shots[shot].display_path().and_then(|path| codec_help(path, r)));
                    match (reason, help) {
                        (_, Some(help)) => codec_help_panel(ui, &p, area, &help),
                        (Some(_), None) => {
                            ui.painter().text(area.center(), Align2::CENTER_CENTER, "No preview found in this file", theme::font(14.0), p.label2);
                        }
                        (None, None) => {
                            ui.painter().text(area.center(), Align2::CENTER_CENTER, "Loading…", theme::font(14.0), p.label2);
                        }
                    }
                }
            }
        }

        // A clip: a Play button. MP4 and MOV play right here; RAW video opens in the
        // system's video player.
        let clip = self.shots[shot].video.then(|| self.shots[shot].display_path().cloned()).flatten();
        if let (Some(path), true) = (clip, self.zoom.is_none()) {
            let here = crate::player::can_play(&path);
            let button = Rect::from_center_size(area.center(), Vec2::splat(76.0));
            let tip = if here {
                format!("Play ({})", self.settings.keybinds.main_text(KeyAction::PlayPause))
            } else {
                "Play in your video player".to_string()
            };
            let play = ui.interact(button, Id::new("play-clip"), Sense::click()).on_hover_text(tip).on_hover_cursor(CursorIcon::PointingHand);
            let painter = ui.painter();
            painter.circle_filled(button.center(), 38.0, Color32::from_black_alpha(if play.hovered() { 200 } else { 150 }));
            play_triangle(painter, button.center(), 15.0, Color32::WHITE);
            // A clip that plays here starts from a click anywhere on it, like a paused one.
            if play.clicked() || (here && area_clicked) {
                if here {
                    self.toggle_playback();
                } else {
                    crate::window::open_link(&path.to_string_lossy());
                }
            }
        }

        if self.rejected.contains(&shot) && self.zoom.is_none() {
            let painter = ui.painter();
            let g = painter.layout_no_wrap("Rejected".to_string(), theme::font(13.0), Color32::WHITE);
            let r = Rect::from_center_size(pos2(area.center().x, area.min.y + 20.0), g.size() + vec2(44.0, 14.0));
            painter.rect_filled(r, 16, Color32::from_rgba_unmultiplied(30, 30, 30, 215));
            let c = pos2(r.min.x + 17.0, r.center().y);
            painter.circle_filled(c, 10.0, theme::DELETE_RED);
            x_mark(painter, c, 4.0, Color32::WHITE);
            painter.galley(pos2(r.min.x + 34.0, r.min.y + 7.0), g, Color32::WHITE);
        }

        if self.picked.contains(&shot) && self.zoom.is_none() {
            let painter = ui.painter();
            let s = &self.shots[shot];
            // Which of this shot's files Export on Pick uses, e.g. "JPG + CR3".
            let files = self
                .settings
                .export_files
                .pick(s.image.as_ref(), s.raw.as_ref())
                .into_iter()
                .map(|f| if Some(f) == s.raw.as_ref() { s.raw_kind() } else { s.image_kind() })
                .collect::<Vec<_>>()
                .join(" + ");
            let text = if self.settings.export_on_pick { format!("Picked · {files}") } else { "Picked".to_string() };
            let g = painter.layout_no_wrap(text, theme::font(13.0), Color32::WHITE);
            let r = Rect::from_center_size(pos2(area.center().x, area.min.y + 20.0), g.size() + vec2(44.0, 14.0));
            painter.rect_filled(r, 16, Color32::from_rgba_unmultiplied(30, 30, 30, 215));
            let c = pos2(r.min.x + 17.0, r.center().y);
            painter.circle_filled(c, 10.0, p.accent);
            check_mark(painter, c, 4.5, p.on_accent);
            painter.galley(pos2(r.min.x + 34.0, r.min.y + 7.0), g, Color32::WHITE);
        }

        // Load: all pixels if zoomed in, then this photo and its neighbours at screen size.
        let mut requests = Vec::new();
        if let (Some(orig), Some(zoom)) = (original_size, self.zoom) {
            let full_w = self.fulls.get(&shot).map(|f| f.tex.size_vec2().x).unwrap_or(0.0);
            let have = self.original.as_ref().is_some_and(|(s, _)| *s == shot);
            let key = Key { shot, kind: Kind::Original };
            if zoom * ppp * orig.x > full_w * 1.05 && !have && !self.failed.contains_key(&key) {
                if let Some(path) = self.shots[shot].display_path() {
                    let side = ui.input(|i| i.max_texture_side) as u32;
                    requests.push(Request { key, path: path.clone(), max: [side, side] });
                }
            }
        }
        let max = [(area.width() * ppp).max(800.0) as u32, (area.height() * ppp).max(600.0) as u32];
        self.viewer_px = max;
        let set = self.working_set();
        let pos = set.binary_search(&shot).unwrap_or_else(|i| i) as isize;
        // A loaded photo needs reloading if the window has grown well past its size.
        let too_small = |f: &FullImage| {
            let scale = (max[0] as f32 / f.original.x).min(max[1] as f32 / f.original.y).min(1.0);
            f.tex.size_vec2().x < f.original.x * scale * 0.8
        };
        for i in [pos, pos + 1, pos - 1, pos + 2] {
            // The shown photo always loads, even if it just left the set (e.g. unpicked).
            let s = if i == pos { Some(shot) } else { usize::try_from(i).ok().and_then(|i| set.get(i)).copied() };
            let Some(s) = s else { continue };
            let key = Key { shot: s, kind: Kind::Full };
            if let Some(path) = self.shots[s].display_path() {
                if self.fulls.get(&s).map_or(true, too_small) && !self.failed.contains_key(&key) {
                    requests.push(Request { key, path: path.clone(), max });
                }
            }
        }
        self.loader.want_full(requests);
        let keep: HashSet<usize> = [pos - 2, pos - 1, pos, pos + 1, pos + 2]
            .into_iter()
            .filter_map(|i| usize::try_from(i).ok().and_then(|i| set.get(i)).copied())
            .chain([shot])
            .collect();
        let read_ahead = &self.cache_keep;
        self.fulls.retain(|s, _| keep.contains(s) || read_ahead.contains(s));
        if self.original.as_ref().is_some_and(|(s, _)| *s != shot) {
            self.original = None;
        }
    }

    fn draw_toast(&mut self, ctx: &egui::Context) {
        let Some(toast) = &self.toast else { return };
        let age = ctx.input(|i| i.time) - toast.started;
        if age > TOAST_HOLD + TOAST_FADE {
            self.toast = None;
            return;
        }
        let alpha = if age < TOAST_HOLD { 1.0 } else { 1.0 - ((age - TOAST_HOLD) / TOAST_FADE) as f32 };
        let p = self.p;
        let painter = ctx.layer_painter(LayerId::new(Order::Foreground, Id::new("toast")));
        let white = Color32::WHITE.gamma_multiply(alpha);
        let g = painter.layout_no_wrap(toast.text.clone(), theme::font(13.5), white);
        let size = g.size() + vec2(50.0, 18.0);
        let r = Rect::from_center_size(pos2(self.content_rect.center().x, self.content_rect.max.y - 22.0 - size.y / 2.0), size);
        painter.rect_filled(r, CornerRadius::same(18), Color32::from_rgba_unmultiplied(40, 40, 42, 235).gamma_multiply(alpha));
        painter.rect_stroke(r, CornerRadius::same(18), Stroke::new(1.0_f32, Color32::from_white_alpha(30).gamma_multiply(alpha)), StrokeKind::Inside);
        let c = pos2(r.min.x + 20.0, r.center().y);
        painter.circle_filled(c, 10.0, p.accent.gamma_multiply(alpha));
        check_mark(&painter, c, 4.5, p.on_accent.gamma_multiply(alpha));
        painter.galley(pos2(r.min.x + 38.0, r.center().y - g.size().y / 2.0), g, white);
        ctx.request_repaint();
    }
}

impl eframe::App for App {
    fn raw_input_hook(&mut self, ctx: &egui::Context, raw_input: &mut egui::RawInput) {
        if std::mem::take(&mut self.release_pointer) {
            if let Some(pos) = ctx.input(|i| i.pointer.latest_pos()) {
                raw_input.events.push(egui::Event::PointerButton {
                    pos,
                    button: egui::PointerButton::Primary,
                    pressed: false,
                    modifiers: raw_input.modifiers,
                });
            }
        }
    }

    fn update(&mut self, ctx: &egui::Context, _frame: &mut eframe::Frame) {
        self.frame_ui(ctx);
    }

    /// Clear to see-through only while Windows 11's Mica shows in the top bar. Otherwise
    /// the window is solid, so nothing behind it (the desktop on Windows 10, say) can
    /// show through and wash out text.
    fn clear_color(&self, _visuals: &egui::Visuals) -> [f32; 4] {
        if self.mica_active {
            [0.0; 4]
        } else {
            let c = self.p.bg;
            [c.r() as f32 / 255.0, c.g() as f32 / 255.0, c.b() as f32 / 255.0, 1.0]
        }
    }
}

impl App {
    /// Everything drawn and handled in one frame.
    fn frame_ui(&mut self, ctx: &egui::Context) {
        self.frame += 1;
        // A clip stops when its viewer closes or another photo shows.
        if self.playback.as_ref().is_some_and(|pb| self.screen != Screen::Viewer || pb.shot != self.current) {
            self.playback = None;
        }
        // Full screen belongs to the viewer.
        if self.fullscreen && (self.screen != Screen::Viewer || self.shots.is_empty()) {
            self.set_fullscreen(ctx, false);
        }
        self.receive(ctx);
        self.finish_delete(ctx);
        self.guard_close(ctx);

        self.handle_drops(ctx);
        self.start_pending_drag(ctx);

        #[derive(Default)]
        struct Keys {
            /// The mouse's back button (usually "Mouse 4", the side button nearer you).
            back: bool,
            delete: bool,
            reject: bool,
            copy: bool,
            open: bool,
            settings: bool,
            play: bool,
            skip_back: bool,
            skip_forward: bool,
            fullscreen: bool,
            export: bool,
            select_all: bool,
            esc: bool,
            enter: bool,
            left: bool,
            right: bool,
            pick: bool,
            home: bool,
            end: bool,
        }
        let capturing = self.capture_keybind(ctx);
        let binds = &self.settings.keybinds;
        let mut k = ctx.input(|i| Keys {
            open: binds.pressed(KeyAction::OpenFolder, i),
            export: binds.pressed(KeyAction::QuickExport, i),
            select_all: binds.pressed(KeyAction::SelectAll, i),
            esc: binds.pressed(KeyAction::Back, i),
            enter: binds.pressed(KeyAction::OpenPhoto, i),
            left: binds.pressed(KeyAction::Previous, i),
            right: binds.pressed(KeyAction::Next, i),
            pick: binds.pressed(KeyAction::Pick, i),
            home: binds.pressed(KeyAction::First, i),
            end: binds.pressed(KeyAction::Last, i),
            back: i.pointer.button_pressed(egui::PointerButton::Extra1),
            delete: binds.pressed(KeyAction::Delete, i),
            reject: binds.pressed(KeyAction::Reject, i),
            copy: binds.pressed(KeyAction::Copy, i),
            settings: binds.pressed(KeyAction::Settings, i),
            play: binds.pressed(KeyAction::PlayPause, i),
            skip_back: binds.pressed(KeyAction::SkipBack, i),
            skip_forward: binds.pressed(KeyAction::SkipForward, i),
            fullscreen: binds.pressed(KeyAction::FullScreen, i),
        });
        // While the delete confirmation is up, it gets the keys (Esc cancels it); while
        // a delete runs, nothing should touch the photos.
        if self.confirm_delete.is_some()
            || self.deleting.is_some()
            || self.update_prompt
            || self.confirm_close
            || self.drive_scan.is_some()
            || capturing
        {
            k = Keys::default();
        }
        // Ctrl + , opens or closes Settings, as in many Windows apps.
        if k.settings {
            self.settings_page = match self.settings_page {
                Some(_) => None,
                None => Some(SettingsPage::DragDrop),
            };
            k = Keys::default();
        }
        // While Settings is open, keys don't reach the grid or viewer; Esc or the mouse
        // back button closes it.
        if self.settings_page.is_some() {
            if k.esc || k.back {
                self.settings_page = None;
            }
            k = Keys::default();
        }
        if k.open {
            self.choose_folder(ctx);
        }
        if k.export && self.folder.is_some() && self.range_mode == RangeMode::Off {
            self.quick_export();
        }
        if k.copy && self.folder.is_some() && self.range_mode == RangeMode::Off {
            self.copy_selected(ctx);
        }
        if k.reject && self.folder.is_some() && self.range_mode == RangeMode::Off {
            self.reject_selected();
        }
        if k.delete && self.folder.is_some() && self.range_mode == RangeMode::Off {
            self.request_delete();
        }
        match self.screen {
            Screen::Viewer if !self.shots.is_empty() => {
                if (k.esc || k.back) && self.fullscreen {
                    self.set_fullscreen(ctx, false);
                } else if k.esc || k.back {
                    self.back_to_grid();
                }
                if k.fullscreen {
                    self.set_fullscreen(ctx, !self.fullscreen);
                }
                if k.skip_back {
                    self.skip_clip(-5.0);
                }
                if k.skip_forward {
                    self.skip_clip(5.0);
                }
                if k.left {
                    self.step(-1);
                }
                if k.right {
                    self.step(1);
                }
                if k.home {
                    self.step(-(self.shots.len() as isize));
                }
                if k.end {
                    self.step(self.shots.len() as isize);
                }
                if k.pick {
                    self.toggle_pick(self.current);
                }
                if k.play {
                    self.toggle_playback();
                }
            }
            _ => {
                if k.esc {
                    if self.range_mode != RangeMode::Off {
                        self.range_mode = RangeMode::Off;
                    } else {
                        self.selected.clear();
                    }
                }
                if k.select_all {
                    let set = self.working_set();
                    self.selected.extend(set);
                }
                if k.enter {
                    if let Some(shot) = self.select_anchor.filter(|s| self.selected.contains(s)) {
                        self.open_viewer(shot);
                    }
                }
                if k.pick && self.range_mode == RangeMode::Off && self.folder.is_some() {
                    self.pick_selected();
                }
            }
        }

        // Full screen is just the photo or clip.
        if !self.fullscreen {
            self.top_bar(ctx);
            self.bottom_bar(ctx);
        }

        let p = self.p;
        let viewer = self.screen == Screen::Viewer && !self.shots.is_empty();
        let margin = if self.fullscreen {
            Margin::ZERO
        } else if viewer {
            // Little at the bottom: the strip's tab sits just above the bar's line.
            Margin { left: 24, right: 24, top: 4, bottom: 4 }
        } else if self.folder.is_some() {
            // Grid: the scroll bar sits against the window's right edge.
            Margin { left: 16, right: 2, top: 12, bottom: 0 }
        } else {
            Margin::same(16)
        };
        let fill = if self.fullscreen { Color32::BLACK } else if viewer { p.viewer_bg } else { p.bg };
        let frame = Frame::new().fill(fill).inner_margin(margin);
        let panel = egui::CentralPanel::default().frame(frame).show(ctx, |ui| {
            if self.folder.is_none() {
                self.empty_screen(ui, ctx);
            } else if viewer {
                self.viewer_screen(ui);
            } else {
                self.grid_screen(ui);
            }
        });
        self.content_rect = panel.response.rect;
        self.settings_window(ctx);
        self.delete_dialog(ctx);
        self.delete_progress(ctx);
        self.update_dialog(ctx);
        self.export_progress(ctx);
        self.close_dialog(ctx);
        self.poll_drive_scan(ctx);
        self.drive_scan_dialog(ctx);
        self.read_ahead();
        self.draw_toast(ctx);
        self.draw_drop_overlay(ctx);
        self.draw_drag_out_overlay(ctx);
    }

    /// While something is dragged over the window: dim it, outline it in blue and say
    /// what dropping will do (only folders open).
    fn draw_drop_overlay(&self, ctx: &egui::Context) {
        let hovered: Vec<PathBuf> = ctx.input(|i| i.raw.hovered_files.iter().filter_map(|f| f.path.clone()).collect());
        let Some((title, detail)) = drop_hint(&hovered, &self.last_drag) else { return };
        let p = self.p;
        let screen = ctx.screen_rect();
        let painter = ctx.layer_painter(LayerId::new(Order::Foreground, Id::new("drop-overlay")));
        painter.rect_filled(screen, 0, Color32::from_black_alpha(150));
        painter.rect_stroke(screen.shrink(3.0), p.r_card.min(8), Stroke::new(2.0_f32, p.accent), StrokeKind::Inside);

        let mut lines = vec![painter.layout_no_wrap(title, theme::font_semibold(16.0), p.label)];
        lines.extend(detail.map(|d| painter.layout_no_wrap(d, theme::font(13.0), p.label2)));
        center_card(&painter, screen.center(), &p, lines, &[6.0], vec2(28.0, 26.0), p.accent);
        ctx.request_repaint();
    }

    /// Settings: a sidebar of pages and the chosen page.
    fn settings_window(&mut self, ctx: &egui::Context) {
        let Some(page) = self.settings_page else { return };
        let p = self.p;
        let mut open = true;
        let mut chosen_theme = None;
        let mut export_action = ExportAction::None;
        let mut keybind_change = KeybindChange::None;
        // Every page gets the same fixed area (long pages scroll inside it), so the
        // window keeps one size when switching pages.
        const SIDEBAR_W: f32 = 140.0;
        const PAGE: Vec2 = vec2(460.0, 420.0);
        egui::Window::new(RichText::new("Settings").font(theme::font_semibold(15.0)))
            .open(&mut open)
            .collapsible(false)
            .resizable(false)
            .anchor(Align2::CENTER_CENTER, Vec2::ZERO)
            .frame(Frame::window(&ctx.style()).fill(p.card).corner_radius(p.r_card).inner_margin(Margin::same(16)))
            .show(ctx, |ui| {
                ui.horizontal_top(|ui| {
                    // Sidebar.
                    ui.vertical(|ui| {
                        ui.set_width(SIDEBAR_W);
                        ui.set_min_height(PAGE.y);
                        let pages = [
                            (SettingsPage::DragDrop, "Drag & Drop"),
                            (SettingsPage::ExportOnPick, "Export on Pick"),
                            (SettingsPage::Keybinds, "Keybinds"),
                            (SettingsPage::Memory, "Memory"),
                            (SettingsPage::Themes, "Themes"),
                        ];
                        for (target, label) in pages {
                            let on = page == target;
                            let b = Button::new(RichText::new(label).color(if on { p.on_accent } else { p.label }))
                                .fill(if on { p.accent } else { Color32::TRANSPARENT })
                                .stroke(Stroke::NONE)
                                .corner_radius(p.r_ctl)
                                .min_size(vec2(SIDEBAR_W, 32.0));
                            if ui.add(b).clicked() {
                                self.settings_page = Some(target);
                            }
                        }
                    });
                    ui.add_space(8.0);
                    ui.separator();
                    ui.add_space(8.0);
                    ui.allocate_ui_with_layout(PAGE, Layout::top_down(Align::Min), |ui| {
                        ui.set_min_size(PAGE);
                        ui.set_max_size(PAGE);
                        ScrollArea::vertical().auto_shrink(false).show(ui, |ui| match page {
                            SettingsPage::Keybinds => keybind_change = keybinds_page(ui, &p, &self.settings.keybinds, self.rebinding),
                            SettingsPage::DragDrop => drag_page(ui, &p, &mut self.settings),
                            SettingsPage::Memory => memory_page(ui, &p, &mut self.settings),
                            SettingsPage::ExportOnPick => export_action = export_page(ui, &p, &mut self.settings),
                            SettingsPage::Themes => chosen_theme = themes_page(ui, &p, self.theme_shown),
                        });
                    });
                });
            });
        if let Some(theme) = chosen_theme {
            self.set_theme(ctx, theme);
        }
        match keybind_change {
            KeybindChange::None => {}
            KeybindChange::Toggle(action) => {
                self.rebinding = if self.rebinding == Some(action) { None } else { Some(action) };
            }
            KeybindChange::Restore => {
                self.rebinding = None;
                self.settings.keybinds = keybinds::Keybinds::default();
                self.settings.save();
            }
        }
        match export_action {
            ExportAction::None => {}
            // Export on Pick needs somewhere to copy to: ask first; cancelling leaves it off.
            ExportAction::TurnOn => {
                if self.settings.export_dir.is_some() || self.choose_export() {
                    self.settings.export_on_pick = true;
                    self.settings.save();
                }
            }
            ExportAction::TurnOff => {
                self.settings.export_on_pick = false;
                self.settings.save();
            }
            ExportAction::ChooseFolder => {
                self.choose_export();
            }
        }
        if !open {
            self.settings_page = None;
        }
    }
}

fn keybinds_page(ui: &mut Ui, p: &Palette, binds: &keybinds::Keybinds, rebinding: Option<KeyAction>) -> KeybindChange {
    let mut change = KeybindChange::None;
    ui.label(RichText::new("Keyboard").font(theme::font_semibold(13.5)).color(p.label2));
    ui.label(RichText::new("Click a shortcut, then press its new key. Esc cancels.").color(p.label2));
    ui.add_space(6.0);
    egui::Grid::new("keyboard").num_columns(2).spacing(vec2(18.0, 6.0)).show(ui, |ui| {
        for action in KeyAction::ALL {
            ui.label(action.label());
            let waiting = rebinding == Some(action);
            let text = if waiting {
                RichText::new("Press a key…").color(p.on_accent)
            } else {
                RichText::new(binds.text(action)).font(theme::font_semibold(13.0))
            };
            let mut button = Button::new(text).corner_radius(p.r_ctl).min_size(vec2(150.0, 28.0));
            if waiting {
                button = button.fill(p.accent).stroke(Stroke::NONE);
            }
            if ui.add(button).clicked() {
                change = KeybindChange::Toggle(action);
            }
            ui.end_row();
        }
    });
    ui.add_space(14.0);
    ui.label(RichText::new("Mouse").font(theme::font_semibold(13.5)).color(p.label2));
    ui.add_space(4.0);
    egui::Grid::new("mouse").num_columns(2).spacing(vec2(18.0, 7.0)).show(ui, |ui| {
        for (control, action) in MOUSE_CONTROLS {
            ui.label(RichText::new(platform::keys(control)).font(theme::font_semibold(13.0)));
            ui.label(RichText::new(*action).color(p.label2));
            ui.end_row();
        }
    });
    ui.add_space(16.0);
    if ui.add(Button::new("Restore Defaults").corner_radius(p.r_ctl).min_size(vec2(150.0, 32.0))).clicked() {
        change = KeybindChange::Restore;
    }
    change
}

fn drag_page(ui: &mut Ui, p: &Palette, settings: &mut Settings) {
    ui.label(RichText::new("Dragging photos out").font(theme::font_semibold(13.5)).color(p.label2));
    ui.label(
        RichText::new("Drag photos from the grid into Lightroom, LightCraft or a folder. A selection drags as one group.")
            .color(p.label2),
    );
    ui.add_space(10.0);
    ui.label(RichText::new("Files to send").font(theme::font_semibold(13.5)));
    ui.add_space(4.0);
    for choice in DragFiles::ALL {
        let on = settings.drag_files == choice;
        let r = ui.radio(on, RichText::new(choice.label()).font(theme::font_semibold(13.5)));
        ui.indent(choice.label(), |ui| {
            ui.label(RichText::new(choice.description()).color(p.label2));
        });
        if r.clicked() && !on {
            settings.drag_files = choice;
            settings.save();
        }
        ui.add_space(4.0);
    }
    ui.add_space(8.0);
    ui.label(
        RichText::new("Photos already copied to your export folder are sent from there, not from the memory card.")
            .color(p.label2)
            .italics(),
    );
}

/// The Memory page: "Use less memory", or how much memory photos read ahead may take.
fn memory_page(ui: &mut Ui, p: &Palette, settings: &mut Settings) {
    ui.horizontal(|ui| {
        if toggle_switch(ui, p, settings.memory_saver).clicked() {
            settings.memory_saver = !settings.memory_saver;
            settings.save();
        }
        ui.label(RichText::new("Use less memory").font(theme::font_semibold(14.0)));
    });
    ui.add_space(6.0);
    ui.label(
        RichText::new(
            "On: Cullery keeps only the photos just around the one you're viewing. Best for computers \
             with little memory.",
        )
        .color(p.label2),
    );
    ui.add_space(4.0);
    ui.label(
        RichText::new(
            "Off: Cullery reads the photos around the one you're viewing ahead of time and keeps them, \
             so moving through a shoot doesn't wait. Small versions of every photo come first, then \
             sharp ones with the memory left over.",
        )
        .color(p.label2),
    );

    let total = crate::platform::total_memory();
    ui.add_space(14.0);
    ui.add_enabled_ui(!settings.memory_saver, |ui| {
        ui.label(RichText::new("Memory for photos").font(theme::font_semibold(13.5)));
        ui.add_space(4.0);
        ui.horizontal(|ui| {
            ui.spacing_mut().slider_width = 220.0;
            let (lo, hi) = crate::settings::MEMORY_PERCENT;
            let slider = ui.add(Slider::new(&mut settings.memory_percent, lo..=hi).step_by(1.0).show_value(false));
            let gb = |percent: f32| total.map(|t| t as f64 * percent as f64 / 100.0 / 1e9);
            let text = match (gb(settings.memory_percent), total) {
                (Some(gb), Some(t)) => format!("{gb:.1} GB  ({:.0}% of {:.0} GB)", settings.memory_percent, t as f64 / 1e9),
                _ => format!("{:.0}% of this computer's memory", settings.memory_percent),
            };
            ui.label(RichText::new(text).font(theme::font_semibold(13.0)));
            if slider.drag_stopped() || (slider.changed() && !slider.dragged()) {
                settings.save();
            }
        });
        ui.add_space(4.0);
        ui.label(
            RichText::new("Cullery may use up to this much for photos. More keeps more of a big shoot ready.")
                .color(p.label2),
        );
    });
}

/// The Export on Pick page. Returns anything that needs the app (a folder dialog).
fn export_page(ui: &mut Ui, p: &Palette, settings: &mut Settings) -> ExportAction {
    let mut action = ExportAction::None;
    ui.horizontal(|ui| {
        if toggle_switch(ui, p, settings.export_on_pick).clicked() {
            action = if settings.export_on_pick { ExportAction::TurnOff } else { ExportAction::TurnOn };
        }
        ui.label(RichText::new("Export on Pick").font(theme::font_semibold(14.0)));
    });
    ui.add_space(6.0);
    ui.label(
        RichText::new(
            "When this is on, picking a photo automatically creates a copy of it in your export folder. \
             Picking the same photo again never makes a second copy, and unpicking leaves copies where they are.",
        )
        .color(p.label2),
    );
    ui.add_space(4.0);
    ui.label(
        RichText::new(
            "When it's off, picking just marks your keepers. Use View Picked to drag them into Lightroom \
             or LightCraft, or Quick Export to copy them.",
        )
        .color(p.label2),
    );

    ui.add_space(14.0);
    ui.label(RichText::new("Export folder").font(theme::font_semibold(13.5)));
    ui.horizontal(|ui| {
        let (text, color) = match &settings.export_dir {
            Some(dir) => (dir.display().to_string(), p.label),
            None => ("Not set".to_string(), p.label2),
        };
        ui.add(egui::Label::new(RichText::new(text).color(color)).truncate());
        if ui.button("Choose…").clicked() {
            action = ExportAction::ChooseFolder;
        }
    });

    ui.add_space(14.0);
    ui.label(RichText::new("Files to copy").font(theme::font_semibold(13.5)));
    ui.add_space(4.0);
    for choice in [DragFiles::Both, DragFiles::Raw, DragFiles::Jpg] {
        let on = settings.export_files == choice;
        let r = ui.radio(on, RichText::new(choice.label()).font(theme::font_semibold(13.5)));
        ui.indent(("export", choice.label()), |ui| {
            ui.label(RichText::new(choice.export_description()).color(p.label2));
        });
        if r.clicked() && !on {
            settings.export_files = choice;
            settings.save();
        }
        ui.add_space(4.0);
    }
    ui.add_space(6.0);
    ui.label(RichText::new("Quick Export copies the same files.").color(p.label2).italics());
    action
}

/// An on/off switch in the theme's accent color.
fn toggle_switch(ui: &mut Ui, p: &Palette, on: bool) -> Response {
    let (rect, resp) = ui.allocate_exact_size(vec2(40.0, 22.0), Sense::click());
    let t = ui.ctx().animate_bool(resp.id, on);
    let painter = ui.painter();
    let radius = if p.r_pill == 0 { 0 } else { 11 };
    let track = if on { p.accent } else { p.control };
    painter.rect_filled(rect, radius, track);
    painter.rect_stroke(rect, radius, Stroke::new(1.0_f32, if on { p.accent } else { p.control_border }), StrokeKind::Inside);
    let x = egui::lerp(rect.min.x + 11.0..=rect.max.x - 11.0, t);
    let knob = if on { p.on_accent } else { p.label2 };
    if p.r_pill == 0 {
        painter.rect_filled(Rect::from_center_size(pos2(x, rect.center().y), Vec2::splat(12.0)), 0, knob);
    } else {
        painter.circle_filled(pos2(x, rect.center().y), 7.0, knob);
    }
    resp.on_hover_cursor(CursorIcon::PointingHand)
}

/// A card per theme, drawn in that theme's own colors and corner style. Returns the
/// theme clicked, if any.
fn themes_page(ui: &mut Ui, p: &Palette, current: ThemeId) -> Option<ThemeId> {
    let mut chosen = None;
    ui.label(RichText::new("Theme").font(theme::font_semibold(13.5)).color(p.label2));
    ui.add_space(6.0);
    egui::Grid::new("themes").num_columns(2).spacing(vec2(12.0, 12.0)).show(ui, |ui| {
        for (i, id) in ThemeId::ALL.into_iter().enumerate() {
            if theme_card(ui, p, id, id == current).clicked() && id != current {
                chosen = Some(id);
            }
            if i % 2 == 1 {
                ui.end_row();
            }
        }
    });
    ui.add_space(12.0);
    if cfg!(windows) {
        ui.label(
            RichText::new("Windows 11 themes show your wallpaper's tint (Mica) in the top bar.")
                .color(p.label2)
                .italics(),
        );
    }
    chosen
}

/// A small preview of a theme: its top bar, a photo tile, an accent button.
fn theme_card(ui: &mut Ui, p: &Palette, id: ThemeId, selected: bool) -> Response {
    let t = id.palette();
    let (rect, resp) = ui.allocate_exact_size(vec2(210.0, 118.0), Sense::click());
    let painter = ui.painter();
    let r = t.r_card.min(10);
    painter.rect_filled(rect, r, t.bg);
    let bar = Rect::from_min_size(rect.min, vec2(rect.width(), 22.0));
    let bar_fill = if t.mica && cfg!(windows) { t.mica_preview } else { t.bar };
    painter.rect_filled(bar, CornerRadius { nw: r, ne: r, sw: 0, se: 0 }, bar_fill);
    for i in 0..3 {
        let tile = Rect::from_min_size(rect.min + vec2(10.0 + i as f32 * 40.0, 30.0), vec2(34.0, 23.0));
        painter.rect_filled(tile, t.r_thumb.min(4), t.tile);
    }
    let button = Rect::from_min_size(rect.min + vec2(130.0, 32.0), vec2(68.0, 18.0));
    painter.rect_filled(button, t.r_pill.min(9), t.accent);
    painter.text(rect.left_bottom() + vec2(10.0, -30.0), Align2::LEFT_CENTER, id.name(), theme::font_semibold(13.0), t.label);
    painter.text(rect.left_bottom() + vec2(10.0, -13.0), Align2::LEFT_CENTER, id.subtitle(), theme::font(11.5), t.label2);
    let stroke = if selected {
        Stroke::new(2.5_f32, p.accent)
    } else if resp.hovered() {
        Stroke::new(1.5_f32, p.label2)
    } else {
        Stroke::new(1.0_f32, p.control_border)
    };
    painter.rect_stroke(rect, r, stroke, StrokeKind::Outside);
    resp.on_hover_cursor(CursorIcon::PointingHand)
}

// -------------------------------------------------------------------- helpers

/// What to tell the user when Windows can't open a HEIC/HEIF/AVIF file because a
/// Microsoft Store extension is missing.
#[derive(Clone, Copy, Debug, PartialEq)]
struct CodecHelp {
    /// Fits on a grid tile.
    short: &'static str,
    title: &'static str,
    detail: &'static str,
    button: &'static str,
    store_link: &'static str,
}

const HEIF_EXTENSION: CodecHelp = CodecHelp {
    short: "Needs HEIC reader",
    title: "This HEIC photo needs a reader",
    detail: "Windows opens HEIC photos with Microsoft's free HEIF Image Extension. Install it, then reopen this folder.",
    button: "Get HEIF Image Extension",
    store_link: "ms-windows-store://pdp/?ProductId=9PMMSR1CGPWG",
};

const HEVC_EXTENSION: CodecHelp = CodecHelp {
    short: "Needs HEVC extension",
    title: "Couldn't open this HEIC photo",
    detail: "Windows has a HEIC reader, but HEIC photos are compressed with HEVC, which also needs the HEVC Video Extensions. Install it, then reopen this folder. (If it still won't open, the file may be damaged.)",
    button: "Get HEVC Video Extensions",
    store_link: "ms-windows-store://pdp/?ProductId=9NMZLZ57R3T7",
};

const AV1_EXTENSION: CodecHelp = CodecHelp {
    short: "Needs AVIF reader",
    title: "This AVIF image needs a reader",
    detail: "Windows opens AVIF images with Microsoft's free AV1 Video Extension. Install it, then reopen this folder.",
    button: "Get AV1 Video Extension",
    store_link: "ms-windows-store://pdp/?ProductId=9MVZQVXJBQ9V",
};

/// Help for a file that failed to load, if the failure is a missing Windows extension.
fn codec_help(path: &Path, reason: &str) -> Option<CodecHelp> {
    let ext = path.extension()?.to_str()?.to_ascii_lowercase();
    let windows_failed = reason == crate::wic::NO_READER || reason.starts_with(crate::wic::DECODE_FAILED);
    match ext.as_str() {
        "heic" | "heif" if reason == crate::wic::NO_READER => Some(HEIF_EXTENSION),
        "heic" | "heif" if windows_failed => Some(HEVC_EXTENSION),
        "avif" if windows_failed => Some(AV1_EXTENSION),
        _ => None,
    }
}

/// The viewer's message for a missing extension, with a button to its Store page.
fn codec_help_panel(ui: &mut Ui, p: &Palette, area: Rect, help: &CodecHelp) {
    let card = Rect::from_center_size(area.center(), vec2(440.0, 170.0));
    ui.scope_builder(egui::UiBuilder::new().max_rect(card).layout(Layout::top_down(Align::Center)), |ui| {
        ui.label(RichText::new(help.title).font(theme::font_semibold(17.0)).color(p.label));
        ui.add_space(6.0);
        ui.label(RichText::new(help.detail).color(p.label2));
        ui.add_space(14.0);
        let button = Button::new(RichText::new(help.button).color(p.on_accent).font(theme::font_semibold(13.5)))
            .fill(p.accent)
            .stroke(Stroke::NONE)
            .corner_radius(p.r_pill)
            .min_size(vec2(0.0, 34.0));
        if ui.add(button).on_hover_text("Opens the Microsoft Store").clicked() {
            crate::window::open_link(help.store_link);
        }
    });
}

fn plural<'a>(n: usize, one: &'a str, many: &'a str) -> &'a str {
    if n == 1 { one } else { many }
}

/// A card in the middle of the window with centered lines of text: `gaps` between the
/// lines, `pad` inside the card's edge, and a thin `border`.
fn center_card(painter: &egui::Painter, center: Pos2, p: &Palette, lines: Vec<Arc<egui::Galley>>, gaps: &[f32], pad: Vec2, border: Color32) {
    let gap_total: f32 = gaps.iter().take(lines.len().saturating_sub(1)).sum();
    let width = lines.iter().map(|g| g.size().x).fold(0.0, f32::max) + pad.x * 2.0;
    let height = lines.iter().map(|g| g.size().y).sum::<f32>() + gap_total + pad.y * 2.0;
    let card = Rect::from_center_size(center, vec2(width, height));
    let r = theme::around(p.r_card, 2);
    painter.rect_filled(card, r, p.card);
    painter.rect_stroke(card, r, Stroke::new(1.0_f32, border), StrokeKind::Inside);
    let mut y = card.min.y + pad.y;
    for (i, g) in lines.into_iter().enumerate() {
        let h = g.size().y;
        painter.galley(pos2(card.center().x - g.size().x / 2.0, y), g, p.label);
        y += h + gaps.get(i).copied().unwrap_or(0.0);
    }
}

/// Whether any photo here has a RAW partner; without one, Show Paired RAW does nothing.
fn has_raw_pairs(shots: &[Shot]) -> bool {
    shots.iter().any(|s| s.image.is_some() && s.raw.is_some())
}

/// The drag-out overlay's text: (title, how many photos, which files go).
fn drag_out_hint(shots: &[&Shot], choice: DragFiles) -> (String, String, String) {
    let n = shots.len();
    let count = format!("{n} {}", plural(n, "photo", "photos"));
    let with_raw = shots.iter().filter(|s| s.raw.is_some()).count();
    let with_image = shots.iter().filter(|s| s.image.is_some()).count();
    // The images' type when they're all one ("JPG", "PNG"), else just "image".
    let mut kinds = shots.iter().filter(|s| s.image.is_some()).map(|s| s.image_kind());
    let kind = kinds.next().filter(|first| kinds.all(|k| k == *first));
    let image = kind.clone().unwrap_or_else(|| "image".to_string());
    let note = match choice {
        DragFiles::Raw if with_raw == n => "Only the RAW files will be sent".to_string(),
        DragFiles::Raw if with_raw == 0 => format!("No RAW files, so the {image}s will be sent"),
        DragFiles::Raw => format!("Only the RAW files will be sent ({image}s for photos without one)"),
        DragFiles::Jpg if with_image == n => format!("Only the {image} files will be sent"),
        DragFiles::Jpg if with_image == 0 => "No image files, so the RAWs will be sent".to_string(),
        DragFiles::Jpg => format!("Only the {image} files will be sent (RAWs for photos without one)"),
        DragFiles::Both if with_raw == 0 => format!("The {image} files will be sent"),
        DragFiles::Both if with_image == 0 => "The RAW files will be sent".to_string(),
        DragFiles::Both => format!("{} + RAW files will be sent", kind.as_deref().unwrap_or("Image")),
    };
    ("Drop into a folder or Lightroom".to_string(), count, note)
}

/// What dropping the hovered items would do: (title, detail), or `None` to show
/// nothing (nothing hovered, or it's Cullery's own drag on its way out).
fn drop_hint(hovered: &[PathBuf], own_drag: &HashSet<PathBuf>) -> Option<(String, Option<String>)> {
    if hovered.is_empty() || hovered.iter().all(|p| own_drag.contains(p)) {
        return None;
    }
    Some(match hovered.iter().find(|p| p.is_dir()) {
        Some(dir) => (format!("Drop to open {}", folder_name(dir)), None),
        None => (
            "Drop a folder to open it".to_string(),
            Some("Individual photos can't be opened. Drag in the folder they're in.".to_string()),
        ),
    })
}

/// The double-click speed set in Windows (Mouse settings), in seconds, plus a little
/// slack: Windows times press to press, egui release to release, once per frame.
fn system_double_click_time() -> f64 {
    #[cfg(windows)]
    {
        #[link(name = "user32")]
        extern "system" {
            fn GetDoubleClickTime() -> u32;
        }
        // SAFETY: takes no arguments and only reads a system setting.
        let ms = unsafe { GetDoubleClickTime() };
        if (100..=5000).contains(&ms) {
            return ms as f64 / 1000.0 + 0.05;
        }
    }
    0.55
}

/// A folder's path, shortened when it's more than three folders deep: the first
/// folder and the last, with "..." between (`C:\Users\...\Wedding`).
fn short_path(dir: &Path) -> String {
    use std::path::Component;
    let names: Vec<String> = dir
        .components()
        .filter_map(|c| match c {
            Component::Normal(name) => Some(name.to_string_lossy().into_owned()),
            _ => None,
        })
        .collect();
    if names.len() <= 3 {
        return dir.display().to_string();
    }
    // The drive (`C:\`) or `/`: everything before the first folder.
    let root: String = dir
        .components()
        .take_while(|c| !matches!(c, Component::Normal(_)))
        .map(|c| c.as_os_str().to_string_lossy().into_owned())
        .collect();
    let sep = std::path::MAIN_SEPARATOR;
    format!("{root}{}{sep}...{sep}{}", names[0], names[names.len() - 1])
}

fn folder_name(dir: &Path) -> String {
    dir.file_name()
        .map(|n| n.to_string_lossy().into_owned())
        .unwrap_or_else(|| dir.display().to_string())
}

/// The largest rect with `size`'s aspect ratio that fits in `outer`, centered.
fn fit_rect(outer: Rect, size: Vec2) -> Rect {
    if size.x <= 0.0 || size.y <= 0.0 {
        return outer;
    }
    let scale = (outer.width() / size.x).min(outer.height() / size.y);
    Rect::from_center_size(outer.center(), size * scale)
}

fn check_mark(painter: &egui::Painter, c: Pos2, s: f32, color: Color32) {
    let points = vec![c + vec2(-s, 0.0), c + vec2(-s * 0.3, s * 0.7), c + vec2(s, -s * 0.7)];
    painter.add(Shape::line(points, Stroke::new(2.2_f32, color)));
}

fn chevron_down(painter: &egui::Painter, c: Pos2, s: f32, color: Color32) {
    let points = vec![c + vec2(-s, -s * 0.5), c + vec2(0.0, s * 0.5), c + vec2(s, -s * 0.5)];
    painter.add(Shape::line(points, Stroke::new(1.8_f32, color)));
}

/// A `‹` / `›` chevron drawn as lines so it sits exactly in the middle of its circle.
fn chevron(painter: &egui::Painter, c: Pos2, half_h: f32, left: bool, width: f32, color: Color32) {
    let d = if left { 1.0 } else { -1.0 };
    let w = half_h * 0.55;
    // The chevron's visual middle is halfway between its tip and its arms.
    let points = vec![
        c + vec2(d * w / 2.0, -half_h),
        c + vec2(-d * w / 2.0, 0.0),
        c + vec2(d * w / 2.0, half_h),
    ];
    painter.add(Shape::line(points, Stroke::new(width, color)));
}

const PICK_W: f32 = 130.0;
/// The delete button: a little smaller than Pick, with a small gap beside it.
const TRASH_W: f32 = 40.0;
const TRASH_GAP: f32 = 12.0;

/// Square button with a red trash can, for deleting photos.
fn trash_button(ui: &mut Ui, p: &Palette, enabled: bool) -> Response {
    let sense = if enabled { Sense::click() } else { Sense::hover() };
    let (rect, resp) = ui.allocate_exact_size(Vec2::splat(TRASH_W), sense);
    let painter = ui.painter();
    let hovered = enabled && resp.hovered();
    let fill = if enabled && resp.is_pointer_button_down_on() {
        theme::shade(p, p.control, 24)
    } else if hovered {
        theme::shade(p, p.control, 12)
    } else {
        p.control
    };
    let border = if hovered { p.danger } else { p.control_border };
    let r = p.r_pill.min(10);
    painter.rect_filled(rect, r, fill);
    painter.rect_stroke(rect, r, Stroke::new(1.0_f32, border), StrokeKind::Inside);

    // The can: lid, handle, tapered body and two ribs.
    let color = if enabled { p.danger } else { p.label2.gamma_multiply(0.5) };
    let stroke = Stroke::new(1.8_f32, color);
    let c = rect.center();
    painter.line_segment([c + vec2(-8.0, -6.0), c + vec2(8.0, -6.0)], stroke);
    painter.add(Shape::line(vec![c + vec2(-3.0, -6.0), c + vec2(-3.0, -9.0), c + vec2(3.0, -9.0), c + vec2(3.0, -6.0)], stroke));
    painter.add(Shape::closed_line(
        vec![c + vec2(-6.0, -4.0), c + vec2(6.0, -4.0), c + vec2(4.8, 9.0), c + vec2(-4.8, 9.0)],
        stroke,
    ));
    painter.line_segment([c + vec2(-2.0, -1.0), c + vec2(-1.7, 6.0)], stroke);
    painter.line_segment([c + vec2(2.0, -1.0), c + vec2(1.7, 6.0)], stroke);
    if enabled {
        resp.on_hover_cursor(CursorIcon::PointingHand)
    } else {
        resp
    }
}
const SCROLLBAR_W: f32 = 14.0;

/// Thin scroll bar with small arrow buttons, in its own strip (like Chromium's on Windows).
/// Returns a new scroll offset when the user moved it.
fn scrollbar(ui: &Ui, p: &Palette, rect: Rect, offset: f32, content_h: f32, view_h: f32, step: f32) -> Option<f32> {
    let max_off = content_h - view_h;
    if max_off <= 1.0 {
        return None;
    }
    const ARROW: f32 = 16.0;
    let id = ui.id().with("grid-scrollbar");
    let up = Rect::from_min_size(rect.min, vec2(rect.width(), ARROW));
    let down = Rect::from_min_max(pos2(rect.min.x, rect.max.y - ARROW), rect.max);
    let track = Rect::from_min_max(pos2(rect.min.x, up.max.y + 2.0), pos2(rect.max.x, down.min.y - 2.0));
    let thumb_h = (track.height() * view_h / content_h).max(28.0).min(track.height());
    let t = (offset / max_off).clamp(0.0, 1.0);
    let thumb = Rect::from_min_size(
        pos2(track.min.x, track.min.y + (track.height() - thumb_h) * t),
        vec2(track.width(), thumb_h),
    );

    let track_r = ui.interact(track, id.with("track"), Sense::click());
    let thumb_r = ui.interact(thumb, id.with("thumb"), Sense::drag());
    let up_r = ui.interact(up, id.with("up"), Sense::click());
    let down_r = ui.interact(down, id.with("down"), Sense::click());

    let mut new = offset;
    if thumb_r.dragged() {
        new += thumb_r.drag_delta().y * max_off / (track.height() - thumb_h).max(1.0);
    }
    if track_r.clicked() {
        if let Some(pos) = track_r.interact_pointer_pos() {
            new += if pos.y < thumb.min.y { -view_h * 0.9 } else { view_h * 0.9 };
        }
    }
    // Arrows: one row per press, then keep scrolling while held.
    let (now, dt, pressed, press_start) =
        ui.input(|i| (i.time, i.stable_dt.min(0.05), i.pointer.primary_pressed(), i.pointer.press_start_time()));
    for (r, dir) in [(&up_r, -1.0), (&down_r, 1.0)] {
        if r.is_pointer_button_down_on() {
            if pressed {
                new += dir * step;
            } else if now - press_start.unwrap_or(now) > 0.35 {
                new += dir * step * 8.0 * dt;
                ui.ctx().request_repaint();
            }
        }
    }

    let painter = ui.painter();
    let active = thumb_r.dragged() || thumb_r.hovered();
    let w = if active || track_r.hovered() { 8.0 } else { 6.0 };
    let bar = Rect::from_center_size(thumb.center(), vec2(w, thumb_h - 2.0));
    let color = if thumb_r.dragged() {
        p.label2
    } else if active {
        p.scroll_hover
    } else {
        p.scroll
    };
    let radius = if p.r_ctl == 0 { 0 } else { (w / 2.0) as u8 };
    painter.rect_filled(bar, CornerRadius::same(radius), color);
    for (r, resp, upward) in [(up, &up_r, true), (down, &down_r, false)] {
        let c = r.center();
        let s = 3.5;
        let color = if resp.hovered() { p.label } else { p.scroll_hover };
        let (tip, base) = if upward { (-s * 0.6, s * 0.6) } else { (s * 0.6, -s * 0.6) };
        let points = vec![c + vec2(-s, base), c + vec2(s, base), c + vec2(0.0, tip)];
        painter.add(Shape::convex_polygon(points, color, Stroke::NONE));
    }

    let new = new.clamp(0.0, max_off);
    ((new - offset).abs() > 0.01).then_some(new)
}

/// The order to read photos ahead in: the one at `anchor`, then two after it for every
/// one before it, outward until the set runs out.
fn read_ahead_order(set: &[usize], anchor: usize) -> Vec<usize> {
    let Some(last) = set.len().checked_sub(1) else { return Vec::new() };
    let here = match set.binary_search(&anchor) {
        Ok(i) | Err(i) => i.min(last),
    };
    let mut order = vec![set[here]];
    let (mut ahead, mut behind) = (here + 1, here);
    while order.len() < set.len() {
        for _ in 0..2 {
            if ahead <= last {
                order.push(set[ahead]);
                ahead += 1;
            }
        }
        if behind > 0 {
            behind -= 1;
            order.push(set[behind]);
        }
    }
    order
}

/// A "✕" centered on `c`, `s` from its middle to each arm's end.
fn x_mark(painter: &egui::Painter, c: Pos2, s: f32, color: Color32) {
    let stroke = Stroke::new(2.0_f32, color);
    painter.line_segment([c + vec2(-s, -s), c + vec2(s, s)], stroke);
    painter.line_segment([c + vec2(-s, s), c + vec2(s, -s)], stroke);
}

/// Four corner brackets: pointing out for "full screen", in for "exit full screen".
fn fullscreen_icon(painter: &egui::Painter, c: Pos2, r: f32, color: Color32, exit: bool) {
    let stroke = Stroke::new(1.8_f32, color);
    let arm = r * 0.55;
    for (sx, sy) in [(-1.0, -1.0), (1.0, -1.0), (-1.0, 1.0), (1.0, 1.0)] {
        let corner = c + vec2(sx * r, sy * r);
        // Exiting: the brackets' corners point at the middle.
        let (dx, dy) = if exit { (sx, sy) } else { (-sx, -sy) };
        let tip = if exit { c + vec2(sx * (r - arm), sy * (r - arm)) } else { corner };
        painter.line_segment([tip, tip + vec2(dx * arm, 0.0)], stroke);
        painter.line_segment([tip, tip + vec2(0.0, dy * arm)], stroke);
    }
}

/// A "play" triangle centered on `c`, `r` from its middle to each corner, nudged right
/// so it looks centered.
fn play_triangle(painter: &egui::Painter, c: Pos2, r: f32, color: Color32) {
    let c = c + vec2(r * 0.2, 0.0);
    let points = vec![c + vec2(-r * 0.75, -r), c + vec2(-r * 0.75, r), c + vec2(r, 0.0)];
    painter.add(egui::Shape::convex_polygon(points, color, Stroke::NONE));
}

/// A small "!" in a filled circle, shown beside Settings when there's an update.
fn update_button(ui: &mut Ui, p: &Palette) -> Response {
    let (rect, _) = ui.allocate_exact_size(vec2(30.0, 32.0), Sense::hover());
    let resp = ui.interact(rect, Id::new("update-button"), Sense::click());
    let painter = ui.painter();
    if resp.hovered() {
        painter.rect_filled(rect, p.r_ctl, p.hover);
    }
    let c = rect.center();
    painter.circle_filled(c, 9.0, p.accent);
    // Drawn rather than typed: a font's "!" doesn't sit in the middle of its line. The
    // bar and dot together span 5 points either side of the center.
    let bar = Rect::from_min_max(c + vec2(-1.15, -5.0), c + vec2(1.15, 1.0));
    painter.rect_filled(bar, 1.15, p.on_accent);
    painter.circle_filled(c + vec2(0.0, 3.7), 1.3, p.on_accent);
    resp.on_hover_cursor(CursorIcon::PointingHand)
}

/// Round icon button with a gear drawn in it, for Settings.
fn cog_button(ui: &mut Ui, p: &Palette, open: bool) -> Response {
    let (rect, resp) = ui.allocate_exact_size(vec2(36.0, 32.0), Sense::click());
    let painter = ui.painter();
    if open || resp.hovered() {
        let fill = if open { p.control } else { p.hover };
        painter.rect_filled(rect, p.r_ctl, fill);
        if let (Some(border), true) = (p.hover_border, resp.hovered()) {
            painter.rect_stroke(rect, p.r_ctl, Stroke::new(1.0_f32, border), StrokeKind::Inside);
        }
    }
    let color = if open || resp.hovered() { p.label } else { p.label2 };
    let c = rect.center();
    // Eight teeth, a ring, and a hole.
    for i in 0..8 {
        let a = i as f32 * std::f32::consts::TAU / 8.0;
        let dir = vec2(a.cos(), a.sin());
        painter.line_segment([c + dir * 5.5, c + dir * 8.5], Stroke::new(3.0_f32, color));
    }
    painter.circle_stroke(c, 5.5, Stroke::new(2.2_f32, color));
    painter.circle_filled(c, 2.0, color);
    resp.on_hover_cursor(CursorIcon::PointingHand)
}

/// Smaller text for a keyboard shortcut next to a button label. Centered on the
/// label's middle; egui's default lines it up along the bottom, so it sat too low.
fn key_hint(size: f32, color: Color32) -> TextFormat {
    TextFormat { font_id: theme::font(size), color, valign: Align::Center, ..Default::default() }
}

/// The big Pick button (grid and viewer). Filled blue once picked.
fn pick_button(ui: &mut Ui, p: &Palette, label: &str, picked: bool, enabled: bool) -> Response {
    let (fill, color) = if picked { (p.accent, p.on_accent) } else { (p.control, p.tint) };
    let mut job = LayoutJob::default();
    job.append(&format!("✔  {label}"), 0.0, TextFormat { font_id: theme::font_semibold(15.0), color, ..Default::default() });
    let key_color = if picked { p.on_accent.gamma_multiply(0.7) } else { p.label2 };
    job.append("   P", 0.0, key_hint(13.0, key_color));
    let button = Button::new(job)
        .fill(fill)
        .stroke(Stroke::new(1.0_f32, if picked { p.accent } else { p.control_border }))
        .corner_radius(p.r_pill)
        .min_size(vec2(PICK_W, 44.0));
    ui.add_enabled(enabled, button)
}

fn chevron_button(ui: &mut Ui, p: &Palette, left: bool) -> Response {
    let (rect, resp) = ui.allocate_exact_size(Vec2::splat(44.0), Sense::click());
    let painter = ui.painter();
    let fill = if resp.is_pointer_button_down_on() {
        theme::shade(p, p.control, 24)
    } else if resp.hovered() {
        theme::shade(p, p.control, 12)
    } else {
        p.control
    };
    // Round for Apple themes (radius clamps to a circle), rounded-square or square for Windows.
    let border = match (p.hover_border, resp.hovered()) {
        (Some(b), true) => b,
        _ => p.control_border,
    };
    painter.rect_filled(rect, p.r_pill, fill);
    painter.rect_stroke(rect, p.r_pill, Stroke::new(1.0_f32, border), StrokeKind::Inside);
    chevron(painter, rect.center(), 8.0, left, 2.4, p.label);
    resp.on_hover_cursor(CursorIcon::PointingHand)
}

/// "‹ Folder" back button for the top bar, with a large chevron.
fn back_button(ui: &mut Ui, p: &Palette, text: &str) -> Response {
    let galley = ui.painter().layout_no_wrap(text.to_owned(), theme::font(15.0), p.tint);
    let size = vec2(30.0 + galley.size().x + 10.0, 34.0);
    let (rect, resp) = ui.allocate_exact_size(size, Sense::click());
    let painter = ui.painter();
    if resp.hovered() {
        painter.rect_filled(rect, p.r_ctl, p.hover);
    }
    chevron(painter, pos2(rect.min.x + 14.0, rect.center().y), 9.0, true, 2.6, p.tint);
    painter.galley(pos2(rect.min.x + 28.0, rect.center().y - galley.size().y / 2.0), galley, p.tint);
    resp.on_hover_cursor(CursorIcon::PointingHand)
}

/// Small/large photo icon on either side of the thumbnail-size slider.
fn size_icon(ui: &mut Ui, p: &Palette, s: f32) {
    let (rect, _) = ui.allocate_exact_size(vec2(18.0, 18.0), Sense::hover());
    let r = Rect::from_center_size(rect.center(), vec2(s, s * 0.75));
    ui.painter().rect_stroke(r, 2, Stroke::new(1.4_f32, p.label2), StrokeKind::Inside);
}

fn section_label(ui: &mut Ui, p: &Palette, text: &str) {
    // `horizontal` sizes to its content; a centered layout here would swallow the
    // remaining height and push the rows below far down the window.
    ui.horizontal(|ui| {
        ui.add_space(12.0);
        ui.label(RichText::new(text).font(theme::font_semibold(12.0)).color(p.label2));
    });
}

/// A full-width clickable row in a rounded card, like System Settings.
fn list_row(ui: &mut Ui, p: &Palette, title: &str, sub: Option<&str>, trailing: Option<&str>) -> Response {
    let h = if sub.is_some() { 52.0 } else { 40.0 };
    let (rect, resp) = ui.allocate_exact_size(vec2(ui.available_width(), h), Sense::click());
    let painter = ui.painter();
    let fill = if resp.hovered() { theme::shade(p, p.card, 10) } else { p.card };
    painter.rect_filled(rect, p.r_card, fill);
    let x = rect.min.x + 14.0;
    match sub {
        Some(sub) => {
            painter.text(pos2(x, rect.center().y - 9.0), Align2::LEFT_CENTER, title, theme::font_semibold(13.5), p.label);
            painter.text(pos2(x, rect.center().y + 10.0), Align2::LEFT_CENTER, sub, theme::font(12.5), p.label2);
        }
        None => {
            painter.text(pos2(x, rect.center().y), Align2::LEFT_CENTER, title, theme::font(13.5), p.label);
        }
    }
    painter.text(
        pos2(rect.max.x - 14.0, rect.center().y),
        Align2::RIGHT_CENTER,
        trailing.unwrap_or("›"),
        theme::font(16.0),
        p.label2,
    );
    resp.on_hover_cursor(CursorIcon::PointingHand)
}

#[cfg(test)]
mod tests {
    use super::*;
    use eframe::egui::{Event, PointerButton, RawInput};

    /// The real app showing a grid of fake shots (their files don't exist, so
    /// thumbnails stay blank, which doesn't matter for clicking).
    struct Harness {
        ctx: egui::Context,
        app: App,
        time: f64,
    }

    impl Harness {
        fn new() -> Self {
            let ctx = egui::Context::default();
            let mut app = App::with_context(&ctx);
            app.folder = Some(PathBuf::from(r"Z:\cullery-test"));
            app.shots = (0..60)
                .map(|i| Shot {
                    name: format!("IMG_{i:04}"),
                    key: format!("img_{i:04}"),
                    image: Some(PathBuf::from(format!(r"Z:\cullery-test\IMG_{i:04}.JPG"))),
                    raw: Some(PathBuf::from(format!(r"Z:\cullery-test\IMG_{i:04}.CR3"))),
                    ..Default::default()
                })
                .collect();
            app.picked.clear();
            app.settings.show_raw = false;
            app.settings.tile_width = 190.0;
            let mut h = Self { ctx, app, time: 0.0 };
            for _ in 0..3 {
                h.frame(vec![], 0.016);
            }
            h
        }

        /// Runs one frame with these input events, then lets `dt` seconds pass.
        fn frame(&mut self, events: Vec<Event>, dt: f64) {
            let input = RawInput {
                screen_rect: Some(Rect::from_min_size(Pos2::ZERO, vec2(1280.0, 800.0))),
                time: Some(self.time),
                events,
                ..Default::default()
            };
            let app = &mut self.app;
            let _ = self.ctx.run(input, |ctx| app.frame_ui(ctx));
            self.time += dt;
        }

        fn button(pos: Pos2, pressed: bool) -> Event {
            Event::PointerButton { pos, button: PointerButton::Primary, pressed, modifiers: Default::default() }
        }

        /// A tile's rect, by its position in the grid.
        fn tile(&self, index: usize) -> Rect {
            self.app.drawn_tiles[index].1
        }

        /// Double-clicks at `pos` (each click in its own frames, `gap` seconds apart)
        /// and reports whether the viewer opened.
        fn double_click(&mut self, pos: Pos2, gap: f64) -> bool {
            self.frame(vec![Event::PointerMoved(pos)], 0.016);
            self.frame(vec![Self::button(pos, true)], 0.06);
            self.frame(vec![Self::button(pos, false)], gap);
            self.frame(vec![Self::button(pos, true)], 0.06);
            self.frame(vec![Self::button(pos, false)], 0.016);
            self.frame(vec![], 0.016);
            self.app.screen == Screen::Viewer
        }
    }

    fn thumb_center(tile: Rect) -> Pos2 {
        pos2(tile.center().x, tile.min.y + (tile.height() - NAME_AREA) / 2.0)
    }

    fn name_point(tile: Rect) -> Pos2 {
        pos2(tile.center().x, tile.max.y - NAME_AREA + 16.0)
    }

    fn with_thumbnails(h: &mut Harness) {
        for shot in 0..h.app.shots.len() {
            let image = egui::ColorImage::new([400, 267], vec![Color32::GRAY; 400 * 267]);
            let tex = h.ctx.load_texture(format!("t{shot}"), image, TextureOptions::LINEAR);
            h.app.thumbs.insert(shot, Thumb { tex, kind: Kind::Thumb, last_used: 0 });
        }
        h.frame(vec![], 0.016);
    }

    /// Normal-speed double-clicks open a photo from its thumbnail or its name,
    /// whether or not thumbnails have loaded. (Before the fix, anything slower than
    /// about 0.25 s between clicks was missed.)
    #[test]
    fn double_click_opens_from_thumbnail_and_name() {
        let window = system_double_click_time();
        for gap in [0.1, 0.25, 0.35, window - 0.12] {
            for loaded in [false, true] {
                for (spot, point) in [("thumbnail", thumb_center as fn(Rect) -> Pos2), ("name", name_point)] {
                    for index in [0, 7, 13] {
                        let mut h = Harness::new();
                        if loaded {
                            with_thumbnails(&mut h);
                        }
                        let pos = point(h.tile(index));
                        assert!(h.double_click(pos, gap), "{spot} of tile {index}, {gap}s apart, thumbnails {loaded}");
                    }
                }
            }
        }
    }

    /// Clicking once to select and then double-clicking still opens the photo.
    #[test]
    fn click_then_double_click_opens() {
        for pause in [0.3, 0.7, 1.2] {
            let mut h = Harness::new();
            let pos = thumb_center(h.tile(7));
            h.frame(vec![Event::PointerMoved(pos)], 0.016);
            h.frame(vec![Harness::button(pos, true)], 0.06);
            h.frame(vec![Harness::button(pos, false)], pause);
            assert!(h.double_click(pos, 0.2), "pause {pause}");
        }
    }

    /// View Picked: only picked photos in the grid, and the viewer's arrows skip the rest.
    #[test]
    fn view_picked_shows_and_steps_through_picks_only() {
        let mut h = Harness::new();
        h.app.picked = [2, 5, 9].into_iter().collect();
        h.app.set_view_picked(true);
        h.frame(vec![], 0.016);
        let shown: Vec<usize> = h.app.drawn_tiles.iter().map(|(it, _)| it.shot).collect();
        assert_eq!(shown, vec![2, 5, 9]);

        h.app.open_viewer(5);
        h.app.step(1);
        assert_eq!(h.app.current, 9);
        h.app.step(1);
        assert_eq!(h.app.current, 9, "stops at the last pick");
        h.app.step(-2);
        assert_eq!(h.app.current, 2);
        assert_eq!(h.app.position_label(), "1 of 3");

        // Unpicked while viewing: the arrows carry on from where it was.
        h.app.current = 5;
        h.app.picked.remove(&5);
        h.app.step(1);
        assert_eq!(h.app.current, 9);
    }

    /// Drags send the chosen file type, from the export folder when a copy is there.
    #[test]
    fn drag_paths_follow_setting_and_prefer_export_copies() {
        let (mut h, dir) = harness_with_files("drag-paths");
        let export = dir.join("export");
        std::fs::create_dir_all(&export).unwrap();
        // A copy of IMG_0003.CR3 already exported (same size as the original).
        std::fs::write(export.join("IMG_0003.CR3"), b"x").unwrap();
        h.app.settings.export_dir = Some(export.clone());

        h.app.settings.drag_files = DragFiles::Raw;
        let paths = h.app.drag_paths(&[3, 4]);
        assert_eq!(paths, vec![export.join("IMG_0003.CR3"), dir.join("IMG_0004.CR3")]);

        h.app.settings.drag_files = DragFiles::Jpg;
        assert_eq!(h.app.drag_paths(&[4]), vec![dir.join("IMG_0004.JPG")]);

        h.app.settings.drag_files = DragFiles::Both;
        assert_eq!(h.app.drag_paths(&[4]).len(), 2);

        // A shot with no RAW sends its JPG even in RAW mode.
        h.app.shots[4].raw = None;
        h.app.settings.drag_files = DragFiles::Raw;
        assert_eq!(h.app.drag_paths(&[4]), vec![dir.join("IMG_0004.JPG")]);
        cleanup(&dir);
    }

    /// Show Paired RAW is offered only when a photo has a RAW partner, and turning it
    /// off still leaves RAWs that have no partner on the grid.
    #[test]
    fn show_paired_raw_only_affects_pairs() {
        let shot = |image: Option<&str>, raw: Option<&str>| Shot {
            image: image.map(Into::into),
            raw: raw.map(Into::into),
            ..Default::default()
        };
        let pair = shot(Some("a.JPG"), Some("a.CR3"));
        let image = shot(Some("b.PNG"), None);
        let lone_raw = shot(None, Some("c.CR3"));
        assert!(has_raw_pairs(&[pair.clone(), image.clone()]));
        assert!(!has_raw_pairs(&[image.clone(), lone_raw.clone()]));
        assert!(!has_raw_pairs(&[]));

        let mut h = Harness::new();
        h.app.shots = vec![pair, image, lone_raw];
        let tiles = |h: &Harness| h.app.items().iter().map(|it| (it.shot, it.raw)).collect::<Vec<_>>();
        h.app.settings.show_raw = false;
        assert_eq!(tiles(&h), [(0, false), (1, false), (2, true)]);
        h.app.settings.show_raw = true;
        assert_eq!(tiles(&h), [(0, true), (0, false), (1, false), (2, true)]);
    }

    /// Changing the sort keeps picks and the selection on the same photos, and ends a
    /// range, whose neighbours have changed.
    #[test]
    fn sorting_keeps_marks_with_their_photos() {
        let (mut h, dir) = harness_with_files("sort");
        std::fs::write(dir.join("IMG_0002.JPG"), b"a much longer file").unwrap();
        h.app.shots = library::scan(&dir).unwrap();
        h.app.picked = [2].into_iter().collect();
        h.app.selected = [2].into_iter().collect();
        h.app.current = 2;
        h.app.range = Some((1, 3));
        h.app.settings.sort_by = SortBy::Size;
        h.app.settings.sort_descending = true;
        h.app.resort();

        assert_eq!(h.app.shots[0].name, "IMG_0002");
        assert_eq!(h.app.picked, [0].into_iter().collect());
        assert_eq!(h.app.selected, [0].into_iter().collect());
        assert_eq!(h.app.current, 0);
        assert_eq!(h.app.range, None);
        h.frame(vec![], 0.016);
        cleanup(&dir);
    }

    /// A photo named on the command line opens in the viewer, matched by file name
    /// whether it's the JPG or the RAW, in any case.
    #[test]
    fn opens_the_viewer_at_a_named_photo() {
        let mut h = Harness::new();
        h.app.show_file(Path::new(r"Z:\cullery-test\img_0007.cr3"));
        assert!(h.app.screen == Screen::Viewer);
        assert_eq!(h.app.current, 7);

        let mut h = Harness::new();
        h.app.show_file(Path::new(r"Z:\cullery-test\notes.txt"));
        assert!(h.app.screen == Screen::Grid);
        assert!(h.app.message.as_ref().is_some_and(|(text, error)| *error && text.contains("notes.txt")));
    }

    /// Leaving the viewer selects the photo only if it's still on the grid: after
    /// deleting the last photo, or unpicking it in View Picked, nothing is selected.
    #[test]
    fn leaving_the_viewer_never_selects_a_missing_photo() {
        let mut h = Harness::new();
        h.app.open_viewer(3);
        h.app.back_to_grid();
        assert_eq!(h.app.selected, [3].into_iter().collect());

        h.app.open_viewer(3);
        h.app.view_picked = true;
        h.app.back_to_grid();
        assert!(h.app.selected.is_empty() && h.app.select_anchor.is_none());

        h.app.view_picked = false;
        h.app.open_viewer(0);
        h.app.shots.clear();
        h.app.back_to_grid();
        assert!(h.app.selected.is_empty());
        h.frame(vec![], 0.016);
    }

    /// A different card at the same location, with the same file names, doesn't inherit
    /// the first card's picks, and they're still there when that card comes back.
    #[test]
    fn picks_stay_with_their_card() {
        let (mut h, dir) = harness_with_files("picks-card");
        h.app.settings.export_on_pick = false;
        h.app.reload_picks();
        h.app.selected = [1].into_iter().collect();
        h.app.pick_selected();
        assert_eq!(h.app.picked, [1].into_iter().collect());

        // "Another card": every photo has a different size.
        let card_a = std::mem::take(&mut h.app.shots);
        h.app.shots = card_a.iter().cloned().map(|s| Shot { size: s.size + 100, ..s }).collect();
        h.app.reload_picks();
        assert!(h.app.picked.is_empty(), "nothing is picked on the other card");
        h.app.selected = [2].into_iter().collect();
        h.app.pick_selected();

        h.app.shots = card_a;
        h.app.reload_picks();
        assert_eq!(h.app.picked, [1].into_iter().collect(), "the first card's pick came back");
        cleanup(&dir);
    }

    #[cfg(windows)]
    #[test]
    fn long_paths_are_shortened() {
        assert_eq!(short_path(Path::new(r"C:\Users\name\Pictures\Shoot\Wedding")), r"C:\Users\...\Wedding");
        assert_eq!(short_path(Path::new(r"F:\DCIM\100CANON")), r"F:\DCIM\100CANON");
        assert_eq!(short_path(Path::new(r"D:\Photos\2026\June")), r"D:\Photos\2026\June");
        assert_eq!(short_path(Path::new(r"F:\")), r"F:\");
    }

    #[test]
    fn reads_two_ahead_for_every_one_behind() {
        let set: Vec<usize> = (0..10).collect();
        assert_eq!(read_ahead_order(&set, 4), [4, 5, 6, 3, 7, 8, 2, 9, 1, 0]);
        assert_eq!(read_ahead_order(&set, 9), [9, 8, 7, 6, 5, 4, 3, 2, 1, 0]);
        assert_eq!(read_ahead_order(&[2, 5, 9], 6), [9, 5, 2], "a photo outside the set starts at the next one");
        assert!(read_ahead_order(&[], 0).is_empty());
    }

    /// Rejecting marks photos to delete later: never picked at the same time, shown
    /// together by View Rejected, and remembered.
    #[test]
    fn rejects_and_view_rejected() {
        let (mut h, dir) = harness_with_files("rejects");
        h.app.settings.export_on_pick = false;
        h.app.reload_picks();
        h.app.selected = [1].into_iter().collect();
        h.app.pick_selected();

        h.app.selected = [1, 2].into_iter().collect();
        h.app.reject_selected();
        assert_eq!(h.app.rejected, [1, 2].into_iter().collect());
        assert!(h.app.picked.is_empty(), "a rejected photo isn't picked");

        h.app.set_view_rejected(true);
        assert_eq!(h.app.working_set(), vec![1, 2]);
        h.app.set_view_picked(true);
        assert!(!h.app.view_rejected, "only one view at a time");
        h.app.set_view_picked(false);

        h.app.selected = [2].into_iter().collect();
        h.app.pick_selected();
        assert_eq!(h.app.rejected, [1].into_iter().collect(), "picking undoes a reject");

        h.app.reload_picks();
        assert_eq!(h.app.rejected, [1].into_iter().collect(), "rejects are remembered");
        if let Some(f) = picks::file_for(&dir) {
            let _ = std::fs::remove_file(f.parent().unwrap().parent().unwrap().join("rejects").join(f.file_name().unwrap()));
        }
        cleanup(&dir);
    }

    /// The viewer's strip: clicking a photo in it shows that photo, and its tab hides
    /// and shows the strip.
    #[test]
    fn viewer_strip_jumps_and_hides() {
        let mut h = Harness::new();
        h.app.settings.filmstrip = true;
        h.app.open_viewer(10);
        for _ in 0..20 {
            h.frame(vec![], 0.05);
        }
        let cell = h.ctx.read_response(Id::new(("strip", 12usize))).expect("photo 12 is in the strip").rect.center();
        h.frame(vec![Event::PointerMoved(cell)], 0.016);
        h.frame(vec![Harness::button(cell, true)], 0.05);
        h.frame(vec![Harness::button(cell, false)], 0.016);
        assert_eq!(h.app.current, 12);

        let tab = h.ctx.read_response(Id::new("strip-tab")).unwrap().rect.center();
        h.frame(vec![Event::PointerMoved(tab)], 0.016);
        h.frame(vec![Harness::button(tab, true)], 0.05);
        h.frame(vec![Harness::button(tab, false)], 0.016);
        h.frame(vec![], 0.016);
        h.frame(vec![], 0.016);
        assert!(!h.app.settings.filmstrip, "the tab hid the strip");
        assert!(h.ctx.read_response(Id::new(("strip", 12usize))).is_none());
        let hidden_tab = h.ctx.read_response(Id::new("strip-tab")).unwrap().rect;
        assert!(hidden_tab.min.y > tab.y, "the tab moved down to the bar");
    }

    /// F fills the screen in the viewer; Esc leaves full screen first, then the viewer.
    #[test]
    fn full_screen_and_back() {
        let mut h = Harness::new();
        h.app.settings.keybinds = keybinds::Keybinds::default();
        let key = |k: KeyCode| Event::Key { key: k, physical_key: None, pressed: true, repeat: false, modifiers: Default::default() };
        h.app.open_viewer(3);
        h.frame(vec![], 0.016);
        h.frame(vec![key(KeyCode::F)], 0.016);
        assert!(h.app.fullscreen);
        h.frame(vec![key(KeyCode::Escape)], 0.016);
        assert!(!h.app.fullscreen && h.app.screen == Screen::Viewer, "Esc left full screen only");
        h.frame(vec![key(KeyCode::F)], 0.016);
        h.app.back_to_grid();
        h.frame(vec![], 0.016);
        assert!(!h.app.fullscreen, "leaving the viewer leaves full screen");
    }

    /// A folder with photos and videos shows photos first; the switch swaps them, and
    /// each kind keeps its own picks.
    #[test]
    fn photos_and_videos_switch() {
        let (mut h, dir) = harness_with_files("media");
        h.app.settings.export_on_pick = false;
        std::fs::write(dir.join("A001.MP4"), b"x").unwrap();
        std::fs::write(dir.join("A001.CRM"), b"x").unwrap();
        let ctx = h.ctx.clone();
        h.app.finish_open(&ctx, dir.clone(), library::scan(&dir));
        assert_eq!(h.app.media, Media::Photos);
        assert_eq!((h.app.shots.len(), h.app.other_shots.len()), (6, 1));

        h.app.selected = [2].into_iter().collect();
        h.app.pick_selected();
        h.app.switch_media(Media::Videos);
        assert_eq!(h.app.shots.len(), 1);
        assert!(h.app.shots[0].video && h.app.shots[0].raw.is_some(), "the clip is paired with its RAW");
        assert!(h.app.picked.is_empty() && h.app.selected.is_empty());
        h.app.selected = [0].into_iter().collect();
        h.app.pick_selected();

        h.app.switch_media(Media::Photos);
        assert_eq!(h.app.picked, [2].into_iter().collect(), "the photo pick is still there");
        h.app.switch_media(Media::Videos);
        assert_eq!(h.app.picked, [0].into_iter().collect(), "and so is the video pick");
        cleanup(&dir);
    }

    /// Saved picks from before sizes were recorded still count.
    #[test]
    fn picks_from_older_versions_still_load() {
        let (mut h, dir) = harness_with_files("picks-old");
        picks::save(&dir, [("img_0002".to_string(), None)].into_iter());
        h.app.reload_picks();
        assert_eq!(h.app.picked, [2].into_iter().collect());
        cleanup(&dir);
    }

    /// Clicking a shortcut waits for a key, and that key then does the job. Esc cancels,
    /// and Restore Defaults' reset puts the old key back.
    #[test]
    fn a_rebound_key_takes_over() {
        let mut h = Harness::new();
        let key = |k: KeyCode| Event::Key { key: k, physical_key: None, pressed: true, repeat: false, modifiers: Default::default() };
        h.app.settings.export_on_pick = false;
        h.app.settings.keybinds = keybinds::Keybinds::default();
        h.app.selected = [3].into_iter().collect();
        h.app.settings_page = Some(SettingsPage::Keybinds);
        h.frame(vec![], 0.016);

        h.app.rebinding = Some(KeyAction::Pick);
        h.frame(vec![key(KeyCode::Escape)], 0.016);
        assert!(h.app.rebinding.is_none());
        assert!(h.app.settings_page == Some(SettingsPage::Keybinds), "Esc only cancelled the wait");
        assert_eq!(h.app.settings.keybinds, keybinds::Keybinds::default());

        h.app.rebinding = Some(KeyAction::Pick);
        h.frame(vec![key(KeyCode::X)], 0.016);
        assert!(h.app.rebinding.is_none());
        h.app.settings_page = None;
        h.frame(vec![], 0.016);

        h.frame(vec![key(KeyCode::P)], 0.016);
        assert!(h.app.picked.is_empty(), "the old key does nothing now");
        h.frame(vec![key(KeyCode::X)], 0.016);
        assert_eq!(h.app.picked, [3].into_iter().collect());
    }

    /// Opening a drive searches in the background and ends up with the photos in its
    /// subfolders.
    #[test]
    fn a_drive_search_opens_its_subfolders() {
        let mut h = Harness::new();
        let dir = std::env::temp_dir().join(format!("cullery-drive-open-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(dir.join("100CANON")).unwrap();
        std::fs::write(dir.join("100CANON").join("IMG_1.JPG"), b"x").unwrap();
        std::fs::write(dir.join("top.png"), b"x").unwrap();

        h.app.start_drive_scan(dir.clone(), None, false);
        for _ in 0..500 {
            h.frame(vec![], 0.016);
            if h.app.drive_scan.is_none() {
                break;
            }
            std::thread::sleep(std::time::Duration::from_millis(5));
        }
        assert!(h.app.drive_scan.is_none(), "the search finished");
        assert_eq!(h.app.folder.as_deref(), Some(dir.as_path()));
        assert_eq!(h.app.shots.len(), 2);
        if let Some(f) = picks::file_for(&dir) {
            let _ = std::fs::remove_file(f);
        }
        std::fs::remove_dir_all(&dir).unwrap();
    }

    /// The "!" only shows with an update waiting. Clicking it opens the popup; closing
    /// the popup leaves the "!" there.
    #[test]
    fn update_button_opens_the_popup() {
        let mut h = Harness::new();
        let button = Id::new("update-button");
        assert!(h.ctx.read_response(button).is_none());

        *h.app.new_version.lock().unwrap() = Some("1.2.0".into());
        h.frame(vec![], 0.016);
        h.frame(vec![], 0.016);
        let pos = h.ctx.read_response(button).expect("the button is drawn").rect.center();
        assert!(!h.app.update_prompt, "the popup waits for a click");
        h.frame(vec![Event::PointerMoved(pos)], 0.016);
        h.frame(vec![Harness::button(pos, true)], 0.06);
        h.frame(vec![Harness::button(pos, false)], 0.016);
        h.frame(vec![], 0.016);
        assert!(h.app.update_prompt);

        let esc = Event::Key { key: KeyCode::Escape, physical_key: None, pressed: true, repeat: false, modifiers: Default::default() };
        h.frame(vec![esc], 0.016);
        h.frame(vec![], 0.016);
        assert!(!h.app.update_prompt);
        assert!(h.ctx.read_response(button).is_some());
    }

    /// Opening another folder drops everything that referred to the last one,
    /// including a delete confirmation (its photo numbers would point at new photos).
    #[test]
    fn a_new_folder_clears_the_old_view() {
        let mut h = Harness::new();
        h.app.confirm_delete = Some(vec![1, 2]);
        h.app.view_picked = true;
        h.app.selected = [4].into_iter().collect();
        h.app.reset_view();
        assert!(h.app.confirm_delete.is_none() && !h.app.view_picked && h.app.selected.is_empty());
        assert_eq!(h.app.pending_scroll, Some(0.0));
    }

    /// Choosing a range drops selected photos outside it, so they can't be acted on
    /// while hidden.
    #[test]
    fn a_range_keeps_only_the_selection_inside_it() {
        let mut h = Harness::new();
        h.app.selected = [2, 10, 40].into_iter().collect();
        h.app.finish_range(20, 5);
        assert_eq!(h.app.range, Some((5, 20)));
        assert_eq!(h.app.selected, [10].into_iter().collect());
    }

    /// Pressing on a photo and moving starts a drag of it (selecting it, like
    /// Explorer); a selected photo drags the whole selection along.
    #[test]
    fn pressing_and_moving_starts_a_drag() {
        let mut h = Harness::new();
        h.app.selected = [1, 2].into_iter().collect();
        let pos = thumb_center(h.tile(7));
        h.frame(vec![Event::PointerMoved(pos)], 0.016);
        h.frame(vec![Harness::button(pos, true)], 0.016);
        for step in 1..=4 {
            h.frame(vec![Event::PointerMoved(pos + vec2(8.0 * step as f32, 0.0))], 0.016);
        }
        // Outside a real window the drag itself can't start, but the selection shows
        // which photos it would carry: the one under the pointer, as it wasn't selected.
        assert_eq!(h.app.selected, [h.app.drawn_tiles[7].0.shot].into_iter().collect());
        assert!(h.app.screen == Screen::Grid);
        // The overlay showed, then the drag was handed off (here: dropped, as there's no
        // window), so the overlay is gone.
        assert!(h.app.pending_drag.is_none());
    }

    /// The drag-out overlay's text: overlay drawn for one frame before the drag starts,
    /// with the photo count and which files go.
    #[test]
    fn drag_out_overlay_says_what_is_sent() {
        let mut h = Harness::new();
        let pos = thumb_center(h.tile(2));
        h.frame(vec![Event::PointerMoved(pos)], 0.016);
        h.frame(vec![Harness::button(pos, true)], 0.016);
        let mut seen = None;
        for step in 1..=4 {
            h.frame(vec![Event::PointerMoved(pos + vec2(8.0 * step as f32, 0.0))], 0.016);
            if let Some(d) = &h.app.pending_drag {
                seen = Some((d.title.clone(), d.count.clone(), d.shown));
            }
        }
        let (title, count, shown) = seen.expect("the overlay appears when the drag starts");
        assert!(title.contains("Lightroom"));
        assert_eq!(count, "1 photo");
        assert!(shown);

        let both = Shot { name: "A".into(), key: "a".into(), image: Some("A.JPG".into()), raw: Some("A.CR3".into()), ..Default::default() };
        let jpg_only = Shot { name: "B".into(), key: "b".into(), image: Some("B.JPG".into()), ..Default::default() };
        let png = Shot { name: "C".into(), key: "c.png".into(), image: Some("C.PNG".into()), ..Default::default() };
        assert_eq!(drag_out_hint(&[&png], DragFiles::Raw).2, "No RAW files, so the PNGs will be sent");
        assert_eq!(drag_out_hint(&[&png], DragFiles::Both).2, "The PNG files will be sent");
        assert_eq!(drag_out_hint(&[&png, &jpg_only], DragFiles::Jpg).2, "Only the image files will be sent");
        let (_, count, note) = drag_out_hint(&[&both, &both], DragFiles::Raw);
        assert_eq!((count.as_str(), note.as_str()), ("2 photos", "Only the RAW files will be sent"));
        assert!(drag_out_hint(&[&both, &jpg_only], DragFiles::Raw).2.contains("JPGs for photos without one"));
        assert!(drag_out_hint(&[&jpg_only], DragFiles::Raw).2.starts_with("No RAW files"));
        assert_eq!(drag_out_hint(&[&both], DragFiles::Jpg).2, "Only the JPG files will be sent");
        assert_eq!(drag_out_hint(&[&both], DragFiles::Both).2, "JPG + RAW files will be sent");
    }

    /// Only a dropped folder changes the open folder. Dropped files — including photos
    /// just dragged out of Cullery landing back on it — leave it alone.
    #[test]
    fn dropped_files_never_switch_folders() {
        use eframe::egui::DroppedFile;
        let drop = |h: &mut Harness, paths: Vec<PathBuf>| {
            let input = RawInput {
                screen_rect: Some(Rect::from_min_size(Pos2::ZERO, vec2(1280.0, 800.0))),
                time: Some(h.time),
                dropped_files: paths.into_iter().map(|p| DroppedFile { path: Some(p), ..Default::default() }).collect(),
                ..Default::default()
            };
            let app = &mut h.app;
            let _ = h.ctx.run(input, |ctx| app.frame_ui(ctx));
            h.time += 0.016;
        };
        let start = PathBuf::from(r"Z:\cullery-test");
        let export = std::env::temp_dir().join(format!("cullery-drop-test-{}", std::process::id()));
        std::fs::create_dir_all(&export).unwrap();
        let copy = export.join("IMG_0003.CR3");
        std::fs::write(&copy, b"x").unwrap();

        // Our own drag landing back on the window.
        let mut h = Harness::new();
        h.app.last_drag = [copy.clone()].into_iter().collect();
        drop(&mut h, vec![copy.clone()]);
        assert_eq!(h.app.folder.as_ref(), Some(&start));

        // Photo files from elsewhere: a note, no switch.
        h.app.last_drag.clear();
        drop(&mut h, vec![copy.clone()]);
        assert_eq!(h.app.folder.as_ref(), Some(&start));
        assert!(h.app.message.is_some());

        std::fs::remove_dir_all(&export).unwrap();
    }

    /// The drop overlay names a dragged-in folder, explains that photos can't be
    /// opened on their own, and stays hidden for Cullery's own drags.
    #[test]
    fn drop_overlay_messages() {
        let dir = std::env::temp_dir().join(format!("cullery-hint-test-{}", std::process::id())).join("101CANON");
        std::fs::create_dir_all(&dir).unwrap();
        let photo = dir.join("IMG_0001.CR3");
        std::fs::write(&photo, b"x").unwrap();
        let none = HashSet::new();

        assert_eq!(drop_hint(&[], &none), None);
        assert_eq!(drop_hint(&[dir.clone()], &none), Some(("Drop to open 101CANON".to_string(), None)));
        let (title, detail) = drop_hint(&[photo.clone()], &none).unwrap();
        assert_eq!(title, "Drop a folder to open it");
        assert!(detail.unwrap().contains("Individual photos"));
        let own: HashSet<PathBuf> = [photo.clone()].into_iter().collect();
        assert_eq!(drop_hint(&[photo], &own), None);

        std::fs::remove_dir_all(dir.parent().unwrap()).unwrap();
    }

    /// Double-click zoom in the viewer works every time, with no cooldown: zoom in,
    /// out and in again in quick succession. (egui's own detection counted the third
    /// click within about a second as a triple-click and ignored it.)
    #[test]
    fn viewer_double_click_zoom_has_no_cooldown() {
        let mut h = Harness::new();
        h.app.open_viewer(5);
        let image = egui::ColorImage::new([1200, 800], vec![Color32::GRAY; 1200 * 800]);
        let tex = h.ctx.load_texture("full", image, TextureOptions::LINEAR);
        h.app.fulls.insert(5, FullImage { tex, original: vec2(6000.0, 4000.0), frames: Vec::new() });
        h.frame(vec![], 0.016);
        let pos = h.ctx.screen_rect().center();

        let double_click = |h: &mut Harness| {
            h.frame(vec![Event::PointerMoved(pos)], 0.016);
            h.frame(vec![Harness::button(pos, true)], 0.06);
            h.frame(vec![Harness::button(pos, false)], 0.12);
            h.frame(vec![Harness::button(pos, true)], 0.06);
            h.frame(vec![Harness::button(pos, false)], 0.016);
            h.frame(vec![], 0.15);
        };
        double_click(&mut h);
        assert!(h.app.zoom.is_some(), "first double-click zooms in");
        double_click(&mut h);
        assert!(h.app.zoom.is_none(), "second double-click zooms back out");
        double_click(&mut h);
        assert!(h.app.zoom.is_some(), "third double-click zooms in again");
    }

    /// Missing Windows extensions get a specific message; other failures don't.
    #[test]
    fn codec_help_names_the_missing_extension() {
        let failed = format!("{}something", crate::wic::DECODE_FAILED);
        assert_eq!(codec_help(Path::new(r"C:\x\IMG_1.HEIC"), crate::wic::NO_READER), Some(HEIF_EXTENSION));
        assert_eq!(codec_help(Path::new(r"C:\x\IMG_1.heif"), &failed), Some(HEVC_EXTENSION));
        assert_eq!(codec_help(Path::new(r"C:\x\pic.avif"), crate::wic::NO_READER), Some(AV1_EXTENSION));
        assert_eq!(codec_help(Path::new(r"C:\x\IMG_1.JPG"), "bad JPEG"), None);
        assert_eq!(codec_help(Path::new(r"C:\x\IMG_1.HEIC"), "file not found"), None);
        assert_eq!(HEIF_EXTENSION.short, "Needs HEIC reader");
    }

    /// A GIF in the viewer shows the frame due at each moment, looping.
    #[test]
    fn gif_frames_follow_their_timing() {
        let ctx = egui::Context::default();
        let tex = |name: &str| {
            ctx.load_texture(name, egui::ColorImage::new([2, 2], vec![Color32::GRAY; 4]), TextureOptions::LINEAR)
        };
        let full = FullImage {
            tex: tex("still"),
            original: vec2(2.0, 2.0),
            frames: vec![(tex("a"), 0.1), (tex("b"), 0.2), (tex("c"), 0.3)],
        };
        let shown = |t: f64| {
            let (tex, _) = full.frame_at(t);
            full.frames.iter().position(|(f, _)| f.id() == tex.id()).unwrap()
        };
        assert_eq!(shown(0.05), 0);
        assert_eq!(shown(0.15), 1);
        assert_eq!(shown(0.45), 2);
        assert_eq!(shown(0.65), 0, "loops after 0.6 s");
        let (_, wait) = full.frame_at(0.15);
        assert!((wait.unwrap() - 0.15).abs() < 1e-9, "next frame due in 0.15 s");
    }

    /// The mouse back button leaves the viewer, and closes Settings.
    #[test]
    fn mouse_back_button_goes_back() {
        let back = |pressed| Event::PointerButton {
            pos: pos2(600.0, 400.0),
            button: egui::PointerButton::Extra1,
            pressed,
            modifiers: Default::default(),
        };
        let mut h = Harness::new();
        h.app.open_viewer(5);
        h.frame(vec![], 0.016);
        h.frame(vec![back(true)], 0.016);
        h.frame(vec![back(false)], 0.016);
        assert!(h.app.screen == Screen::Grid);

        h.app.settings_page = Some(SettingsPage::Themes);
        h.frame(vec![back(true)], 0.016);
        assert!(h.app.settings_page.is_none());
    }

    /// A real folder of six JPG + CR3 shots, opened in the harness.
    fn harness_with_files(tag: &str) -> (Harness, PathBuf) {
        let dir = std::env::temp_dir().join(format!("cullery-delete-{tag}-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(&dir).unwrap();
        for i in 0..6 {
            std::fs::write(dir.join(format!("IMG_{i:04}.JPG")), b"x").unwrap();
            std::fs::write(dir.join(format!("IMG_{i:04}.CR3")), b"x").unwrap();
        }
        let mut h = Harness::new();
        h.app.folder = Some(dir.clone());
        h.app.shots = library::scan(&dir).unwrap();
        h.app.picked.clear();
        h.app.selected.clear();
        h.app.thumbs.clear();
        h.frame(vec![], 0.016);
        (h, dir)
    }

    fn cleanup(dir: &Path) {
        let _ = std::fs::remove_dir_all(dir);
        if let Some(f) = picks::file_for(dir) {
            let _ = std::fs::remove_file(f);
        }
    }

    /// Runs frames until the background delete has ended.
    fn finish_deleting(h: &mut Harness) {
        for _ in 0..2000 {
            if h.app.deleting.is_none() {
                return;
            }
            h.frame(vec![], 0.016);
            std::thread::sleep(std::time::Duration::from_millis(1));
        }
        panic!("the delete never finished");
    }

    /// While a delete runs the window keeps going, but keys can't start anything else.
    #[test]
    fn keys_do_nothing_while_deleting() {
        let (mut h, dir) = harness_with_files("busy");
        h.app.selected = [0, 1].into_iter().collect();
        let ctx = h.ctx.clone();
        h.app.delete_shots(&ctx, &[4], true);
        let key = Event::Key { key: KeyCode::Delete, physical_key: None, pressed: true, repeat: false, modifiers: Default::default() };
        if h.app.deleting.is_some() {
            // Time passes so the bar is drawn too.
            h.frame(vec![], 0.5);
            h.frame(vec![key], 0.016);
            assert!(h.app.confirm_delete.is_none() || h.app.deleting.is_none());
        }
        finish_deleting(&mut h);
        assert!(!dir.join("IMG_0004.JPG").exists());
        cleanup(&dir);
    }

    /// Deleting removes each shot's JPG and RAW; picks, selection and thumbnails
    /// still point at the right photos afterwards.
    #[test]
    fn delete_removes_both_files_and_keeps_the_rest_in_place() {
        let (mut h, dir) = harness_with_files("grid");
        h.app.picked = [2, 4].into_iter().collect();
        h.app.selected = [1, 2].into_iter().collect();
        let image = egui::ColorImage::new([4, 4], vec![Color32::GRAY; 16]);
        let tex = h.ctx.load_texture("t", image, TextureOptions::LINEAR);
        h.app.thumbs.insert(4, Thumb { tex, kind: Kind::Thumb, last_used: 0 });

        let ctx = h.ctx.clone();
        h.app.delete_shots(&ctx, &[1, 2], true);
        finish_deleting(&mut h);

        let left: Vec<String> = h.app.shots.iter().map(|s| s.name.clone()).collect();
        assert_eq!(left, ["IMG_0000", "IMG_0003", "IMG_0004", "IMG_0005"]);
        for gone in ["IMG_0001.JPG", "IMG_0001.CR3", "IMG_0002.JPG", "IMG_0002.CR3"] {
            assert!(!dir.join(gone).exists(), "{gone} should be deleted");
        }
        assert!(dir.join("IMG_0003.CR3").exists());
        // IMG_0004 (was #4, now #2) is still picked and keeps its thumbnail.
        assert_eq!(h.app.picked, [2].into_iter().collect());
        assert!(h.app.thumbs.contains_key(&2));
        assert!(h.app.selected.is_empty());
        cleanup(&dir);
    }

    /// Deleting in the viewer moves on to the next photo.
    #[test]
    fn delete_in_viewer_moves_to_next_photo() {
        let (mut h, dir) = harness_with_files("viewer");
        h.app.open_viewer(3);
        let ctx = h.ctx.clone();
        h.app.delete_shots(&ctx, &[3], true);
        finish_deleting(&mut h);
        assert!(h.app.screen == Screen::Viewer);
        assert_eq!(h.app.shots[h.app.current].name, "IMG_0004");
        cleanup(&dir);
    }

    /// The confirmation comes first; Esc cancels and nothing is deleted.
    #[test]
    fn delete_asks_first_and_esc_cancels() {
        let (mut h, dir) = harness_with_files("cancel");
        h.app.selected = [0].into_iter().collect();
        let key = |key| Event::Key { key, physical_key: None, pressed: true, repeat: false, modifiers: Default::default() };
        h.frame(vec![key(KeyCode::Delete)], 0.016);
        assert_eq!(h.app.confirm_delete, Some(vec![0]));
        h.frame(vec![], 0.016);
        h.frame(vec![key(KeyCode::Escape)], 0.016);
        h.frame(vec![], 0.016);
        assert!(h.app.confirm_delete.is_none());
        assert!(dir.join("IMG_0000.JPG").exists() && dir.join("IMG_0000.CR3").exists());
        assert!(h.app.selected.contains(&0), "Esc on the dialog doesn't clear the selection");
        cleanup(&dir);
    }

    /// Closing the window mid-export is held back and asked about; Close When Done
    /// closes once the copying ends.
    #[test]
    fn closing_while_exporting_asks_first() {
        let mut h = Harness::new();
        let commands = |h: &mut Harness, close: bool| {
            let mut input = RawInput {
                screen_rect: Some(Rect::from_min_size(Pos2::ZERO, vec2(1280.0, 800.0))),
                time: Some(h.time),
                ..Default::default()
            };
            if close {
                input.viewports.entry(egui::ViewportId::ROOT).or_default().events.push(egui::ViewportEvent::Close);
            }
            let app = &mut h.app;
            let out = h.ctx.run(input, |ctx| app.frame_ui(ctx));
            h.time += 0.016;
            out.viewport_output.get(&egui::ViewportId::ROOT).map(|v| v.commands.clone()).unwrap_or_default()
        };
        let cancelled = |c: &[ViewportCommand]| c.iter().any(|c| matches!(c, ViewportCommand::CancelClose));
        let closed = |c: &[ViewportCommand]| c.iter().any(|c| matches!(c, ViewportCommand::Close));

        // Nothing copying: the close goes through.
        assert!(!cancelled(&commands(&mut h, true)));
        assert!(!h.app.confirm_close);

        h.app.exporter.pretend_busy(180, PathBuf::from(r"E:\Lightroom import"));
        assert!(cancelled(&commands(&mut h, true)));
        assert!(h.app.confirm_close, "asks first");
        commands(&mut h, false);

        h.app.close_when_done = true;
        h.app.confirm_close = false;
        assert!(!closed(&commands(&mut h, false)), "still copying");
        h.app.exporter.pretend_done();
        assert!(closed(&commands(&mut h, false)), "closes when the copying ends");
    }

    /// Runs frames until every background copy has finished.
    fn wait_for_copies(h: &mut Harness) {
        for _ in 0..500 {
            if h.app.exporter.pending() == 0 {
                return;
            }
            std::thread::sleep(std::time::Duration::from_millis(5));
            h.frame(vec![], 0.016);
        }
        panic!("copies didn't finish");
    }

    fn files_in(dir: &Path) -> Vec<String> {
        let mut names: Vec<String> = std::fs::read_dir(dir)
            .map(|d| d.map(|e| e.unwrap().file_name().to_string_lossy().into_owned()).collect())
            .unwrap_or_default();
        names.sort();
        names
    }

    /// Export on Pick: picking copies (JPG + RAW by default); unpicking deletes nothing;
    /// picking again makes no second copy.
    #[test]
    fn export_on_pick_never_duplicates_or_deletes() {
        let (mut h, dir) = harness_with_files("export-on-pick");
        let export = dir.join("export");
        h.app.settings.export_dir = Some(export.clone());
        h.app.settings.export_on_pick = true;
        h.app.settings.export_files = DragFiles::Both;

        h.app.toggle_pick(1);
        wait_for_copies(&mut h);
        assert_eq!(files_in(&export), ["IMG_0001.CR3", "IMG_0001.JPG"]);

        h.app.toggle_pick(1); // unpick
        wait_for_copies(&mut h);
        assert!(!h.app.picked.contains(&1));
        assert_eq!(files_in(&export), ["IMG_0001.CR3", "IMG_0001.JPG"], "unpicking keeps the copies");

        h.app.toggle_pick(1); // pick again
        wait_for_copies(&mut h);
        assert_eq!(files_in(&export), ["IMG_0001.CR3", "IMG_0001.JPG"], "no second copy");
        cleanup(&dir);
    }

    /// With Export on Pick off, picking copies nothing, even with a folder set; the
    /// files choice decides what Export on Pick copies.
    #[test]
    fn export_on_pick_off_and_file_choice() {
        let (mut h, dir) = harness_with_files("export-choice");
        let export = dir.join("export");
        h.app.settings.export_dir = Some(export.clone());
        h.app.settings.export_on_pick = false;
        h.app.toggle_pick(2);
        wait_for_copies(&mut h);
        assert!(h.app.picked.contains(&2));
        assert!(files_in(&export).is_empty(), "nothing copied while off");

        h.app.settings.export_on_pick = true;
        h.app.settings.export_files = DragFiles::Raw;
        h.app.toggle_pick(3);
        wait_for_copies(&mut h);
        assert_eq!(files_in(&export), ["IMG_0003.CR3"]);
        cleanup(&dir);
    }

    /// Picking works with no export folder set: the photo is marked (and remembered)
    /// without asking for a folder or copying anything.
    #[test]
    fn picking_without_export_folder_just_marks() {
        let (mut h, dir) = harness_with_files("pick-no-export");
        h.app.settings.export_dir = None;
        h.app.selected = [1, 3].into_iter().collect();
        h.app.pick_selected();
        assert_eq!(h.app.picked, [1, 3].into_iter().collect());
        assert_eq!(h.app.exporter.pending(), 0, "nothing copied");
        let saved: HashSet<String> = picks::load(&dir).unwrap().into_iter().map(|(name, _)| name).collect();
        assert_eq!(saved, ["img_0001", "img_0003"].map(String::from).into_iter().collect());
        h.app.pick_selected();
        assert!(h.app.picked.is_empty(), "picking again unpicks");
        cleanup(&dir);
    }

    /// Esc closes Settings and nothing else: the grid selection behind it stays.
    #[test]
    fn esc_closes_settings_only() {
        let mut h = Harness::new();
        h.app.selected.insert(3);
        h.app.settings_page = Some(SettingsPage::Themes);
        h.frame(vec![], 0.016);
        let esc = Event::Key {
            key: KeyCode::Escape,
            physical_key: None,
            pressed: true,
            repeat: false,
            modifiers: Default::default(),
        };
        h.frame(vec![esc], 0.016);
        assert!(h.app.settings_page.is_none());
        assert!(h.app.selected.contains(&3));
    }

    /// Slow clicks, or clicks on two different photos, only select.
    #[test]
    fn separate_clicks_do_not_open() {
        let mut h = Harness::new();
        assert!(!h.double_click(thumb_center(h.tile(7)), system_double_click_time() + 0.3));
        assert!(h.app.selected.contains(&h.app.drawn_tiles[7].0.shot));

        let mut h = Harness::new();
        let (a, b) = (thumb_center(h.tile(7)), thumb_center(h.tile(8)));
        h.frame(vec![Event::PointerMoved(a)], 0.016);
        h.frame(vec![Harness::button(a, true)], 0.06);
        h.frame(vec![Harness::button(a, false), Event::PointerMoved(b)], 0.1);
        h.frame(vec![Harness::button(b, true)], 0.06);
        h.frame(vec![Harness::button(b, false)], 0.016);
        h.frame(vec![], 0.016);
        assert!(h.app.screen == Screen::Grid);
    }
}
