# MetaSafe v0.6.0

[Русский](README.ru.md) · **English**

MetaSafe is an open-source desktop app and CLI for inspecting photo and video metadata and saving verified copies. Processing stays on your device, and the originals are preserved. The source is available under the [MIT License](LICENSE).

## Download

[Download MetaSafe v0.6.0](https://github.com/V3rCg1/MetaSafe/releases/latest). The desktop app is available for Linux (x64 and ARM64), Windows (x64), and macOS (Apple Silicon).

### Features

- View photo and video metadata, including EXIF, GPS and comments when present.
- Remove supported metadata from individual files or batches and save copies alongside or separately from the originals.
- Clean static JPEG, PNG and WebP without re-encoding.
- Clean MP4, MOV, MKV and WebM without re-encoding, preserving subtitle tracks and HDR signaling by default.
- Play compatible videos in the built-in player. Playback depends on system codecs.

## Desktop app

1. In the Tauri app, click **Open files** and select photos or videos. You can also drag files into the window.
2. Select a file to see its preview and metadata. Expand any row marked **Found** to see the values. EXIF fields can contain precise coordinates.
3. Click **Create cleaned copy…** for one file and choose a name. For multiple files, click **Create cleaned copies…** and choose a destination folder.
4. When processing finishes, expand the report for any file.

In batch mode, `photo.jpeg` becomes `photo_clean.jpg`. If that name already exists, MetaSafe reports an error for that file and continues with the rest. Use separate output folders for files with the same base name. The app follows the system theme. Use the language switcher in the upper-right corner to change the interface language.

## Command-line interface

On macOS, replace the example names with paths to your JPEG files. Create the `cleaned` folder before running batch cleanup.

```sh
./metasafe inspect "photo 1.jpg"
./metasafe clean "photo 1.jpg" --output "photo 1_clean.jpg"
./metasafe verify "photo 1_clean.jpg"
mkdir -p cleaned
./metasafe batch "photo 1.jpg" "photo 2.jpeg" --output-dir cleaned
```

In PowerShell:

```powershell
.\metasafe.exe inspect "photo 1.jpg"
.\metasafe.exe clean "photo 1.jpg" --output "photo 1_clean.jpg"
.\metasafe.exe verify "photo 1_clean.jpg"
New-Item -ItemType Directory -Force cleaned
.\metasafe.exe batch "photo 1.jpg" "photo 2.jpeg" --output-dir cleaned
```

- `inspect` reports JPEG container metadata and EXIF fields.
- `clean` applies EXIF orientation, saves and verifies a temporary copy beside the destination, then publishes it without overwriting an existing file. It returns an error if EXIF is malformed or orientation is ambiguous.
- `verify` returns 0 if the checks pass, 1 if metadata is found or a check is incomplete, and 2 for an input, read, or decoding error.
- `batch` returns 0 if all copies are saved, 1 if any file fails, and 2 for a command-level error such as a missing output directory.

## Image formats in the current source

MetaSafe opens JPEG, PNG, APNG, WebP, GIF, TIFF, BMP, ICO, TGA, QOI, PNM, HDR, and supported DDS files directly. HEIC/HEIF, AVIF, PSD/PSB, JPEG 2000, JPEG XL, and supported camera RAW files use local tools.

- Install ExifTool for metadata in these additional formats.
- Install ImageMagick with the required format codecs for HEIC/HEIF, AVIF, PSD/PSB, JPEG 2000, and JPEG XL.
- Install LibRaw with `dcraw_emu` for camera RAW, including DNG, CR2/CR3, NEF, ARW, RAF, and RW2 when the camera is supported.

On macOS, Homebrew can install these tools with `brew install imagemagick exiftool libraw`. JPEG 2000 and JPEG XL require a build that includes their codecs. On Windows, put the executables on PATH. You can also set absolute executable paths in `METASAFE_MAGICK`, `METASAFE_EXIFTOOL`, and `METASAFE_DCRAW_EMU`.

Open files in the Tauri interface and expand a metadata row marked **Found** to see its fields. Unverified checks are labeled separately. Batch cleanup defaults to JPEG for JPEG inputs and PNG for other inputs. PNG copies retain transparency. Animated and multi-image inputs export one rendered image, without layers or animation.

## Clean without re-encoding

In Tauri, turn on **Keep encoding** before saving a single file or batch. The CLI offers the same mode.

```sh
metasafe clean photo.webp --output photo_clean.webp --no-reencode
metasafe batch photo.jpg diagram.png image.webp --output-dir cleaned --no-reencode
```

This mode removes container metadata from static JPEG, PNG, and WebP while copying the compressed image data. The output uses the same format. PNG and WebP transparency stays intact. Files that require rotation or a color transform and animated files return a per-file error. Batch cleanup continues with other files. Removing color profiles may affect rendering in color-managed viewers.

## Video

The Tauri app and CLI accept MP4, MOV, MKV, and WebM. Install `ffmpeg` and `ffprobe` and make them available on `PATH`. Open videos in Tauri to inspect their metadata, play compatible tracks, and save one or more cleaned copies. Playback depends on the codecs available to the system webview.

```sh
metasafe inspect clip.mkv
metasafe clean clip.mkv --output clip_clean.mkv
mkdir -p cleaned
metasafe batch clip.mkv recording.webm --output-dir cleaned
metasafe verify cleaned/recording_clean.webm
```

Default cleanup copies encoded video, audio, and subtitle tracks to the same container format. It removes supported container tags and chapters. Subtitle text and HDR signaling can carry information and are retained. Review subtitle content before sharing. Matroska files with attachments such as subtitle fonts are rejected to avoid silently changing how subtitles display.

`--strip-h264-sei` opts into removing H.264 SEI messages from encoded packets without re-encoding. Use it only for compatible SDR H.264 input. The tool rejects detectable HDR or embedded caption signaling when filtering. H.264 SEI can carry other information too, and the filter removes all SEI messages it encounters. The video size limit is under 2 GiB and each file can contain up to 32 tracks. Large files need temporary disk space for the input snapshot and cleaned copy. The metadata report does not certify removal of private data inside encoded streams or subtitle text. Other codecs, Dolby Vision, HDR10+ and custom stream payloads do not have a general in-stream cleaner.

## Linux

Download the Linux `.deb` package for your architecture (`amd64` for x64 or `arm64` for ARM64). On systems with APT, run `sudo apt install ./MetaSafe_0.6.0_arm64.deb` from the download folder for ARM64, or use the `amd64` filename for x64. APT installs FFmpeg and the desktop integration dependencies. Its recommended GStreamer packages broaden playback support. On Linux, the built-in player streams the selected file through a private address on the same device (`127.0.0.1`).

The CI workflow builds the CLI and the Tauri `.deb` package on Ubuntu 24.04. To build locally, install the Linux dependencies listed by [Tauri](https://v2.tauri.app/start/prerequisites/#linux), then run the Tauri steps below. For a `.deb` package, run `npm run tauri build -- --bundles deb` in `desktop/`. For the CLI alone, run `cargo build --release --locked --no-default-features` from the project root.

## Run the Tauri interface

Install Node.js, Rust 1.90 or later, and the [Tauri system prerequisites](https://v2.tauri.app/start/prerequisites/). From the project root, run:

```sh
cd desktop
npm ci
npm run tauri dev
```

Use `npm run tauri build` from `desktop/` to create a desktop bundle for your platform.

## Build from source

Install Rust 1.88 or later and run this from the project root:

```sh
cargo build --release --locked --bins
```

The binaries are written to `target/release`. You can also install both with `cargo install --path . --locked`. To build only the CLI, run `cargo build --release --locked --no-default-features --bin metasafe`.

## Verification and limits

MetaSafe supports JPEG. It checks EXIF, GPS in EXIF, XMP, IPTC, JPEG comments, and ICC profiles. The report flags unrecognized segments and data after the JPEG end marker. A standard JFIF header without a thumbnail is allowed. The `verify` command also checks whether the JPEG decodes.

Cleaning re-encodes the JPEG at quality 90 and removes the original ICC profile. File size, image quality, and color rendering may change. Faces, addresses, and other visible details remain in the pixels. A passing report does not establish anonymity.

EXIF parsing is limited to 1,024 fields, 32 TIFF directories, and an estimated 1 MiB allocation budget. If any limit is exceeded, MetaSafe marks EXIF and GPS as unverified and stops cleaning that file.

Choose an output folder and parent directories that only trusted users can modify. MetaSafe compares the saved file with the verified copy and reports a mismatch. A process with write access can replace the file during or after this check.

Photo input and output files are limited to 64 MiB. Decoding is limited to 16,384 pixels per side and 40 megapixels. The decoder allocation limit is best-effort and does not cap total process memory. A crash can leave a temporary file in the output folder.
