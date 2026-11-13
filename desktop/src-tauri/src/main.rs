#![cfg_attr(not(debug_assertions), windows_subsystem = "windows")]

use std::{
    collections::HashSet,
    fs::File,
    io::{Read, Seek, SeekFrom},
    path::{Path, PathBuf},
    process::Command,
    sync::{
        Mutex, MutexGuard,
        atomic::{AtomicBool, Ordering},
    },
};

use base64::{Engine, engine::general_purpose::STANDARD};
use image::{ImageEncoder, codecs::png::PngEncoder};
use metasafe::core::{
    self,
    metadata::{Report, Status},
};
use rfd::AsyncFileDialog;
use serde::Serialize;
use tauri::{
    DragDropEvent, Emitter, Manager, State, WindowEvent,
    http::{Method, Request, Response, StatusCode, header},
    ipc::Channel,
};

#[cfg(target_os = "linux")]
mod media_loopback;

type AppResult<T> = Result<T, String>;

#[derive(Default)]
struct Workspace {
    selection: Mutex<Selection>,
    output: Mutex<Option<PathBuf>>,
    cleaning: AtomicBool,
    processing: tauri::async_runtime::Mutex<()>,
    playback: std::sync::Arc<Mutex<Option<Playback>>>,
    #[cfg(target_os = "linux")]
    playback_port: std::sync::OnceLock<u16>,
}

#[derive(Default)]
struct Selection {
    revision: u32,
    files: Vec<PathBuf>,
}

#[derive(Serialize, Clone)]
struct SelectedFiles {
    revision: u32,
    names: Vec<String>,
    videos: Vec<bool>,
}

struct Playback {
    token: String,
    file: File,
    length: u64,
    mime: &'static str,
}

impl Workspace {
    fn replace_selection(&self, files: Vec<PathBuf>) -> AppResult<SelectedFiles> {
        let mut selection = lock(&self.selection)?;
        *lock(&self.playback)? = None;
        let mut seen = HashSet::new();
        selection.files = files
            .into_iter()
            .filter(|path| seen.insert(path.clone()))
            .collect();
        selection.revision = selection.revision.wrapping_add(1);
        Ok(SelectedFiles {
            revision: selection.revision,
            names: selection.files.iter().map(|path| filename(path)).collect(),
            videos: selection
                .files
                .iter()
                .map(|path| core::video::is_video(path))
                .collect(),
        })
    }

    fn selection(&self, revision: u32) -> AppResult<MutexGuard<'_, Selection>> {
        let selection = lock(&self.selection)?;
        if selection.revision != revision || selection.files.is_empty() {
            return Err("The file selection changed.".into());
        }
        Ok(selection)
    }
}

fn lock<T>(mutex: &Mutex<T>) -> AppResult<MutexGuard<'_, T>> {
    mutex.lock().map_err(|_| "Workspace is unavailable.".into())
}

fn filename(path: &Path) -> String {
    path.file_name()
        .unwrap_or(path.as_os_str())
        .to_string_lossy()
        .into_owned()
}

#[derive(Serialize)]
struct MetadataReport {
    checks: Vec<(&'static str, &'static str)>,
    format: String,
    single_frame_export: bool,
    details: Vec<(&'static str, String, String)>,
    truncated: bool,
    orientation_unverified: bool,
}

impl From<Report> for MetadataReport {
    fn from(report: Report) -> Self {
        let orientation_unverified = report.orientation_unverified();
        Self {
            format: report.format,
            single_frame_export: report.single_frame_export,
            checks: report
                .checks
                .into_iter()
                .map(|check| {
                    let status = match check.status {
                        Status::NotDetected => "clear",
                        Status::Present => "present",
                        Status::Unverified => "unknown",
                    };
                    (metadata_key(check.kind), status)
                })
                .collect(),
            details: report
                .details
                .into_iter()
                .map(|detail| (metadata_key(detail.kind), detail.name, detail.value))
                .collect(),
            truncated: report.exif_fields_truncated,
            orientation_unverified,
        }
    }
}

fn metadata_key(kind: core::metadata::MetadataKind) -> &'static str {
    use core::metadata::MetadataKind::*;
    match kind {
        Exif => "exif",
        Gps => "gps",
        Xmp => "xmp",
        Iptc => "iptc",
        Comment => "comment",
        Icc => "icc",
        Unknown => "unknown",
        TrailingData => "trailing",
    }
}

