use std::{
    collections::HashSet,
    path::{Path, PathBuf},
    process::Command,
    sync::mpsc::{self, Receiver, TryRecvError},
    thread,
};

use eframe::egui::{self, Color32, Context, RichText, Ui, Vec2};
use metasafe::core::{
    self,
    metadata::{MetadataKind, Report, Status},
};

#[derive(Clone, Copy)]
enum Locale {
    English,
    Russian,
}

impl Locale {
    fn new(storage: Option<&dyn eframe::Storage>) -> Self {
        match storage
            .and_then(|storage| storage.get_string("language"))
            .as_deref()
        {
            Some("ru") => Self::Russian,
            Some("en") => Self::English,
            _ => {
                let locale = sys_locale::get_locale().unwrap_or_default();
                if locale.split(['-', '_']).next() == Some("ru") {
                    Self::Russian
                } else {
                    Self::English
                }
            }
        }
    }

    fn pick<'a>(self, en: &'a str, ru: &'a str) -> &'a str {
        match self {
            Self::English => en,
            Self::Russian => ru,
        }
    }

    fn kind(self, kind: MetadataKind) -> &'static str {
        use MetadataKind::*;
        match kind {
            Exif => "EXIF",
            Gps => self.pick("Location (GPS)", "Местоположение (GPS)"),
            Xmp => "XMP",
            Iptc => "IPTC",
            Comment => self.pick("JPEG comments", "Комментарии JPEG"),
            Icc => self.pick("ICC profile", "Профиль ICC"),
            Unknown => self.pick("Other JPEG segments", "Другие сегменты JPEG"),
            TrailingData => self.pick("Data after JPEG end", "Данные после конца JPEG"),
        }
    }

    fn status(self, status: Status) -> &'static str {
        match status {
            Status::NotDetected => self.pick("Not detected", "Не обнаружено"),
            Status::Present => self.pick("Detected", "Обнаружено"),
            Status::Unverified => self.pick("Could not verify", "Не удалось проверить"),
        }
    }

    fn error(self, error: &core::Error) -> String {
        match error {
            core::Error::OutputExists(_) => self
                .pick(
                    "This name is taken. Choose another name or folder.",
                    "Имя занято. Выберите другое имя или папку.",
                )
                .to_owned(),
            _ => {
                let summary = match error {
                    core::Error::Read { .. } | core::Error::NotAFile(_) => self.pick(
                        "Could not open the source file.",
                        "Не удалось открыть исходный файл.",
                    ),
                    core::Error::FileTooLarge(_) | core::Error::PixelLimit(..) => self.pick(
                        "This image exceeds the processing limits.",
                        "Изображение превышает ограничения обработки.",
                    ),
                    core::Error::Scan(_) | core::Error::NotJpeg | core::Error::Image(_) => self
                        .pick(
                            "Could not read this JPEG.",
                            "Не удалось прочитать этот JPEG.",
                        ),
                    core::Error::UnverifiedOrientation => self.pick(
                        "Could not verify EXIF orientation.",
                        "Не удалось проверить ориентацию EXIF.",
                    ),
                    core::Error::VerificationFailed => self.pick(
                        "The copy did not pass verification.",
                        "Копия не прошла проверку.",
                    ),
                    core::Error::PublishedOutputChanged(_) => self.pick(
                        "The saved file changed. Choose a trusted folder and clean again.",
                        "Содержимое сохранённого файла изменилось. Выберите доверенную папку и повторите очистку.",
                    ),
                    _ => self.pick(
                        "Could not save the copy. Check the name and destination folder.",
                        "Не удалось сохранить копию. Проверьте имя и папку назначения.",
                    ),
                };
                format!("{summary}\n{}", safe_text(&error.to_string()))
            }
        }
    }
}

struct Preview {
    report: Report,
    texture: egui::TextureHandle,
}

struct Outcome {
    source: PathBuf,
    path: PathBuf,
    result: Result<Report, core::Error>,
}

enum Update {
    Preview(Result<(Report, image::RgbaImage), core::Error>),
    Progress,
    Saved(Vec<Outcome>),
    Failed(core::Error),
}

pub struct MetaSafe {
    locale: Locale,
    paths: Vec<PathBuf>,
    selected: usize,
    preview: Option<Preview>,
    error: Option<String>,
    worker: Option<Receiver<Update>>,
    progress: usize,
    total: usize,
    outcomes: Vec<Outcome>,
}

