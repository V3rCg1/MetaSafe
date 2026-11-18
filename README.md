# MetaSafe

[English](README.md) · [Русский](README.ru.md) · [Download v0.8.1](https://github.com/V3rCg1/MetaSafe/releases/tag/v0.8.1) · [Changes](docs/releases/v0.8.1.md) · [MIT license](LICENSE)

Inspect and remove supported metadata from photos, videos and documents on your device. MetaSafe saves a separate, verified copy and keeps the original. It includes a desktop app for macOS, Windows and Linux, plus a command-line interface.

JPEG, PNG, WebP, PDF, DOCX and XLSX work without extra tools. Videos need FFmpeg. Additional image formats use the local tools listed below.

## Download and install

Download the file for your operating system from [v0.8.1](https://github.com/V3rCg1/MetaSafe/releases/tag/v0.8.1). GitHub's automatically generated **Source code** archives contain the project sources, not an installer.

| System | Desktop download | Installation |
| --- | --- | --- |
| macOS, Apple Silicon (M1 or later) | [MetaSafe-v0.8.1-macos-arm64.zip](https://github.com/V3rCg1/MetaSafe/releases/download/v0.8.1/MetaSafe-v0.8.1-macos-arm64.zip) | Unzip and move `MetaSafe.app` to Applications |
| Windows, x64 | [MetaSafe_0.8.1_x64-setup.exe](https://github.com/V3rCg1/MetaSafe/releases/download/v0.8.1/MetaSafe_0.8.1_x64-setup.exe) | Run the installer |
| Linux, x64 with APT | [MetaSafe_0.8.1_amd64.deb](https://github.com/V3rCg1/MetaSafe/releases/download/v0.8.1/MetaSafe_0.8.1_amd64.deb) | Install with APT as shown below |

Ready-made desktop builds cover these architectures. macOS Intel and Linux ARM64 packages are not included in this release.

### macOS

1. Unzip the archive and drag `MetaSafe.app` into Applications.
2. Open MetaSafe from Applications.
3. The app is not notarized by Apple. If macOS blocks it, check the downloaded file's checksum, then use **System Settings > Privacy & Security > Open Anyway** if you trust the download. The option appears after the blocked launch.

For videos, install [Homebrew](https://brew.sh/) if needed, then run:

```sh
brew install ffmpeg
```

Restart MetaSafe after installing the tools. Ordinary photo and document cleanup does not need FFmpeg.

### Windows

1. Download and run `MetaSafe_0.8.1_x64-setup.exe`.
2. Follow the installer and open MetaSafe from the Start menu.
3. The installer is not signed with a commercial code-signing certificate. If SmartScreen blocks it, verify the checksum before choosing **More info > Run anyway** for a download you trust.

The desktop app uses Microsoft Edge WebView2. If the installer needs to install that runtime, allow it to finish; downloading the runtime may need an internet connection.

For video support, install a Windows build of [FFmpeg](https://ffmpeg.org/download.html), add the folder containing `ffmpeg.exe` and `ffprobe.exe` to `PATH`, and restart MetaSafe. These commands should work in a new terminal:

```powershell
ffmpeg -version
ffprobe -version
```

### Linux

From the folder containing the downloaded package, run:

```sh
sudo apt install ./MetaSafe_0.8.1_amd64.deb
```

APT installs FFmpeg and the required desktop dependencies. Open MetaSafe from the application menu. The package is built on Ubuntu 24.04; other Linux systems need compatible libraries or a [source build](#build-from-source). Recommended GStreamer packages broaden video playback support.

### CLI downloads

| System | Command-line executable |
| --- | --- |
| macOS, Apple Silicon | [metasafe-v0.8.1-macos-arm64](https://github.com/V3rCg1/MetaSafe/releases/download/v0.8.1/metasafe-v0.8.1-macos-arm64) |
| Windows, x64 | [metasafe-v0.8.1-windows-x64.exe](https://github.com/V3rCg1/MetaSafe/releases/download/v0.8.1/metasafe-v0.8.1-windows-x64.exe) |
| Linux, x64 | [metasafe-v0.8.1-linux-x64](https://github.com/V3rCg1/MetaSafe/releases/download/v0.8.1/metasafe-v0.8.1-linux-x64) |

On macOS or Linux, rename the downloaded executable to `metasafe`, then install it for your user:

```sh
mkdir -p "$HOME/.local/bin"
install -m 755 metasafe "$HOME/.local/bin/metasafe"
export PATH="$HOME/.local/bin:$PATH"
metasafe --version
```

Add the same `PATH` setting to your shell configuration if you want it in future terminal sessions. You can also run `./metasafe` from its folder. On Windows, rename the CLI download to `metasafe.exe` and run `.\metasafe.exe --version` in PowerShell from that folder.

### Check a download

The release includes [SHA256SUMS.txt](https://github.com/V3rCg1/MetaSafe/releases/download/v0.8.1/SHA256SUMS.txt). Compare its entry for your file with the hash calculated locally:

```sh
# macOS
shasum -a 256 MetaSafe-v0.8.1-macos-arm64.zip
# Linux
sha256sum MetaSafe_0.8.1_amd64.deb
```

```powershell
# Windows
Get-FileHash .\MetaSafe_0.8.1_x64-setup.exe -Algorithm SHA256
```

## What's new in v0.8.1

PDF loading now shares a 64 MiB decoding budget across object streams, cross-reference data and intermediate compression filters. The existing 8 MiB per-stream limit remains. A file that exceeds either limit is rejected before a cleaned copy is published.

Seven regression tests cover aggregate expansion, nested filters, indirect stream lengths, empty filter arrays and earlier PDF revisions. Normal PDF cleanup still preserves the tested page content, links and form data. See the [detailed release notes](docs/releases/v0.8.1.md) for the implementation, checks and limits.

## Features

- View photo and video metadata, including EXIF, GPS and comments when present.
- View and remove supported PDF, DOCX and XLSX properties.
- Remove supported metadata from individual files or batches and save copies alongside or separately from the originals.
- Clean static JPEG, PNG and WebP without re-encoding.
- Clean MP4, MOV, MKV and WebM without re-encoding, preserving subtitle tracks and HDR signaling by default.
- Play compatible videos in the built-in player. Playback depends on system codecs.

## Desktop app

1. In the desktop app, click **Open files** and select photos, videos or documents. You can also drag files into the window.
2. Select a file to see its metadata and a preview for supported media. Expand any row marked **Found** to see the values. EXIF fields can contain precise coordinates.
3. Click **Create cleaned copy…** for one file and choose a name. For multiple files, click **Create cleaned copies…** and choose a destination folder.
4. When processing finishes, expand the report for any file.

In batch mode, `photo.jpeg` becomes `photo_clean.jpg`. If that name already exists, MetaSafe reports an error for that file and continues with the rest. Use separate output folders for files with the same base name. Use the Theme selector in the upper-right corner to choose System, Light or Dark; the choice is saved. Use the language switcher in the upper-right corner to change the interface language.

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

- `inspect` reports supported metadata and displays available fields.
- `clean` applies EXIF orientation, saves and verifies a temporary copy beside the destination, then publishes it without overwriting an existing file. It returns an error if EXIF is malformed or orientation is ambiguous.
- `verify` returns 0 if the checks pass, 1 if metadata is found or a check is incomplete, and 2 for an input, read, or decoding error.
- `batch` returns 0 if all copies are saved, 1 if any file fails, and 2 for a command-level error such as a missing output directory.

## Image formats

| Input | Metadata inspection and verification | Default cleaned copy in the app | Keep encoding |
| --- | --- | --- | --- |
| JPEG | EXIF, GPS, XMP, IPTC, comments, ICC, and unknown or trailing data | JPEG | Static JPEG with no required transforms |
| PNG | EXIF, GPS, text, XMP, ICC, and unknown or trailing data | PNG | Static PNG with no required transforms |
| WebP | EXIF, GPS, XMP, ICC, and unknown or trailing data | PNG | Static WebP with no required transforms |

MetaSafe opens JPEG, PNG, APNG, WebP, GIF, TIFF, BMP, ICO, TGA, QOI, PNM, HDR, and supported DDS files directly. HEIC/HEIF, AVIF, PSD/PSB, JPEG 2000, JPEG XL, and supported camera RAW files use local tools.

- Install ExifTool for metadata in these additional formats.
- Install ImageMagick with the required format codecs for HEIC/HEIF, AVIF, PSD/PSB, JPEG 2000, and JPEG XL.
- Install LibRaw with `dcraw_emu` for camera RAW, including DNG, CR2/CR3, NEF, ARW, RAF, and RW2 when the camera is supported.

On macOS, Homebrew can install these tools with `brew install imagemagick exiftool libraw`. JPEG 2000 and JPEG XL require a build that includes their codecs. On Windows, put the executables on PATH. You can also set absolute executable paths in `METASAFE_MAGICK`, `METASAFE_EXIFTOOL`, and `METASAFE_DCRAW_EMU`.

Open files in the desktop app and expand a metadata row marked **Found** to see its fields. Unverified checks are labeled separately. Batch cleanup defaults to JPEG for JPEG inputs and PNG for other inputs. PNG copies retain transparency. Animated and multi-image inputs export one rendered image, without layers or animation.

## Clean without re-encoding

In the desktop app, turn on **Keep encoding** before saving a single file or batch. The CLI offers the same mode.

```sh
metasafe clean photo.webp --output photo_clean.webp --no-reencode
metasafe batch photo.jpg diagram.png image.webp --output-dir cleaned --no-reencode
```

This mode removes container metadata from static JPEG, PNG, and WebP while copying the compressed image data. The output uses the same format. PNG and WebP transparency stays intact. Files that require rotation or a color transform and animated files return a per-file error. Batch cleanup continues with other files. Removing color profiles may affect rendering in color-managed viewers.

## Video

The Tauri app and CLI accept MP4, MOV, MKV, and WebM. Install `ffmpeg` and `ffprobe` and make them available on `PATH`. Open videos in the desktop app to inspect their metadata, play compatible tracks, and save one or more cleaned copies. Playback depends on the codecs available to the system webview.

```sh
metasafe inspect clip.mkv
metasafe clean clip.mkv --output clip_clean.mkv
mkdir -p cleaned
metasafe batch clip.mkv recording.webm --output-dir cleaned
metasafe verify cleaned/recording_clean.webm
```

Default cleanup copies encoded video, audio, and subtitle tracks to the same container format. It removes supported container tags and chapters. Subtitle text and HDR signaling can carry information and are retained. Review subtitle content before sharing. Matroska files with attachments such as subtitle fonts are rejected to avoid silently changing how subtitles display.

`--strip-h264-sei` opts into removing H.264 SEI messages from encoded packets without re-encoding. Use it only for compatible SDR H.264 input. The tool rejects detectable HDR or embedded caption signaling when filtering. H.264 SEI can carry other information too, and the filter removes all SEI messages it encounters. The video size limit is under 2 GiB and each file can contain up to 32 tracks. Large files need temporary disk space for the input snapshot and cleaned copy. The metadata report does not certify removal of private data inside encoded streams or subtitle text. Other codecs, Dolby Vision, HDR10+ and custom stream payloads do not have a general in-stream cleaner.

## Documents

MetaSafe shows and removes PDF document properties and catalog XMP. For DOCX and XLSX, it handles core properties, custom properties and selected extended properties such as Company and Manager. Files are processed locally. The app saves a separate copy in the same format and verifies the supported properties again before saving.

```sh
metasafe inspect draft.docx
metasafe clean report.pdf --output report_clean.pdf
metasafe verify report_clean.pdf
metasafe batch draft.docx budget.xlsx report.pdf --output-dir cleaned
```

PDF pages, links and form data stay in the copy. Attachments, annotations, object metadata and PDF identifiers are outside the cleanup scope. Office document content, including comments, revisions, hidden sheets and links, is preserved. Cleanup refuses signed or encrypted files and PDF/A files. Word fields that depend on document properties can also prevent cleanup.

## Linux playback and builds

On Linux, the built-in player streams the selected file through a private address on the same device (`127.0.0.1`).

The CI workflow builds the CLI and the Tauri `.deb` package on Ubuntu 24.04. To build locally, install the Linux dependencies listed by [Tauri](https://v2.tauri.app/start/prerequisites/#linux), then run the Tauri steps below. For a `.deb` package, run `npm run tauri build -- --bundles deb` in `desktop/`. For the CLI alone, run `cargo build --release --locked --no-default-features` from the project root.

## Build the main desktop app

Install Node.js 22.13 or later, Rust 1.90 or later, and the [Tauri system prerequisites](https://v2.tauri.app/start/prerequisites/). On Windows these include the Microsoft C++ build tools and WebView2. On macOS, install the Xcode Command Line Tools with `xcode-select --install`. On Linux, install the packages from the linked prerequisites. Clone this repository first, then run from its root:

```sh
cd desktop
npm ci
npm run tauri dev
```

Use `npm run tauri build` from `desktop/` to create a desktop bundle for your platform.

## Build from source

Clone the repository, including its `vendor/lopdf` directory. Install Rust 1.88 or later. The following build creates the CLI and the older eframe interface; use the Tauri steps above for the main desktop app.

```sh
git clone https://github.com/V3rCg1/MetaSafe.git
cd MetaSafe
```

From the project root, run:

```sh
cargo build --release --locked --bins
```

The binaries are written to `target/release` (with `.exe` on Windows). You can also install both with `cargo install --path . --locked`. To build only the CLI, run `cargo build --release --locked --no-default-features --bin metasafe`.

## Verification and limits

Input and output photos and documents are limited to 64 MiB.

Verification reports each supported check as **Found**, **Not found**, or **Unverified**. For images, `verify` also attempts to decode the file. A standard JFIF header without a thumbnail is allowed in JPEG. Format-specific checks are listed in the image table and the video section.

The default JPEG output uses quality 90. File size, image quality, and color rendering may change. Visible details remain in the pixels, and speech, subtitles, or other on-screen text may still disclose information. A passing report does not establish anonymity.

EXIF parsing is limited to 1,024 fields, 32 TIFF directories, and an estimated 1 MiB allocation budget. If any limit is exceeded, MetaSafe marks EXIF and GPS as unverified and stops cleaning that file.

PDF loading allows up to 8 MiB per decoded stream and a shared 64 MiB budget for successful decoding outputs, including intermediate filter layers and repeated decoding across revisions. Catalog XMP is limited separately to 1 MiB. Exceeding a limit stops processing without saving a copy. These budgets do not cap total process memory.

Choose an output folder and parent directories that only trusted users can modify. MetaSafe compares the saved file with the verified copy and reports a mismatch. A process with write access can replace the file during or after this check.

Photo input and output files are limited to 64 MiB. Decoding is limited to 16,384 pixels per side and 40 megapixels. The decoder allocation limit is best-effort and does not cap total process memory. A crash can leave a temporary file in the output folder.

## Development and license

The shared Rust processing code is in `src/core`; the main Tauri and React app is in `desktop`. The PDF dependency has a local resource-budget patch documented in [vendor/lopdf/PATCHES.md](vendor/lopdf/PATCHES.md). Keep that directory when building from source.

```sh
cargo fmt --all -- --check
cargo clippy --locked --all-targets -- -D warnings
cargo test --locked
npm --prefix desktop ci
npm --prefix desktop run build
```

Video integration tests need FFmpeg and the codecs listed in `tests/video_formats.rs`; run them with `cargo test --locked --test video_formats -- --ignored`.

MetaSafe uses the [MIT license](LICENSE). The local lopdf copy retains its [MIT license](vendor/lopdf/LICENSE). Font license files are included with the relevant source and release packages.
