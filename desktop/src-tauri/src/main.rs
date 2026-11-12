#![cfg_attr(not(debug_assertions), windows_subsystem = "windows")]

use std::{
    collections::HashSet,
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
use tauri::{DragDropEvent, Emitter, Manager, State, WindowEvent, ipc::Channel};

type AppResult<T> = Result<T, String>;

#[derive(Default)]
struct Workspace {
    selection: Mutex<Selection>,
    output: Mutex<Option<PathBuf>>,
    cleaning: AtomicBool,
    processing: tauri::async_runtime::Mutex<()>,
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
}

impl Workspace {
    fn replace_selection(&self, files: Vec<PathBuf>) -> AppResult<SelectedFiles> {
        let mut selection = lock(&self.selection)?;
        let mut seen = HashSet::new();
        selection.files = files
            .into_iter()
            .filter(|path| seen.insert(path.clone()))
            .collect();
        selection.revision = selection.revision.wrapping_add(1);
        Ok(SelectedFiles {
            revision: selection.revision,
            names: selection.files.iter().map(|path| filename(path)).collect(),
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
    image: String,
    report: MetadataReport,
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
    let chosen = AsyncFileDialog::new()
        .add_filter("Images", core::EXTENSIONS)
        .pick_files()
        .await;
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
    let path = workspace
        .selection(revision)?
        .files
        .get(index)
        .cloned()
        .ok_or("File is unavailable.")?;
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
            image: format!("data:image/png;base64,{}", STANDARD.encode(bytes)),
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
) -> AppResult<Option<Vec<Outcome>>> {
    if workspace.cleaning.swap(true, Ordering::AcqRel) {
        return Err("Cleaning is already running.".into());
    }
    let _guard = CleaningGuard(&workspace.cleaning);
    let files = workspace.selection(revision)?.files.clone();
    let _processing = workspace.processing.lock().await;

    let (folder, results) = if let [source] = files.as_slice() {
        let extension = core::suggested_output_extension(source);
        let suggested = format!(
            "{}_clean.{extension}",
            source.file_stem().unwrap_or_default().to_string_lossy()
        );
        let dialog = AsyncFileDialog::new()
            .add_filter("Image copy", &[extension])
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
            let result = outcome(&source, &destination, core::clean(&source, &destination));
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
            let batch = core::batch_clean(files, &destination, |_| {
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