#[derive(Serialize)]
struct Preview {
    image: Option<String>,
    video: Option<String>,
    video_port: Option<u16>,
    report: MetadataReport,
}

const MEDIA_CHUNK: u64 = 1024 * 1024;

fn byte_range(value: &str, length: u64) -> Option<(u64, u64)> {
    let (start, end) = value.strip_prefix("bytes=")?.split_once('-')?;
    let last = length.checked_sub(1)?;
    let (start, end) = if start.is_empty() {
        (length.saturating_sub(end.parse().ok()?), last)
    } else {
        let end = if end.is_empty() {
            last
        } else {
            end.parse::<u64>().ok()?.min(last)
        };
        (start.parse::<u64>().ok()?, end)
    };
    (start <= end).then_some((start, end))
}

fn empty_response(status: StatusCode) -> Response<Vec<u8>> {
    let mut response = Response::new(Vec::new());
    *response.status_mut() = status;
    response
}

fn video_response(workspace: &Workspace, request: Request<Vec<u8>>) -> Response<Vec<u8>> {
    if request.method() != Method::GET && request.method() != Method::HEAD {
        return empty_response(StatusCode::METHOD_NOT_ALLOWED);
    }
    let Ok(mut playback) = workspace.playback.lock() else {
        return empty_response(StatusCode::INTERNAL_SERVER_ERROR);
    };
    let Some(media) = active_playback(&mut playback, request.uri().path()) else {
        return empty_response(StatusCode::NOT_FOUND);
    };
    if media.length == 0 {
        return empty_response(StatusCode::NOT_FOUND);
    }
    let mut response = Response::builder()
        .header(header::CONTENT_TYPE, media.mime)
        .header(header::CACHE_CONTROL, "no-store")
        .header(header::X_CONTENT_TYPE_OPTIONS, "nosniff")
        .header(header::ACCEPT_RANGES, "bytes");
    if request.method() == Method::HEAD {
        return response
            .header(header::CONTENT_LENGTH, media.length)
            .body(Vec::new())
            .unwrap_or_else(|_| empty_response(StatusCode::INTERNAL_SERVER_ERROR));
    }
    let range = request.headers().get(header::RANGE);
    let (start, end) = if let Some(value) = range {
        let Some(range) = value
            .to_str()
            .ok()
            .and_then(|value| byte_range(value, media.length))
        else {
            return response
                .status(StatusCode::RANGE_NOT_SATISFIABLE)
                .header(header::CONTENT_RANGE, format!("bytes */{}", media.length))
                .body(Vec::new())
                .unwrap_or_else(|_| empty_response(StatusCode::INTERNAL_SERVER_ERROR));
        };
        range
    } else if media.length > MEDIA_CHUNK {
        (0, MEDIA_CHUNK - 1)
    } else {
        (0, media.length - 1)
    };
    let end = end.min(start.saturating_add(MEDIA_CHUNK - 1));
    if range.is_some() || media.length > MEDIA_CHUNK {
        response = response.status(StatusCode::PARTIAL_CONTENT).header(
            header::CONTENT_RANGE,
            format!("bytes {start}-{end}/{}", media.length),
        );
    }
    let mut bytes = vec![0; (end - start + 1) as usize];
    if media
        .file
        .seek(SeekFrom::Start(start))
        .and_then(|_| media.file.read_exact(&mut bytes))
        .is_err()
    {
        return empty_response(StatusCode::INTERNAL_SERVER_ERROR);
    }
    response
        .header(header::CONTENT_LENGTH, bytes.len())
        .body(bytes)
        .unwrap_or_else(|_| empty_response(StatusCode::INTERNAL_SERVER_ERROR))
}