impl MetaSafe {
    pub fn new(context: &eframe::CreationContext<'_>) -> Self {
        let mut fonts = egui::FontDefinitions::default();
        // Inter is distributed under the SIL Open Font License alongside this file.
        fonts.font_data.insert(
            "Inter".into(),
            egui::FontData::from_static(include_bytes!("fonts/Inter.ttf")).into(),
        );
        fonts
            .families
            .entry(egui::FontFamily::Proportional)
            .or_default()
            .insert(0, "Inter".into());
        context.egui_ctx.set_fonts(fonts);
        context.egui_ctx.set_theme(egui::ThemePreference::System);
        context
            .egui_ctx
            .set_visuals_of(egui::Theme::Light, palette(false));
        context
            .egui_ctx
            .set_visuals_of(egui::Theme::Dark, palette(true));
        context.egui_ctx.all_styles_mut(|style| {
            style.spacing.item_spacing = Vec2::new(8.0, 8.0);
            style.spacing.button_padding = Vec2::new(12.0, 7.0);
            for (style_key, size) in [
                (egui::TextStyle::Heading, 20.0),
                (egui::TextStyle::Body, 15.0),
                (egui::TextStyle::Button, 15.0),
                (egui::TextStyle::Small, 13.0),
            ] {
                style
                    .text_styles
                    .insert(style_key, egui::FontId::proportional(size));
            }
        });
        Self {
            locale: Locale::new(context.storage),
            paths: Vec::new(),
            selected: 0,
            preview: None,
            error: None,
            worker: None,
            progress: 0,
            total: 0,
            outcomes: Vec::new(),
        }
    }

    fn start(
        &mut self,
        ctx: &Context,
        task: impl FnOnce(mpsc::Sender<Update>, Context) + Send + 'static,
    ) {
        let (send, receive) = mpsc::channel();
        let ctx = ctx.clone();
        self.error = None;
        match thread::Builder::new()
            .name("metasafe-worker".to_owned())
            .spawn(move || {
                task(send, ctx.clone());
                ctx.request_repaint();
            }) {
            Ok(_) => self.worker = Some(receive),
            Err(error) => self.error = Some(error.to_string()),
        }
    }

    fn poll(&mut self, ctx: &Context) {
        while let Some(worker) = &self.worker {
            match worker.try_recv() {
                Ok(Update::Progress) => self.progress += 1,
                Ok(Update::Preview(result)) => {
                    self.worker = None;
                    match result {
                        Ok((report, pixels)) => {
                            let size = [pixels.width() as usize, pixels.height() as usize];
                            let image =
                                egui::ColorImage::from_rgba_unmultiplied(size, pixels.as_raw());
                            self.preview = Some(Preview {
                                report,
                                texture: ctx.load_texture(
                                    "preview",
                                    image,
                                    egui::TextureOptions::LINEAR,
                                ),
                            });
                        }
                        Err(error) => self.error = Some(self.locale.error(&error)),
                    }
                }
                Ok(Update::Saved(outcomes)) => {
                    self.worker = None;
                    self.outcomes = outcomes;
                }
                Ok(Update::Failed(error)) => {
                    self.worker = None;
                    self.error = Some(self.locale.error(&error));
                }
                Err(TryRecvError::Empty) => break,
                Err(TryRecvError::Disconnected) => {
                    self.worker = None;
                    self.error = Some(
                        self.locale
                            .pick("Processing stopped.", "Обработка прервалась.")
                            .to_owned(),
                    );
                }
            }
        }
    }

    fn open(&mut self, ctx: &Context) {
        let dialog = rfd::FileDialog::new()
            .set_title(self.locale.pick("Open JPEG files", "Открыть JPEG"))
            .add_filter("JPEG", &["jpeg", "jpg"]);
        if let Some(paths) = dialog.pick_files() {
            self.set_paths(ctx, paths);
        }
    }

    fn set_paths(&mut self, ctx: &Context, mut paths: Vec<PathBuf>) {
        let mut seen = HashSet::new();
        paths.retain(|path| seen.insert(path.clone()));
        if paths.is_empty() {
            return;
        }
        self.paths = paths;
        self.selected = 0;
        self.outcomes.clear();
        self.load_preview(ctx);
    }

    fn load_preview(&mut self, ctx: &Context) {
        self.preview = None;
        self.total = 0;
        let path = self.paths[self.selected].clone();
        self.start(ctx, move |send, _| {
            let _ = send.send(Update::Preview(core::preview(&path)));
        });
    }

