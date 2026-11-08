# Cullery

*Cull + gallery.* A fast, simple photo culler for RAW + JPEG shoots (Canon CR3). Browse the JPGs; picking a
shot copies its JPG **and** its matching CR3 to your export folder.

## Run

```
cargo run --release
```

The built app is `target\release\cullery.exe`.

## Use

- **Open a folder:** Open Folder button, `Ctrl+O`, drag a folder onto the window, or click
  a memory card / recent folder on the start screen.
- **Grid:** click to select (`Ctrl`+click adds/removes, `Shift`+click selects a run,
  `Ctrl+A` all, `Esc` clears); double-click or `Enter` opens a photo. `P` / `Space` or the
  Pick button picks everything selected (or unpicks, if all are picked). The circle in a
  thumbnail's corner picks just that one. "JPG Only / JPG + RAW" (top bar) switches whether
  RAW files show as their own tiles. "Select Range" narrows the grid to one set of shots (click first, then last).
  The photo in the middle of the grid stays put when the layout changes.
- **View Picked** (bottom right): show only picked photos; the viewer's arrows skip the rest.
- **Drag out:** drag a photo (or a selection) from the grid into Lightroom, LightCraft, Explorer
  or any app that takes dropped files; it arrives as one group. Settings → Drag & Drop picks
  RAW (default), JPG or both. Copies already in the export folder are sent from there.
  While dragging, the window dims and says how many photos and which files are going, and
  a little stack of the photos (with the count) follows the cursor.
- **Delete** (trash button, or the `Delete` key): asks first, then deletes the selected
  photos (or the one in the viewer) with their RAW files — to the Recycle Bin, or
  permanently from a memory card (which has no Recycle Bin; the dialog says so).
  Export-folder copies are kept.
- **Themes** (Settings → Themes): Apple, Windows 11 (with the Mica wallpaper-tinted top bar,
  on Windows 11 22H2+) and Windows 10, each in dark and light.
- **Quick Export** (`Ctrl+E`): copies the selected photos (or the one in the viewer) to the
  export folder without marking them picked, then confirms at the bottom of the window.
- **Viewer:** `←` / `→` previous / next, `P` or `Space` pick, `Esc` back to grid.
  Scroll wheel zooms around the cursor, double-click zooms to that spot (again to fit),
  drag to move around. The zoom readout next to the arrows switches between Fit and 100%.
- **Picks** are just marks unless **Export on Pick** (Settings, off by default) is on; then
  picking also copies the chosen files (JPG + RAW by default) to the export folder.
  Copies are never duplicated (an existing copy is reused), never overwritten (a
  different file with the same name gets ` (2)`), and unpicking never deletes them.
  Picks are remembered per folder in `%APPDATA%\Cullery\picks\`.
- **Export folder:** the button in the top right, or Settings → Export on Pick.

JPG and RAW are paired by file name (`IMG_4821.JPG` + `IMG_4821.CR3`); HEIC/HEIF pair the
same way. PNG, GIF (animated in the viewer), WebP, TIFF and BMP show on their own.
HEIC/HEIF (and AVIF) are decoded by Windows, which needs Microsoft's HEIF Image Extension
(plus HEVC Video Extensions; AV1 Video Extension for AVIF) from the Store. When one is
missing, the tile says so and the viewer has a button to its Store page.
Shots with only a RAW are shown using the JPEG previews the camera embeds in the file
(no RAW processing): CR3 (thumbnail, 1620×1080 and full size), TIFF-based RAWs (CR2, NEF,
ARW, DNG, ORF, RW2, PEF, SRW) and Fujifilm RAF.

To check the RAW reader against real files:

```
$env:CULLERY_RAW_SAMPLES='F:\DCIM\100CANON'; cargo test --release raw -- --nocapture
```
Settings live in `%APPDATA%\Cullery\settings.txt`.

## Test photos

```
powershell -ExecutionPolicy Bypass -File tools\make-test-photos.ps1
```

makes `test-photos\` with fake full-size JPGs and dummy CR3s (a few unpaired on purpose).