fn active_playback<'a>(playback: &'a mut Option<Playback>, path: &str) -> Option<&'a mut Playback> {
    let media = playback.as_mut()?;
    (path.strip_prefix('/')? == media.token).then_some(media)
}

#[derive(Serialize)]
struct Outcome {
    source: String,
    destination: String,
    report: Option<MetadataReport>,
    error: Option<String>,
}

fn outcome(source: &Path, destination: &Path, result: Result<Report, core::Error>) -> Outcome {
    let (report, error) = match result {
        Ok(report) => (Some(report.into()), None),
        Err(error) => (None, Some(error.to_string())),
    };
    Outcome {
        source: filename(source),
        destination: filename(destination),
        report,
        error,
    }
}

#[tauri::command]
async fn choose_files(workspace: State<'_, Workspace>) -> AppResult<Option<SelectedFiles>> {
    let dialog = AsyncFileDialog::new();
    // rfd maps extension filters to NSOpenPanel's allowed file types on macOS.
    // Let the core identify the selected files when macOS cannot resolve a type.
    #[cfg(not(target_os = "macos"))]
    let dialog = {
        let extensions = [core::EXTENSIONS, core::video::EXTENSIONS].concat();
        dialog.add_filter("Photos and videos", &extensions)
    };
    let chosen = dialog.pick_files().await;
    chosen
        .map(|files| {
            workspace.replace_selection(files.iter().map(|file| file.path().to_owned()).collect())
        })
        .transpose()
}

#[tauri::command]
async fn preview_file(
    workspace: State<'_, Workspace>,
    revision: u32,
    index: usize,
) -> AppResult<Preview> {
    let _processing = workspace.processing.lock().await;
    let path = {
        let selection = workspace.selection(revision)?;
        let path = selection
            .files
            .get(index)
            .cloned()
            .ok_or("File is unavailable.")?;
        *lock(&workspace.playback)? = None;
        path
    };
    if core::video::is_video(&path) {
        let (report, media) = tauri::async_runtime::spawn_blocking(move || {
            let (file, metadata) =
                core::open_regular_file(&path).map_err(|error| error.to_string())?;
            let report = core::video::inspect(&path).map_err(|error| error.to_string())?;
            let mime = match path
                .extension()
                .and_then(|ext| ext.to_str())
                .map(str::to_ascii_lowercase)
                .as_deref()
            {
                Some("webm") => "video/webm",
                Some("mkv") => "video/x-matroska",
                Some("mov") => "video/quicktime",
                _ => "video/mp4",
            };
            let mut random = [0_u8; 24];
            getrandom::fill(&mut random).map_err(|_| "Could not authorize video playback.")?;
            Ok::<_, String>((
                report,
                Playback {
                    token: base64::engine::general_purpose::URL_SAFE_NO_PAD.encode(random),
                    file,
                    length: metadata.len(),
                    mime,
                },
            ))
        })
        .await
        .map_err(|error| error.to_string())??;
        let _selection = workspace.selection(revision)?;
        #[cfg(target_os = "linux")]
        let video_port = workspace.playback_port.get().copied();
        #[cfg(not(target_os = "linux"))]
        let video_port = None;
        #[cfg(target_os = "linux")]
        if video_port.is_none() {
            return Ok(Preview {
                image: None,
                video: None,
                video_port,
                report: report.into(),
            });
        }
        let token = media.token.clone();
        *lock(&workspace.playback)? = Some(media);
        return Ok(Preview {
            image: None,
            video: Some(token),
            video_port,
            report: report.into(),
        });
    }
    tauri::async_runtime::spawn_blocking(move || {
        let (report, rgba) = core::preview(&path).map_err(|error| error.to_string())?;
        let mut bytes = Vec::new();
        PngEncoder::new(&mut bytes)
            .write_image(
                &rgba,
                rgba.width(),
                rgba.height(),
                image::ExtendedColorType::Rgba8,
            )
            .map_err(|error| error.to_string())?;
        Ok(Preview {
            image: Some(format!("data:image/png;base64,{}", STANDARD.encode(bytes))),
            video: None,
            video_port: None,
            report: report.into(),
        })
    })
    .await
    .map_err(|error| error.to_string())?
}