    fn save(&mut self, ctx: &Context) {
        if self.paths.len() == 1 {
            let source = self.paths[0].clone();
            let suggested = format!(
                "{}_clean.jpg",
                source.file_stem().unwrap_or_default().to_string_lossy()
            );
            let mut dialog = rfd::FileDialog::new()
                .set_title(
                    self.locale
                        .pick("Create cleaned copy", "Создать очищенную копию"),
                )
                .add_filter("JPEG", &["jpg", "jpeg"])
                .set_file_name(suggested);
            if let Some(parent) = source.parent() {
                dialog = dialog.set_directory(parent);
            }
            if let Some(mut path) = dialog.save_file() {
                if path.extension().is_none() {
                    path.set_extension("jpg");
                }
                if !path
                    .extension()
                    .and_then(|value| value.to_str())
                    .is_some_and(|value| {
                        value.eq_ignore_ascii_case("jpg") || value.eq_ignore_ascii_case("jpeg")
                    })
                {
                    self.error = Some(
                        self.locale
                            .pick(
                                "Use a .jpg or .jpeg file name.",
                                "Укажите имя с расширением .jpg или .jpeg.",
                            )
                            .to_owned(),
                    );
                    return;
                }
                self.outcomes.clear();
                self.progress = 0;
                self.total = 1;
                self.start(ctx, move |send, _| {
                    let result = core::clean(&source, &path);
                    let _ = send.send(Update::Saved(vec![Outcome {
                        source,
                        path,
                        result,
                    }]));
                });
            }
            return;
        }

        let Some(folder) = rfd::FileDialog::new()
            .set_title(
                self.locale
                    .pick("Choose output folder", "Выбрать папку для копий"),
            )
            .pick_folder()
        else {
            return;
        };
        let paths = self.paths.clone();
        self.progress = 0;
        self.total = paths.len();
        self.outcomes.clear();
        self.start(ctx, move |send, ctx| {
            let report = core::batch_clean(&paths, &folder, |_| {
                let _ = send.send(Update::Progress);
                ctx.request_repaint();
            });
            let update = match report {
                Ok(report) => Update::Saved(
                    report
                        .items
                        .into_iter()
                        .map(|item| Outcome {
                            source: item.source,
                            path: item.destination,
                            result: item.result,
                        })
                        .collect(),
                ),
                Err(error) => Update::Failed(error),
            };
            let _ = send.send(update);
        });
    }

    fn image_panel(&self, ui: &mut Ui) {
        let Some(preview) = &self.preview else {
            return;
        };
        ui.vertical_centered(|ui| {
            let size = preview.texture.size_vec2();
            let available = ui.available_width();
            let max_height = (ui.ctx().screen_rect().height() * 0.52).max(180.0);
            let factor = (available / size.x).min(max_height / size.y).min(1.0);
            ui.image((preview.texture.id(), size * factor));
        });
    }

    fn details(&self, ui: &mut Ui) {
        let locale = self.locale;
        let path = &self.paths[self.selected];
        ui.heading(file_name(path))
            .on_hover_text(safe_text(&path.to_string_lossy()));
        let Some(preview) = &self.preview else {
            return;
        };
        checks(ui, locale, &preview.report, "source");
        if preview.report.orientation_unverified() {
            ui.label(locale.pick(
                "Orientation could not be verified. Choose a JPEG with valid EXIF before cleaning.",
                "Не удалось проверить ориентацию. Для очистки выберите JPEG с корректным EXIF.",
            ));
        }
        ui.collapsing(
            locale.pick("All EXIF fields", "Все поля EXIF"),
            |ui| {
                if preview.report.exif_fields.is_empty() {
                    ui.label(locale.pick("No fields to show.", "Нет полей для просмотра."));
                }
                if !preview.report.exif_fields.is_empty() {
                    egui::Grid::new("exif-fields").striped(true).show(ui, |ui| {
                        ui.strong(locale.pick("Field", "Поле"));
                        ui.strong(locale.pick("Value", "Значение"));
                        ui.strong("IFD");
                        ui.end_row();
                        for field in &preview.report.exif_fields {
                            ui.label(safe_text(&field.name));
                            ui.label(safe_text(&field.value));
                            ui.label(field.ifd.to_string());
                            ui.end_row();
                        }
                    });
                }
                if preview.report.exif_fields_truncated {
                    ui.label(locale.pick(
                        "Some values are shortened or omitted.",
                        "Часть значений сокращена или пропущена.",
                    ));
                }
            },
        );
    }

