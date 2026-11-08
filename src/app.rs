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

use crate::devices::{self, CardFolder};
use crate::export::{Done, Exporter};
use crate::library::{self, Shot};
use crate::loader::{Key, Kind, Loader, Request, THUMB_PX};
use crate::picks;
use crate::settings::Settings;
use crate::theme::{self, Palette};

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
    Themes,
}

/// Shortcuts shown on the Keybinds page: (keys, what they do).
const GRID_KEYS: &[(&str, &str)] = &[
    ("Click", "Select a photo"),
    ("Ctrl + Click", "Add or remove from the selection"),
    ("Shift + Click", "Select everything from the last clicked photo"),
    ("Ctrl + A", "Select all"),
    ("Double-click  /  Enter", "Open the photo"),
    ("P  /  Space", "Pick the selected photos (again to unpick)"),
    ("Ctrl + E", "Quick Export the selected photos"),
    ("Esc", "Clear the selection, or cancel Select Range"),
    ("Ctrl + O", "Open a folder"),
];
const VIEWER_KEYS: &[(&str, &str)] = &[
    ("←  /  →", "Previous / next photo"),
    ("Home  /  End", "First / last photo"),
    ("P  /  Space", "Pick (again to unpick)"),
    ("Ctrl + E", "Quick Export this photo"),
    ("Scroll wheel", "Zoom around the cursor"),
    ("Double-click", "Zoom to that spot, or back to fit"),
    ("Drag", "Move around while zoomed"),
    ("Esc", "Back to the grid"),
];

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

enum Action {
    /// Select (or, as the second of a double-click, open).
    Click(usize),
    TogglePick(usize),
    Mark(usize),
}

struct Thumb {
    tex: TextureHandle,
    kind: Kind,
    last_used: u64,
}

struct FullImage {
    tex: TextureHandle,
    /// The photo's real size in pixels.
    original: Vec2,
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
    range: Option<(usize, usize)>,
    tile_width: i32,
}

pub struct App {
    p: Palette,
    settings: Settings,
    loader: Loader,
    generation: u64,
    exporter: Exporter,
    cards: Arc<Mutex<Vec<CardFolder>>>,

    folder: Option<PathBuf>,
    shots: Vec<Shot>,
    picked: HashSet<usize>,
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
    /// Longest gap between the clicks of a double-click, from Windows' setting.
    double_click_time: f64,
    /// The settings window, and which page it shows.
    settings_page: Option<SettingsPage>,

    thumbs: HashMap<usize, Thumb>,
    fulls: HashMap<usize, FullImage>,
    original: Option<(usize, TextureHandle)>,
    failed: HashSet<Key>,
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
}

impl App {
    pub fn new(cc: &eframe::CreationContext<'_>) -> Self {
        Self::with_context(&cc.egui_ctx)
    }