struct CleaningGuard<'a>(&'a AtomicBool);
impl Drop for CleaningGuard<'_> {
    fn drop(&mut self) {
        self.0.store(false, Ordering::Release);
    }
}

#[tauri::command]
async fn clean_files(
    workspace: State<'_, Workspace>,
    revision: u32,
    progress: Channel<usize>,
    no_reencode: bool,
    strip_h264_sei: bool,
) -> AppResult<Option<Vec<Outcome>>> {
    let mode = if no_reencode {
        core::CleanMode::KeepEncoding
    } else {
        core::CleanMode::Reencode
    };
    if workspace.cleaning.swap(true, Ordering::AcqRel) {
        return Err("Cleaning is already running.".into());
    }
    let _guard = CleaningGuard(&workspace.cleaning);
    let files = workspace.selection(revision)?.files.clone();
    let _processing = workspace.processing.lock().await;

    let (folder, results) = if let [source] = files.as_slice() {
        let extension = core::suggested_output_extension_for_mode(source, mode);
        let suggested = format!(
            "{}_clean.{extension}",
            source.file_stem().unwrap_or_default().to_string_lossy()
        );
        let dialog = AsyncFileDialog::new()
            .add_filter("Cleaned copy", &[extension])
            .set_file_name(suggested);
        let dialog = match source.parent() {
            Some(parent) => dialog.set_directory(parent),
            None => dialog,
        };
        let Some(file) = dialog.save_file().await else {
            return Ok(None);
        };
        let mut destination = file.path().to_owned();
        if destination.extension().is_none() {
            destination.set_extension(extension);
        }
        let folder = destination
            .parent()
            .ok_or("Output folder is unavailable.")?
            .to_owned();
        let source = source.clone();
        let result = tauri::async_runtime::spawn_blocking(move || {
            let result = outcome(
                &source,
                &destination,
                core::clean_with_options(&source, &destination, mode, strip_h264_sei),
            );
            let _ = progress.send(1);
            vec![result]
        })
        .await
        .map_err(|error| error.to_string())?;
        (folder, result)
    } else {
        let Some(folder) = AsyncFileDialog::new().pick_folder().await else {
            return Ok(None);
        };
        let folder = folder.path().to_owned();
        let destination = folder.clone();
        let results = tauri::async_runtime::spawn_blocking(move || {
            let mut completed = 0;
            let batch =
                core::batch_clean_with_options(files, &destination, mode, strip_h264_sei, |_| {
                    completed += 1;
                    let _ = progress.send(completed);
                })
                .map_err(|error| error.to_string())?;
            Ok::<_, String>(
                batch
                    .items
                    .into_iter()
                    .map(|item| outcome(&item.source, &item.destination, item.result))
                    .collect(),
            )
        })
        .await
        .map_err(|error| error.to_string())??;
        (folder, results)
    };

    if results.iter().any(|item| item.report.is_some()) {
        *lock(&workspace.output)? = Some(folder);
    }
    Ok(Some(results))
}