    fn results(&mut self, ui: &mut Ui) {
        if self.outcomes.is_empty() {
            return;
        }
        let locale = self.locale;
        let mut folder_error = None;
        let count = self
            .outcomes
            .iter()
            .filter(|item| item.result.is_ok())
            .count();
        ui.separator();
        ui.heading(locale.pick("Results", "Результат"));
        ui.label(format!(
            "{count}/{} {}",
            self.outcomes.len(),
            locale.pick("copies created", "копий создано")
        ));
        for (index, item) in self.outcomes.iter().enumerate() {
            ui.push_id(index, |ui| {
                let name = file_name(&item.path);
                match &item.result {
                    Ok(report) => {
                        egui::CollapsingHeader::new(format!(
                            "{} · {name}",
                            locale.pick("Created", "Создано")
                        ))
                        .default_open(self.outcomes.len() == 1)
                        .show(ui, |ui| {
                            checks(ui, locale, report, "output");
                            if ui
                                .button(locale.pick("Show in folder", "Показать в папке"))
                                .clicked()
                            {
                                let result = show_folder(&item.path);
                                if let Err(error) = result {
                                    folder_error = Some(format!(
                                        "{}\n{}",
                                        locale.pick(
                                            "Could not open the folder.",
                                            "Не удалось открыть папку."
                                        ),
                                        safe_text(&error.to_string())
                                    ));
                                }
                            }
                        });
                    }
                    Err(error) => {
                        ui.label(format!(
                            "{} · {}",
                            file_name(&item.source),
                            locale.error(error)
                        ));
                    }
                }
            });
        }
        if let Some(error) = folder_error {
            self.error = Some(error);
        }
        ui.label(
            RichText::new(locale.pick(
                "Details visible in the photo can still reveal personal information.",
                "Детали на самой фотографии могут раскрывать личные данные.",
            ))
            .small(),
        );
    }
}

impl eframe::App for MetaSafe {
    fn save(&mut self, storage: &mut dyn eframe::Storage) {
        storage.set_string("language", self.locale.pick("en", "ru").to_owned());
    }

    fn persist_egui_memory(&self) -> bool {
        false
    }

    fn update(&mut self, ctx: &Context, _frame: &mut eframe::Frame) {
        self.poll(ctx);
        let locale = self.locale;
        let busy = self.worker.is_some();
        if !busy {
            let shortcut = egui::KeyboardShortcut::new(egui::Modifiers::COMMAND, egui::Key::O);
            if ctx.input_mut(|input| input.consume_shortcut(&shortcut)) {
                self.open(ctx);
            }
            let dropped = ctx.input(|input| {
                input
                    .raw
                    .dropped_files
                    .iter()
                    .filter_map(|file| file.path.clone())
                    .collect::<Vec<_>>()
            });
            if !dropped.is_empty() && self.worker.is_none() {
                self.set_paths(ctx, dropped);
            }
        }
        let busy = self.worker.is_some();
        egui::TopBottomPanel::top("header").show(ctx, |ui| {
            ui.add_space(8.0);
            ui.horizontal(|ui| {
                ui.heading("MetaSafe");
                if ui
                    .add_enabled(
                        !busy,
                        egui::Button::new(locale.pick("Open JPEG", "Открыть JPEG")),
                    )
                    .clicked()
                {
                    self.open(ctx);
                }
                ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
                    if ui
                        .button(locale.pick("RU", "EN"))
                        .on_hover_text(locale.pick("Русский", "English"))
                        .clicked()
                    {
                        self.locale = match locale {
                            Locale::English => Locale::Russian,
                            Locale::Russian => Locale::English,
                        };
                    }
                });
            });
            ui.add_space(8.0);
        });

        let busy = self.worker.is_some();
        egui::TopBottomPanel::bottom("footer").show(ctx, |ui| {
            ui.add_space(8.0);
            let allow_save =
                !self.paths.is_empty() && (self.paths.len() > 1 || self.preview.is_some());
            let action = if self.paths.len() > 1 {
                locale.pick("Create cleaned copies…", "Создать очищенные копии…")
            } else {
                locale.pick("Create cleaned copy…", "Создать очищенную копию…")
            };
            ui.horizontal_wrapped(|ui| {
                if ui
                    .add_enabled(
                        !busy && allow_save,
                        egui::Button::new(RichText::new(action).color(Color32::WHITE))
                            .fill(Color32::from_rgb(49, 85, 212)),
                    )
                    .clicked()
                {
                    self.save(ctx);
                }
                ui.label(
                    RichText::new(
                        locale.pick("Processed on your device", "Обработка на устройстве"),
                    )
                    .small(),
                );
            });
            ui.add_space(8.0);
        });

        egui::CentralPanel::default().show(ctx, |ui| {
            egui::ScrollArea::vertical().show(ui, |ui| {
                ui.add_space(12.0);
                if self.paths.is_empty() {
                    ui.vertical_centered(|ui| {
                        ui.add_space(56.0);
                        ui.heading(locale.pick("Drop JPEG files here", "Перетащите JPEG сюда"));
                        ui.label(locale.pick(
                            "Or select photos in the file picker.",
                            "Или выберите фотографии в диалоге.",
                        ));
                    });
                    return;
                }
                if self.paths.len() > 1 {
                    let previous = self.selected;
                    ui.add_enabled_ui(!busy, |ui| {
                        egui::ComboBox::from_label(
                            locale.pick("Preview file", "Файл для просмотра"),
                        )
                        .selected_text(file_name(&self.paths[self.selected]))
                        .show_ui(ui, |ui| {
                            for (index, path) in self.paths.iter().enumerate() {
                                ui.selectable_value(&mut self.selected, index, file_name(path))
                                    .on_hover_text(safe_text(&path.to_string_lossy()));
                            }
                        });
                    });
                    if self.selected != previous {
                        self.load_preview(ctx);
                    }
                }
                if self.worker.is_some() {
                    if self.total == 0 {
                        ui.label(locale.pick("Loading preview…", "Загрузка превью…"));
                    } else {
                        ui.add(
                            egui::ProgressBar::new(self.progress as f32 / self.total as f32).text(
                                format!(
                                    "{} {}/{}",
                                    locale.pick("Processed", "Обработано"),
                                    self.progress,
                                    self.total
                                ),
                            ),
                        );
                    }
                }
                if let Some(error) = &self.error {
                    ui.label(RichText::new(error).strong());
                }
                if ui.available_width() < 860.0 {
                    self.image_panel(ui);
                    self.details(ui);
                } else {
                    let left_width = ui.available_width() * 0.55;
                    ui.horizontal_top(|ui| {
                        ui.allocate_ui_with_layout(
                            Vec2::new(left_width, 0.0),
                            egui::Layout::top_down(egui::Align::Min),
                            |ui| {
                                ui.set_max_width(left_width);
                                self.image_panel(ui);
                            },
                        );
                        ui.vertical(|ui| self.details(ui));
                    });
                }
                self.results(ui);
            });
        });
    }
}

