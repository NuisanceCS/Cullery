# Cullery

## About

*Cull + gallery.* A fast, simple photo culler for Windows and Mac, made for RAW + JPEG shoots.

## What it does

- **Cull right from the folder.** Open folders directly in Cullery and pick your favorite pictures out of thousands.
- **RAW and JPEG, linked.** Every RAW is automatically linked to its JPEG, so each shot is one photo to look at.
- **Batch export anywhere.** Send your picks directly into Lightroom or any other app, or out to a folder.
- **Built with Rust.** No unnecessary waiting for things to load.

## Download

Get the latest version from the [Releases page](../../releases/latest).

## Build

```
cargo build --release
```

On Windows the app is a single file: `target\release\cullery.exe`. To make the Mac app and installer, run `bash tools/make-mac-app.sh` on a Mac.

## Contributing

Ideas and improvements are welcome. Open an issue or a pull request. Contributions are accepted under the same license as the project.

## License

Copyright © 2026 NuisanceCS. Cullery is free and open source under the [GNU General Public License v3](LICENSE).

The name "Cullery" and the logo aren't covered by that license. If you distribute your own version, please give it its own name and logo. See [NOTICE](NOTICE).
