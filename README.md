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
- **Quick Export** (`Ctrl+E`): copies the selected photos (or the one in the viewer) to the
  export folder without marking them picked, then confirms at the bottom of the window.
- **Viewer:** `←` / `→` previous / next, `P` or `Space` pick, `Esc` back to grid.
  Scroll wheel zooms around the cursor, double-click zooms to that spot (again to fit),
  drag to move around. The zoom readout next to the arrows switches between Fit and 100%.
- **Export folder:** the button in the top right. Picking copies the files right away;
  un-picking moves those copies to the Recycle Bin. Picks are remembered per folder in
  `%APPDATA%\Cullery\picks\`.

JPG and RAW are paired by file name (`IMG_4821.JPG` + `IMG_4821.CR3`).
Settings live in `%APPDATA%\Cullery\settings.txt`.

## Test photos

```
powershell -ExecutionPolicy Bypass -File tools\make-test-photos.ps1
```

makes `test-photos\` with fake full-size JPGs and dummy CR3s (a few unpaired on purpose).