fn palette(dark: bool) -> egui::Visuals {
    let mut visuals = if dark {
        egui::Visuals::dark()
    } else {
        egui::Visuals::light()
    };
    let (background, surface, text, accent) = if dark {
        ([11, 27, 59], [23, 43, 82], [247, 249, 255], [133, 160, 255])
    } else {
        (
            [245, 247, 252],
            [255, 255, 255],
            [11, 27, 59],
            [49, 85, 212],
        )
    };
    let rgb = |[r, g, b]: [u8; 3]| Color32::from_rgb(r, g, b);
    visuals.panel_fill = rgb(background);
    visuals.window_fill = rgb(surface);
    visuals.extreme_bg_color = rgb(surface);
    visuals.override_text_color = Some(rgb(text));
    visuals.hyperlink_color = rgb(accent);
    visuals.selection.stroke.color = rgb(accent);
    visuals
}

fn checks(ui: &mut Ui, locale: Locale, report: &Report, id: &str) {
    egui::Grid::new(id).striped(true).show(ui, |ui| {
        for check in &report.checks {
            ui.label(locale.kind(check.kind));
            ui.label(locale.status(check.status));
            ui.end_row();
        }
    });
}

fn file_name(path: &Path) -> String {
    let name = path
        .file_name()
        .unwrap_or(path.as_os_str())
        .to_string_lossy();
    safe_text(&name)
}

fn safe_text(text: &str) -> String {
    text.chars()
        .map(|character| match character {
            '\u{061c}'
            | '\u{200e}'
            | '\u{200f}'
            | '\u{2028}'..='\u{202e}'
            | '\u{2066}'..='\u{2069}' => '�',
            _ if character.is_control() => '�',
            _ => character,
        })
        .collect()
}

fn show_folder(path: &Path) -> std::io::Result<()> {
    let folder = path
        .parent()
        .filter(|path| !path.as_os_str().is_empty())
        .unwrap_or_else(|| Path::new("."));
    let folder = folder.canonicalize()?;
    #[cfg(target_os = "macos")]
    let mut command = Command::new("/usr/bin/open");
    #[cfg(target_os = "windows")]
    let mut command = Command::new(
        std::path::Path::new(
            &std::env::var_os("SystemRoot")
                .ok_or_else(|| std::io::Error::other("SystemRoot is unavailable"))?,
        )
        .join("explorer.exe"),
    );
    #[cfg(not(any(target_os = "macos", target_os = "windows")))]
    let mut command = Command::new("xdg-open");
    command.arg(folder).spawn().map(|_| ())
}
