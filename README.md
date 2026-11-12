# MetaSafe v0.5.0

[Русский](README.ru.md) · **English**

MetaSafe is an open-source desktop app and CLI for inspecting photo metadata and saving verified copies. Processing stays on your device, and the originals are preserved. The source is available under the [MIT License](LICENSE).

## Quick start

To run v0.5.0, [build the Tauri app from source](#run-the-tauri-interface). The instructions below cover the published v0.3.0 archives. [Download v0.3.0](https://github.com/V3rCg1/MetaSafe/releases/tag/v0.3.0)

### macOS on Apple Silicon

Open Terminal, go to the `package` folder inside the extracted archive, and launch the desktop app:

```sh
./metasafe-gui
```

For the command-line interface, run `./metasafe` from the same folder. The binaries lack a publisher signature, so macOS may display a developer warning.

### Windows x64

Double-click `metasafe-gui.exe` in the extracted folder. For the command-line interface, open PowerShell in the same folder and run `.\metasafe.exe`. The binaries lack a publisher signature, so Windows may display a publisher warning.

## Desktop app

1. Click **Open JPEG** and select one or more `.jpg` or `.jpeg` files. You can also drag files into the window.
2. Select a photo to see its preview and metadata. EXIF fields can contain precise coordinates.
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

## Run the Tauri interface

Install Node.js, Rust, and the [Tauri system prerequisites](https://v2.tauri.app/start/prerequisites/). From the project root, run:

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

Input and output files are limited to 64 MiB. Decoding is limited to 16,384 pixels per side and 40 megapixels. The decoder allocation limit is best-effort and does not cap total process memory. A crash can leave a temporary file in the output folder.