    fn with_context(ctx: &egui::Context) -> Self {
        let p = theme::DARK;
        theme::apply(ctx, &p);
        // egui allows only 0.3 s between the clicks of a double-click (Windows: 0.5 s by
        // default), measured end to end, so normal double-clicks often went unnoticed.
        let double_click_time = system_double_click_time();
        ctx.options_mut(|o| o.input_options.max_double_click_delay = double_click_time);
        let ctx = ctx.clone();
        Self {
            p,
            settings: Settings::load(),
            loader: Loader::new(ctx.clone()),
            generation: 0,
            exporter: Exporter::new(ctx.clone()),
            cards: devices::watch(ctx),
            folder: None,
            shots: Vec::new(),
            picked: HashSet::new(),
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
            double_click_time,
            settings_page: None,
            thumbs: HashMap::new(),
            fulls: HashMap::new(),
            original: None,
            failed: HashSet::new(),
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

    fn open_folder(&mut self, ctx: &egui::Context, dir: PathBuf) {
        match library::scan(&dir) {
            Ok(shots) => {
                self.generation = self.loader.reset();
                self.thumbs.clear();
                self.fulls.clear();
                self.original = None;
                self.failed.clear();
                self.shots = shots;
                self.selected.clear();
                self.select_anchor = None;
                self.range = None;
                self.range_mode = RangeMode::Off;
                self.screen = Screen::Grid;
                self.current = 0;
                self.view_anchor = None;
                self.reset_zoom();
                self.message = if self.shots.is_empty() {
                    Some(("No JPG or RAW files in this folder".into(), false))
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

    /// Picks are remembered per folder, so they survive closing the app.
    fn reload_picks(&mut self) {
        self.picked.clear();
        let Some(folder) = &self.folder else { return };
        match picks::load(folder) {
            Some(names) => {
                for (i, shot) in self.shots.iter().enumerate() {
                    if names.contains(&shot.name.to_lowercase()) {
                        self.picked.insert(i);
                    }
                }
            }
            None => {
                // Folders picked in before picks were saved: count what's in the export folder.
                let Some(dest) = &self.settings.export_dir else { return };
                let Ok(entries) = std::fs::read_dir(dest) else { return };
                let names: HashSet<String> = entries
                    .filter_map(|e| e.ok())
                    .map(|e| e.file_name().to_string_lossy().to_lowercase())
                    .collect();
                for (i, shot) in self.shots.iter().enumerate() {
                    let exported = shot.files().any(|f| {
                        f.file_name().is_some_and(|n| names.contains(&n.to_string_lossy().to_lowercase()))
                    });
                    if exported {
                        self.picked.insert(i);
                    }
                }
                if !self.picked.is_empty() {
                    self.save_picks();
                }
            }
        }
    }

    fn save_picks(&self) {
        let Some(folder) = &self.folder else { return };
        picks::save(folder, self.picked.iter().map(|&i| self.shots[i].name.as_str()));
    }

    fn toggle_pick(&mut self, shot: usize) {
        let pick = !self.picked.contains(&shot);
        self.set_picked(&[shot], pick);
    }

    /// Picks (copies JPG + RAW) or unpicks (moves the copies to the Recycle Bin) shots.
    fn set_picked(&mut self, shots: &[usize], pick: bool) {
        let Some(dest) = self.export_dir_or_ask() else { return };
        for &shot in shots {
            if self.picked.contains(&shot) == pick {
                continue;
            }
            let files: Vec<PathBuf> = self.shots[shot].files().cloned().collect();
            if pick {
                self.picked.insert(shot);
                self.exporter.copy(shot, files, dest.clone(), None);
            } else {
                self.picked.remove(&shot);
                self.exporter.remove(shot, files, dest.clone());
            }
        }
        self.save_picks();
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

    /// Copies the selected photos (grid) or the shown photo (viewer), JPG + RAW.
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
            let files: Vec<PathBuf> = self.shots[shot].files().cloned().collect();
            self.exporter.copy(shot, files, dest.clone(), Some(batch));
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
        self.selected.clear();
        self.selected.insert(self.current);
        self.select_anchor = Some(self.current);
        self.fulls.clear();
        self.original = None;
        self.reset_zoom();
        self.loader.want_full(Vec::new());
    }

    fn reset_zoom(&mut self) {
        self.zoom = None;
        self.zoom_percent = None;
    }

    /// First and last shot index in the working set (all shots, or the selected range).
    fn shown_range(&self) -> Option<(usize, usize)> {
        if self.shots.is_empty() {
            return None;
        }
        match (self.range_mode, self.range) {
            (RangeMode::Off, Some(r)) => Some(r),
            _ => Some((0, self.shots.len() - 1)),
        }
    }

    fn items(&self) -> Vec<Item> {
        let mut items = Vec::new();
        let Some((from, to)) = self.shown_range() else { return items };
        for shot in from..=to {
            let s = &self.shots[shot];
            if s.raw.is_some() && (self.settings.show_raw || s.jpg.is_none()) {
                items.push(Item { shot, raw: true });
            }
            if s.jpg.is_some() {
                items.push(Item { shot, raw: false });
            }
        }
        items
    }

    fn step(&mut self, delta: isize) {
        let Some((from, to)) = self.shown_range() else { return };
        let next = (self.current as isize + delta).clamp(from as isize, to as isize) as usize;
        if next != self.current {
            self.current = next;
            self.reset_zoom();
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
            let (image, original) = match loaded.image {
                Ok(v) => v,
                Err(_) => {
                    self.failed.insert(key);
                    continue;
                }
            };
            match key.kind {
                Kind::Full => {
                    let tex = ctx.load_texture(format!("full-{}", key.shot), image, TextureOptions::LINEAR);
                    let original = vec2(original[0] as f32, original[1] as f32);
                    self.fulls.insert(key.shot, FullImage { tex, original });
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

        while let Some(done) = self.exporter.try_recv() {
            match done {
                Done::Copied { shot, batch, result } => {
                    if let Err(e) = &result {
                        if batch.is_none() {
                            self.picked.remove(&shot);
                            self.save_picks();
                        }
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
                Done::Removed { shot, result: Err(e) } => {
                    self.picked.insert(shot);
                    self.save_picks();
                    self.error(e);
                }
                Done::Removed { .. } => {}
            }
        }

        if self.thumbs.len() > MAX_THUMBS {
            let mut by_age: Vec<(u64, usize)> = self.thumbs.iter().map(|(k, t)| (t.last_used, *k)).collect();
            by_age.sort_unstable();
            for (_, shot) in by_age.iter().take(self.thumbs.len() - MAX_THUMBS * 4 / 5) {
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
            if have.is_none() && is_visible && !self.failed.contains(&preview) {
                previews.push(Request { key: preview, path: path.clone(), max: [THUMB_PX, THUMB_PX] });
            }
            let thumb = Key { shot, kind: Kind::Thumb };
            if have != Some(Kind::Thumb) && !self.failed.contains(&thumb) {
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
            .frame(Frame::new().fill(p.bar).inner_margin(Margin::symmetric(14, 9)))
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
                        if ui.add(Button::new(label)).on_hover_text("Open folder (Ctrl+O)").clicked() {
                            self.choose_folder(ctx);
                        }
                        self.recent_menu(ui, ctx);
                        if self.folder.is_some() {
                            ui.add_space(14.0);
                            let choice = segmented(ui, &p, &["JPG Only", "JPG + RAW"], self.settings.show_raw as usize);
                            if let Some(i) = choice {
                                self.settings.show_raw = i == 1;
                                self.settings.save();
                            }
                        }
                    }

                    ui.with_layout(Layout::right_to_left(Align::Center), |ui| {
                        self.export_button(ui);
                        // Right-to-left: the cog lands just left of "Export to".
                        let open = self.settings_page.is_some();
                        if cog_button(ui, &p, open).on_hover_text("Settings").clicked() {
                            self.settings_page = if open { None } else { Some(SettingsPage::Keybinds) };
                        }
                    });
                });

                // Centered title, drawn over the bar so it stays centered in the window.
                let center = ui.max_rect().center();
                let painter = ui.painter();
                match self.screen {
                    Screen::Viewer if !self.shots.is_empty() => {
                        let shot = &self.shots[self.current];
                        let (title, sub) = match (&shot.jpg, &shot.raw) {
                            (Some(_), Some(_)) => (shot.jpg_name(), format!("Paired with {}", shot.raw_name())),
                            (Some(_), None) => (shot.jpg_name(), "No RAW".into()),
                            _ => (shot.raw_name(), "RAW only".into()),
                        };
                        painter.text(center - vec2(0.0, 7.0), Align2::CENTER_CENTER, title, theme::font_semibold(13.5), p.label);
                        painter.text(center + vec2(0.0, 10.0), Align2::CENTER_CENTER, sub, theme::font(11.0), p.label2);
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
            Some(dir) => format!("{}     ", dir.display()),
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
        if let Some(dir) = open {
            self.open_folder(ctx, dir);
        }
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
                format!("Picks are copied to {}\nClick to change", dir.display())
            }
            None => {
                job.append("Not Set", 0.0, TextFormat { color: p.label2, ..plain });
                "Choose where picks are copied".to_string()
            }
        };
        let button = Button::new(job)
            .corner_radius(15)
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
                        ui.painter().text(center, Align2::CENTER_CENTER, "Open a folder to start", theme::font(13.5), p.label2);
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
        job.append("   Ctrl E", 0.0, key_hint(12.0, p.label2));
        let tip = "Copy the selected photos' JPG + RAW to the export folder";
        if ui.add_enabled(enabled, Button::new(job)).on_hover_text(tip).clicked() {
            self.quick_export();
        }
    }

    fn grid_controls(&mut self, ui: &mut Ui) {
        let p = self.p;
        let bar = ui.max_rect();
        self.quick_export_button(ui, !self.selected.is_empty());

        let selecting = self.range_mode != RangeMode::Off;
        let mut range_btn = Button::new(RichText::new("Select Range").color(if selecting { Color32::WHITE } else { p.label }));
        if selecting {
            range_btn = range_btn.fill(p.accent).stroke(Stroke::new(1.0_f32, p.accent));
        }
        if ui.add(range_btn).on_hover_text("Click the first and last shot of a set to work on just those").clicked() {
            self.range_mode = if selecting { RangeMode::Off } else { RangeMode::PickStart };
        }

        ui.add_space(6.0);
        size_icon(ui, &p, 9.0);
        ui.spacing_mut().slider_width = 110.0;
        let size = ui.add(Slider::new(&mut self.settings.tile_width, 120.0..=360.0).show_value(false));
        if size.drag_stopped() || size.lost_focus() {
            self.settings.save();
        }
        size_icon(ui, &p, 15.0);

        // Pick button in the middle of the bar, for everything selected.
        let pad = bar.center().x - PICK_W / 2.0 - ui.cursor().min.x;
        if pad > 0.0 {
            ui.add_space(pad);
        }
        let n = self.selected.len();
        let all_picked = n > 0 && self.selected.iter().all(|s| self.picked.contains(s));
        let label = match (n, all_picked) {
            (_, true) => "Picked".to_string(),
            (0 | 1, _) => "Pick".to_string(),
            (n, _) => format!("Pick {n}"),
        };
        let tip = if all_picked { "Unpick the selected photos" } else { "Copy the selected photos' JPG + RAW to the export folder" };
        if pick_button(ui, &p, &label, all_picked, n > 0).on_hover_text(tip).clicked() {
            self.pick_selected();
        }

        ui.with_layout(Layout::right_to_left(Align::Center), |ui| {
            self.picked_badge(ui);
            if let Some((from, to)) = self.shown_range() {
                let mut text = format!("{} shots", to - from + 1);
                if self.selected.len() > 1 {
                    text = format!("{} selected · {text}", self.selected.len());
                }
                ui.label(RichText::new(text).color(p.label2));
            }
            self.message_label(ui);
        });
    }

    fn picked_badge(&self, ui: &mut Ui) {
        let p = self.p;
        let files: usize = self.picked.iter().map(|&s| self.shots[s].files().count()).sum();
        let mut text = format!("✔  {} Picked · {} files", self.picked.len(), files);
        if self.exporter.pending() > 0 {
            text += "  · copying…";
        }
        Frame::new()
            .fill(p.tint_bg)
            .corner_radius(15)
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
        if pick_button(ui, &p, label, picked, true).on_hover_text("Copy JPG + RAW to the export folder").clicked() {
            self.toggle_pick(self.current);
        }
        if chevron_button(ui, &p, false).on_hover_text("Next (→)").clicked() {
            self.step(1);
        }

        ui.with_layout(Layout::right_to_left(Align::Center), |ui| {
            if let Some((from, to)) = self.shown_range() {
                let text = format!("{} of {} · {} Picked", self.current - from + 1, to - from + 1, self.picked.len());
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
                let open_btn = Button::new(RichText::new("Open Folder…    Ctrl O").color(Color32::WHITE).font(theme::font_semibold(14.0)))
                    .fill(p.accent)
                    .stroke(Stroke::NONE)
                    .corner_radius(18)
                    .min_size(vec2(0.0, 36.0));
                if ui.add(open_btn).clicked() {
                    self.choose_folder(ctx);
                }

                if !cards.is_empty() {
                    ui.add_space(24.0);
                    section_label(ui, &p, "Memory Cards");
                    for card in &cards {
                        let sub = format!("{} JPG · {} RAW", card.jpgs, card.raws);
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
        if self.range_mode != RangeMode::Off || self.range.is_some() {
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
        });
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
                Action::TogglePick(shot) => self.toggle_pick(shot),
                Action::Mark(shot) => match self.range_mode {
                    RangeMode::PickEnd(start) => {
                        self.range = Some((start.min(shot), start.max(shot)));
                        self.range_mode = RangeMode::Off;
                    }
                    _ => self.range_mode = RangeMode::PickEnd(shot),
                },
            }
        }
        Some((view, output.state.offset.y, output.content_size.y, row_pitch))
    }

    fn range_banner(&mut self, ui: &mut Ui) {
        let p = self.p;
        let (text, button) = match (self.range_mode, self.range) {
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
            .corner_radius(10)
            .inner_margin(Margin { left: 14, right: 6, top: 6, bottom: 6 })
            .show(ui, |ui| {
                ui.horizontal(|ui| {
                    ui.label(text);
                    ui.with_layout(Layout::right_to_left(Align::Center), |ui| {
                        let b = Button::new(RichText::new(button).color(p.tint).font(theme::font_semibold(13.5)))
                            .fill(Color32::TRANSPARENT)
                            .stroke(Stroke::NONE);
                        if ui.add(b).clicked() {
                            if self.range_mode != RangeMode::Off {
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
        let (rect, resp) = ui.allocate_exact_size(vec2(w, thumb_h + NAME_AREA), Sense::click());
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
        let circle = (!selecting).then(|| {
            ui.interact(circle_rect, ui.id().with(("pick", item.shot, item.raw)), Sense::click())
                .on_hover_text(if picked { "Unpick (moves the copies to the Recycle Bin)" } else { "Pick: copy JPG + RAW to the export folder" })
        });

        let painter = ui.painter();
        // Selection: a soft box around the whole tile, like Finder / Explorer.
        if selected {
            let sel = Rect::from_min_max(thumb_rect.min - vec2(6.0, 6.0), pos2(thumb_rect.max.x + 6.0, rect.max.y + 2.0));
            painter.rect_filled(sel, 9, Color32::from_white_alpha(28));
            painter.rect_stroke(sel, 9, Stroke::new(1.0_f32, Color32::from_white_alpha(40)), StrokeKind::Inside);
        }
        painter.rect_filled(thumb_rect, 6, p.tile);
        if let Some(t) = self.thumbs.get_mut(&item.shot) {
            t.last_used = self.frame;
            let fit = fit_rect(thumb_rect, t.tex.size_vec2());
            Image::new((t.tex.id(), fit.size())).corner_radius(4).paint_at(ui, fit);
        } else if self.failed.contains(&Key { shot: item.shot, kind: Kind::Thumb }) {
            painter.text(thumb_rect.center(), Align2::CENTER_CENTER, "No preview", theme::font(12.0), p.label2);
        }
        let hovered = resp.hovered() || circle.as_ref().is_some_and(|c| c.hovered());
        if resp.hovered() {
            painter.rect_filled(thumb_rect, 6, Color32::from_white_alpha(12));
        }
        if picked || is_start {
            painter.rect_stroke(thumb_rect, 7, Stroke::new(3.0_f32, p.accent), StrokeKind::Outside);
        }

        // File type badge (top-left).
        let (badge, fg, bg) = if item.raw {
            ("RAW", Color32::from_rgb(0x1D, 0x1D, 0x1F), Color32::from_white_alpha(235))
        } else {
            ("JPG", Color32::WHITE, Color32::from_black_alpha(140))
        };
        let galley = painter.layout_no_wrap(badge.into(), theme::font_semibold(10.0), fg);
        let badge_rect = Rect::from_min_size(thumb_rect.min + vec2(8.0, 8.0), galley.size() + vec2(10.0, 4.0));
        painter.rect_filled(badge_rect, 4, bg);
        painter.galley(badge_rect.min + vec2(5.0, 2.0), galley, fg);

        if circle.is_some() && (picked || hovered) {
            let c = circle_rect.center();
            if picked {
                painter.circle_filled(c, 12.0, p.accent);
                painter.circle_stroke(c, 12.0, Stroke::new(2.0_f32, Color32::WHITE));
                check_mark(painter, c, 5.5, Color32::WHITE);
            } else {
                painter.circle_filled(c, 12.0, Color32::from_black_alpha(80));
                painter.circle_stroke(c, 12.0, Stroke::new(1.5_f32, Color32::from_white_alpha(230)));
            }
        }

        if selecting && hovered {
            let text = if is_start { "Start" } else if matches!(self.range_mode, RangeMode::PickStart) { "Set Start" } else { "Set End" };
            let g = painter.layout_no_wrap(text.into(), theme::font_semibold(13.0), Color32::WHITE);
            let r = Rect::from_center_size(thumb_rect.center(), g.size() + vec2(24.0, 12.0));
            painter.rect_filled(r, 14, if is_start { p.accent } else { Color32::from_black_alpha(160) });
            painter.galley(r.min + vec2(12.0, 6.0), g, Color32::WHITE);
        }

        // File name, with a blue highlight when selected (like Finder).
        let name = if item.raw { shot.raw_name() } else { shot.jpg_name() };
        let name_color = if selected { Color32::WHITE } else { p.label };
        let g = painter.layout_no_wrap(name, theme::font(12.5), name_color);
        let name_rect = Rect::from_center_size(
            pos2(thumb_rect.center().x, thumb_rect.max.y + 16.0),
            g.size() + vec2(12.0, 2.0),
        );
        if selected {
            painter.rect_filled(name_rect, 5, p.accent);
        }
        painter.galley(name_rect.min + vec2(6.0, 1.0), g, name_color);
        let sub = match (item.raw, shot.jpg.is_some(), shot.raw.is_some()) {
            (true, true, _) => "Paired with JPG".to_string(),
            (true, false, _) => "No JPG".to_string(),
            (false, _, true) => format!("+ {}", shot.raw_kind()),
            (false, _, false) => "No RAW".to_string(),
        };
        painter.text(pos2(thumb_rect.center().x, name_rect.max.y + 8.0), Align2::CENTER_CENTER, sub, theme::font(11.0), p.label2);

        if circle.is_some_and(|c| c.clicked()) {
            return (Some(Action::TogglePick(item.shot)), rect);
        }
        let resp = resp.on_hover_cursor(if selecting { CursorIcon::Crosshair } else { CursorIcon::Default });
        // Double-clicks are spotted in grid_contents from two clicks in a row.
        let action = if selecting {
            resp.clicked().then_some(Action::Mark(item.shot))
        } else {
            resp.clicked().then_some(Action::Click(item.shot))
        };
        (action, rect)
    }

    fn viewer_screen(&mut self, ui: &mut Ui) {
        let p = self.p;
        let area = ui.available_rect_before_wrap();
        let resp = ui.allocate_rect(area, Sense::click_and_drag());
        let shot = self.current;
        let ppp = ui.ctx().pixels_per_point();
        let max_zoom = MAX_ZOOM / ppp;

        let full = self.fulls.get(&shot);
        let original_size = full.map(|f| f.original);
        self.shown_original = original_size;

        match (full, original_size) {
            (Some(full), Some(orig)) => {
                let fit = (area.width() / orig.x).min(area.height() / orig.y);

                // Double-click: zoom to 100% at that spot, or back to fit.
                if resp.double_clicked() {
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
                let tex = match &self.original {
                    Some((s, t)) if *s == shot && needs_original => t,
                    _ => &full.tex,
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
                    let failed = self.failed.contains(&Key { shot, kind: Kind::Full });
                    let text = if failed { "No preview found in this file" } else { "Loading…" };
                    ui.painter().text(area.center(), Align2::CENTER_CENTER, text, theme::font(14.0), p.label2);
                }
            }
        }

        if self.picked.contains(&shot) && self.zoom.is_none() {
            let painter = ui.painter();
            let s = &self.shots[shot];
            let copied = match (s.jpg.is_some(), s.raw.is_some()) {
                (true, true) => format!("JPG + {}", s.raw_kind()),
                (true, false) => "JPG".to_string(),
                _ => s.raw_kind(),
            };
            let g = painter.layout_no_wrap(format!("Picked · {copied} copied"), theme::font(13.0), Color32::WHITE);
            let r = Rect::from_center_size(pos2(area.center().x, area.min.y + 20.0), g.size() + vec2(44.0, 14.0));
            painter.rect_filled(r, 16, Color32::from_rgba_unmultiplied(30, 30, 30, 215));
            let c = pos2(r.min.x + 17.0, r.center().y);
            painter.circle_filled(c, 10.0, p.accent);
            check_mark(painter, c, 4.5, Color32::WHITE);
            painter.galley(pos2(r.min.x + 34.0, r.min.y + 7.0), g, Color32::WHITE);
        }

        // Load: all pixels if zoomed in, then this photo and its neighbours at screen size.
        let mut requests = Vec::new();
        if let (Some(orig), Some(zoom)) = (original_size, self.zoom) {
            let full_w = self.fulls.get(&shot).map(|f| f.tex.size_vec2().x).unwrap_or(0.0);
            let have = self.original.as_ref().is_some_and(|(s, _)| *s == shot);
            let key = Key { shot, kind: Kind::Original };
            if zoom * ppp * orig.x > full_w * 1.05 && !have && !self.failed.contains(&key) {
                if let Some(path) = self.shots[shot].display_path() {
                    let side = ui.input(|i| i.max_texture_side) as u32;
                    requests.push(Request { key, path: path.clone(), max: [side, side] });
                }
            }
        }
        let max = [(area.width() * ppp).max(800.0) as u32, (area.height() * ppp).max(600.0) as u32];
        if let Some((from, to)) = self.shown_range() {
            for s in [shot as isize, shot as isize + 1, shot as isize - 1, shot as isize + 2] {
                if s < from as isize || s > to as isize {
                    continue;
                }
                let s = s as usize;
                let key = Key { shot: s, kind: Kind::Full };
                if let Some(path) = self.shots[s].display_path() {
                    if !self.fulls.contains_key(&s) && !self.failed.contains(&key) {
                        requests.push(Request { key, path: path.clone(), max });
                    }
                }
            }
        }
        self.loader.want_full(requests);
        self.fulls.retain(|s, _| s.abs_diff(shot) <= 2);
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
        check_mark(&painter, c, 4.5, white);
        painter.galley(pos2(r.min.x + 38.0, r.center().y - g.size().y / 2.0), g, white);
        ctx.request_repaint();
    }
}

impl eframe::App for App {
    fn update(&mut self, ctx: &egui::Context, _frame: &mut eframe::Frame) {
        self.frame_ui(ctx);
    }
}

impl App {
    /// Everything drawn and handled in one frame.
    fn frame_ui(&mut self, ctx: &egui::Context) {
        self.frame += 1;
        self.receive(ctx);

        // A folder (or a photo inside one) dropped onto the window opens it.
        let dropped = ctx.input(|i| i.raw.dropped_files.iter().find_map(|f| f.path.clone()));
        if let Some(path) = dropped {
            let dir = if path.is_dir() { Some(path) } else { path.parent().map(Path::to_path_buf) };
            if let Some(dir) = dir {
                self.open_folder(ctx, dir);
            }
        }

        #[derive(Default)]
        struct Keys {
            open: bool,
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
        let mut k = ctx.input(|i| Keys {
            open: i.modifiers.command && i.key_pressed(KeyCode::O),
            export: i.modifiers.command && i.key_pressed(KeyCode::E),
            select_all: i.modifiers.command && i.key_pressed(KeyCode::A),
            esc: i.key_pressed(KeyCode::Escape),
            enter: i.key_pressed(KeyCode::Enter),
            left: i.key_pressed(KeyCode::ArrowLeft),
            right: i.key_pressed(KeyCode::ArrowRight),
            pick: !i.modifiers.command && (i.key_pressed(KeyCode::P) || i.key_pressed(KeyCode::Space)),
            home: i.key_pressed(KeyCode::Home),
            end: i.key_pressed(KeyCode::End),
        });
        // While Settings is open, keys don't reach the grid or viewer; Esc closes it.
        if self.settings_page.is_some() {
            if k.esc {
                self.settings_page = None;
            }
            k = Keys::default();
        }
        if k.open {
            self.choose_folder(ctx);
        }
        if k.export && self.folder.is_some() {
            self.quick_export();
        }
        match self.screen {
            Screen::Viewer if !self.shots.is_empty() => {
                if k.esc {
                    self.back_to_grid();
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
                    if let Some((from, to)) = self.shown_range() {
                        self.selected.extend(from..=to);
                    }
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

        self.top_bar(ctx);
        self.bottom_bar(ctx);

        let p = self.p;
        let viewer = self.screen == Screen::Viewer && !self.shots.is_empty();
        let margin = if viewer {
            Margin::same(24)
        } else if self.folder.is_some() {
            // Grid: the scroll bar sits against the window's right edge.
            Margin { left: 16, right: 2, top: 12, bottom: 0 }
        } else {
            Margin::same(16)
        };
        let frame = Frame::new().fill(if viewer { p.viewer_bg } else { p.bg }).inner_margin(margin);
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
        self.draw_toast(ctx);
    }

    /// Settings: a sidebar of pages (Keybinds, Themes) and the chosen page.
    fn settings_window(&mut self, ctx: &egui::Context) {
        let Some(page) = self.settings_page else { return };
        let p = self.p;
        let mut open = true;
        egui::Window::new(RichText::new("Settings").font(theme::font_semibold(15.0)))
            .open(&mut open)
            .collapsible(false)
            .resizable(false)
            .anchor(Align2::CENTER_CENTER, Vec2::ZERO)
            .fixed_size(vec2(620.0, 430.0))
            .frame(Frame::window(&ctx.style()).fill(p.card).inner_margin(Margin::same(16)))
            .show(ctx, |ui| {
                ui.horizontal_top(|ui| {
                    // Sidebar.
                    ui.vertical(|ui| {
                        ui.set_width(140.0);
                        for (target, label) in [(SettingsPage::Keybinds, "Keybinds"), (SettingsPage::Themes, "Themes")] {
                            let on = page == target;
                            let b = Button::new(RichText::new(label).color(if on { Color32::WHITE } else { p.label }))
                                .fill(if on { p.accent } else { Color32::TRANSPARENT })
                                .stroke(Stroke::NONE)
                                .corner_radius(theme::RADIUS)
                                .min_size(vec2(140.0, 32.0));
                            if ui.add(b).clicked() {
                                self.settings_page = Some(target);
                            }
                        }
                    });
                    ui.add_space(8.0);
                    ui.separator();
                    ui.add_space(8.0);
                    ui.vertical(|ui| match page {
                        SettingsPage::Keybinds => keybinds_page(ui, &p),
                        SettingsPage::Themes => themes_page(ui, &p),
                    });
                });
            });
        if !open {
            self.settings_page = None;
        }
    }
}

fn keybinds_page(ui: &mut Ui, p: &Palette) {
    ScrollArea::vertical().max_height(370.0).show(ui, |ui| {
        for (title, keys) in [("Grid", GRID_KEYS), ("Viewer", VIEWER_KEYS)] {
            ui.label(RichText::new(title).font(theme::font_semibold(13.5)).color(p.label2));
            egui::Grid::new(title).num_columns(2).spacing(vec2(18.0, 7.0)).show(ui, |ui| {
                for (key, action) in keys {
                    ui.label(RichText::new(*key).font(theme::font_semibold(13.0)));
                    ui.label(RichText::new(*action).color(p.label2));
                    ui.end_row();
                }
            });
            ui.add_space(12.0);
        }
        ui.label(RichText::new("Changing keybinds is coming soon.").color(p.label2).italics());
    });
}

fn themes_page(ui: &mut Ui, p: &Palette) {
    ui.label(RichText::new("Theme").font(theme::font_semibold(13.5)).color(p.label2));
    ui.add_space(4.0);
    // The one theme so far, shown as selected.
    Frame::new()
        .fill(p.bg)
        .stroke(Stroke::new(2.0_f32, p.accent))
        .corner_radius(10)
        .inner_margin(Margin::same(12))
        .show(ui, |ui| {
            ui.set_width(220.0);
            ui.horizontal(|ui| {
                for c in [p.bar, p.control, p.label2, p.accent] {
                    let (r, _) = ui.allocate_exact_size(vec2(22.0, 22.0), Sense::hover());
                    ui.painter().circle_filled(r.center(), 10.0, c);
                }
            });
            ui.add_space(6.0);
            ui.label(RichText::new("Dark (Apple)").font(theme::font_semibold(13.5)));
            ui.label(RichText::new("Current theme").color(p.label2));
        });
    ui.add_space(14.0);
    ui.label(
        RichText::new("More themes, including a Windows-style one, and ways to customize them are coming soon.")
            .color(p.label2)
            .italics(),
    );
}

// -------------------------------------------------------------------- helpers

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
        Color32::from_gray(0xA8)
    } else if active {
        Color32::from_gray(0x8C)
    } else {
        Color32::from_gray(0x6A)
    };
    painter.rect_filled(bar, CornerRadius::same((w / 2.0) as u8), color);
    for (r, resp, upward) in [(up, &up_r, true), (down, &down_r, false)] {
        let c = r.center();
        let s = 3.5;
        let color = if resp.hovered() { p.label } else { Color32::from_gray(0x8A) };
        let (tip, base) = if upward { (-s * 0.6, s * 0.6) } else { (s * 0.6, -s * 0.6) };
        let points = vec![c + vec2(-s, base), c + vec2(s, base), c + vec2(0.0, tip)];
        painter.add(Shape::convex_polygon(points, color, Stroke::NONE));
    }

    let new = new.clamp(0.0, max_off);
    ((new - offset).abs() > 0.01).then_some(new)
}

/// Round icon button with a gear drawn in it, for Settings.
fn cog_button(ui: &mut Ui, p: &Palette, open: bool) -> Response {
    let (rect, resp) = ui.allocate_exact_size(vec2(36.0, 32.0), Sense::click());
    let painter = ui.painter();
    if open || resp.hovered() {
        let fill = if open { p.control } else { Color32::from_white_alpha(14) };
        painter.rect_filled(rect, theme::RADIUS, fill);
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
    let (fill, color) = if picked { (p.accent, Color32::WHITE) } else { (p.control, p.tint) };
    let mut job = LayoutJob::default();
    job.append(&format!("✔  {label}"), 0.0, TextFormat { font_id: theme::font_semibold(15.0), color, ..Default::default() });
    let key_color = if picked { Color32::from_white_alpha(170) } else { p.label2 };
    job.append("   P", 0.0, key_hint(13.0, key_color));
    let button = Button::new(job)
        .fill(fill)
        .stroke(Stroke::new(1.0_f32, if picked { p.accent } else { p.control_border }))
        .corner_radius(22)
        .min_size(vec2(PICK_W, 44.0));
    ui.add_enabled(enabled, button)
}

fn chevron_button(ui: &mut Ui, p: &Palette, left: bool) -> Response {
    let (rect, resp) = ui.allocate_exact_size(Vec2::splat(44.0), Sense::click());
    let painter = ui.painter();
    let fill = if resp.is_pointer_button_down_on() {
        theme::lighten(p.control, 24)
    } else if resp.hovered() {
        theme::lighten(p.control, 12)
    } else {
        p.control
    };
    painter.circle_filled(rect.center(), 22.0, fill);
    painter.circle_stroke(rect.center(), 21.5, Stroke::new(1.0_f32, p.control_border));
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
        painter.rect_filled(rect, theme::RADIUS, Color32::from_white_alpha(14));
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

/// macOS-style segmented control. Returns the index clicked, if any.
fn segmented(ui: &mut Ui, p: &Palette, options: &[&str], selected: usize) -> Option<usize> {
    let mut clicked = None;
    Frame::new()
        .fill(p.segment_bg)
        .corner_radius(8)
        .inner_margin(Margin::same(2))
        .show(ui, |ui| {
            ui.spacing_mut().item_spacing.x = 0.0;
            for (i, option) in options.iter().enumerate() {
                let on = i == selected;
                let b = Button::new(RichText::new(*option).color(p.label))
                    .fill(if on { p.segment_on } else { Color32::TRANSPARENT })
                    .stroke(Stroke::NONE)
                    .corner_radius(6)
                    .min_size(vec2(0.0, 26.0));
                if ui.add(b).clicked() {
                    clicked = Some(i);
                }
            }
        });
    clicked
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
    let fill = if resp.hovered() { theme::lighten(p.card, 10) } else { p.card };
    painter.rect_filled(rect, CornerRadius::same(10), fill);
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
                    jpg: Some(PathBuf::from(format!(r"Z:\cullery-test\IMG_{i:04}.JPG"))),
                    raw: Some(PathBuf::from(format!(r"Z:\cullery-test\IMG_{i:04}.CR3"))),
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