#[tauri::command]
fn show_output(workspace: State<'_, Workspace>) -> AppResult<()> {
    let path = lock(&workspace.output)?
        .clone()
        .ok_or("No output folder.")?;
    #[cfg(target_os = "macos")]
    let mut command = Command::new("/usr/bin/open");
    #[cfg(target_os = "windows")]
    let mut command = Command::new(
        PathBuf::from(std::env::var_os("SystemRoot").ok_or("SystemRoot is unavailable.")?)
            .join("explorer.exe"),
    );
    #[cfg(not(any(target_os = "macos", target_os = "windows")))]
    let mut command = Command::new("xdg-open");
    let child = command
        .arg(path)
        .spawn()
        .map_err(|error| error.to_string())?;
    tauri::async_runtime::spawn_blocking(move || {
        let mut child = child;
        let _ = child.wait();
    });
    Ok(())
}

fn main() {
    tauri::Builder::default()
        .manage(Workspace::default())
        .setup(|app| {
            #[cfg(target_os = "linux")]
            {
                let workspace = app.state::<Workspace>();
                if let Ok(port) = media_loopback::start(workspace.playback.clone()) {
                    let _ = workspace.playback_port.set(port);
                }
            }
            #[cfg(not(target_os = "linux"))]
            let _ = app;
            Ok(())
        })
        .register_asynchronous_uri_scheme_protocol(
            "metasafe-video",
            |context, request, responder| {
                if context.webview_label() != "main" {
                    responder.respond(empty_response(StatusCode::NOT_FOUND));
                    return;
                }
                let app = context.app_handle().clone();
                tauri::async_runtime::spawn_blocking(move || {
                    responder.respond(video_response(&app.state::<Workspace>(), request));
                });
            },
        )
        .on_window_event(|window, event| {
            if let WindowEvent::DragDrop(DragDropEvent::Drop { paths, .. }) = event {
                let workspace = window.state::<Workspace>();
                if !workspace.cleaning.load(Ordering::Acquire) && !paths.is_empty() {
                    if let Ok(selection) = workspace.replace_selection(paths.clone()) {
                        let _ = window.emit("metasafe-files", selection);
                    }
                }
            }
        })
        .invoke_handler(tauri::generate_handler![
            choose_files,
            preview_file,
            clean_files,
            show_output
        ])
        .run(tauri::generate_context!())
        .expect("could not start MetaSafe");
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn range_is_bounded_and_invalid_offsets_are_rejected() {
        assert_eq!(byte_range("bytes=0-1", 12), Some((0, 1)));
        assert_eq!(byte_range("bytes=-4", 12), Some((8, 11)));
        assert_eq!(
            byte_range("bytes=0-", MEDIA_CHUNK * 2),
            Some((0, MEDIA_CHUNK * 2 - 1))
        );
        assert_eq!(byte_range("bytes=12-", 12), None);
        assert_eq!(byte_range("bytes=-0", 12), None);
        assert_eq!(byte_range("bytes=8-2", 12), None);
        assert_eq!(byte_range("bytes=0-1,4-8", 12), None);
    }

    #[test]
    fn media_endpoint_serves_the_active_file_only() {
        const PATH: &str = concat!(env!("CARGO_MANIFEST_DIR"), "/Cargo.toml");
        let file = File::open(PATH).unwrap();
        let workspace = Workspace {
            playback: std::sync::Arc::new(Mutex::new(Some(Playback {
                token: "active".into(),
                length: file.metadata().unwrap().len(),
                file,
                mime: "video/mp4",
            }))),
            ..Workspace::default()
        };
        let range = |path| {
            video_response(
                &workspace,
                Request::get(path)
                    .header(header::RANGE, "bytes=0-2")
                    .body(Vec::new())
                    .unwrap(),
            )
        };
        let active = range("/active");
        assert_eq!(active.status(), StatusCode::PARTIAL_CONTENT);
        assert_eq!(active.body(), &std::fs::read(PATH).unwrap()[..3]);
        assert_eq!(range("/inactive").status(), StatusCode::NOT_FOUND);
        workspace.replace_selection(Vec::new()).unwrap();
        assert_eq!(range("/active").status(), StatusCode::NOT_FOUND);
    }
}
