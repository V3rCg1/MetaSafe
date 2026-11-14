import { Channel, convertFileSrc, invoke, isTauri } from "@tauri-apps/api/core";
import { listen } from "@tauri-apps/api/event";
import {
  AlertCircle, Check, ChevronDown, FileImage, FolderOpen, Image, LockKeyhole,
  Monitor, Moon, Plus, ShieldCheck, Sparkles, Sun, Upload, Video,
} from "lucide-react";
import { motion, useReducedMotion } from "motion/react";
import { useEffect, useLayoutEffect, useState } from "react";
import "@fontsource-variable/geist";
import "./App.css";

type Theme = "system" | "light" | "dark";
type Language = "en" | "ru";
type Status = "clear" | "present" | "unknown";
type Report = {
  checks: [string, Status][];
  format: string;
  single_frame_export: boolean;
  details: [string, string, string][];
  truncated: boolean;
  orientation_unverified: boolean;
};
type Selection = { revision: number; names: string[]; videos: boolean[] };
type Preview = { image: string | null; video: string | null; video_port: number | null; report: Report };
type Outcome = { source: string; destination: string; report: Report | null; error: string | null };
type LoadedPreview = { revision: number; index: number } & (
  { data: Preview; error?: never } | { data?: never; error: string }
);

const messages = {
  en: {
    theme: "Theme", system: "System", light: "Light", dark: "Dark",
    workspace: "Photos and videos", open: "Open files", files: "Files", noFiles: "No files selected",
    preview: "Preview", emptyTitle: "Open a photo or video", emptyText: "Drop files here or choose them from your device.",
    metadata: "Metadata", emptyMeta: "Open a file to see which metadata it contains.",
    noDetails: "Individual fields are unavailable.", clear: "Not found", present: "Found", unknown: "Unverified",
    keepEncoding: "Keep encoding",
    keepEncodingHint: "Static JPEG, PNG and WebP in the original format",
    videoScope: "Verification covers container and track metadata. Subtitles, picture and sound can contain personal information.",
    stripSei: "Remove H.264 SEI messages",
    stripSeiHint: "Blocked if embedded captions or HDR are detected. Other playback information may change.",
    videoUnsupported: "This video cannot play in the system player. You can still clean it.",
    clean: "Create cleaned copy", batch: "Create cleaned copies", working: "Processing",
    singleFrame: "The copy contains one rendered image.",
    local: "On-device processing", original: "Originals are preserved",
    results: "Verification results", saved: "Verified", failed: "Failed", folder: "Open output folder",
    noPreview: "Preview unavailable", loading: "Reading file…", browser: "Open MetaSafe in Tauri to select files.",
    orientation: "EXIF orientation could not be verified.", truncated: "Some EXIF fields were omitted or shortened.",
    checks: { exif: "EXIF", gps: "Location", xmp: "XMP", iptc: "IPTC", comment: "Comments", icc: "ICC profile", unknown: "Other segments", trailing: "Trailing data" },
  },
  ru: {
    theme: "Тема", system: "Системная", light: "Светлая", dark: "Тёмная",
    workspace: "Фото и видео", open: "Открыть файлы", files: "Файлы", noFiles: "Файлы не выбраны",
    preview: "Превью", emptyTitle: "Откройте фото или видео", emptyText: "Перетащите файлы сюда или выберите их на устройстве.",
    metadata: "Метаданные", emptyMeta: "Откройте файл, чтобы увидеть его метаданные.",
    noDetails: "Отдельные поля недоступны.", clear: "Не найдено", present: "Найдено", unknown: "Не проверено",
    keepEncoding: "Без перекодирования",
    keepEncodingHint: "Статические JPEG, PNG и WebP в исходном формате",
    videoScope: "Проверяются метаданные контейнера и дорожек. Субтитры, изображение и звук могут содержать личные данные.",
    stripSei: "Удалить сообщения H.264 SEI",
    stripSeiHint: "При обнаружении встроенных субтитров или HDR операция блокируется. Другие данные воспроизведения могут измениться.",
    videoUnsupported: "Системный плеер не воспроизводит это видео. Его всё равно можно очистить.",
    clean: "Создать очищенную копию", batch: "Создать очищенные копии", working: "Обработка",
    singleFrame: "Копия содержит одно изображение.",
    local: "Обработка на устройстве", original: "Оригиналы сохраняются",
    results: "Результаты проверки", saved: "Проверено", failed: "Ошибка", folder: "Открыть папку",
    noPreview: "Превью недоступно", loading: "Чтение файла…", browser: "Запустите MetaSafe в Tauri, чтобы выбрать файлы.",
    orientation: "Не удалось проверить ориентацию EXIF.", truncated: "Часть полей EXIF скрыта или сокращена.",
    checks: { exif: "EXIF", gps: "Местоположение", xmp: "XMP", iptc: "IPTC", comment: "Комментарии", icc: "Профиль ICC", unknown: "Другие сегменты", trailing: "Данные после изображения" },
  },
} as const;

type Messages = (typeof messages)[Language];

function preferredLanguage(): Language {
  try {
    const saved = localStorage.getItem("metasafe.language");
    if (saved === "en" || saved === "ru") return saved;
  } catch {
    // The system language is available even when storage is disabled.
  }
  return navigator.language.toLowerCase().startsWith("ru") ? "ru" : "en";
}

function preferredTheme(): Theme {
  try {
    const saved = localStorage.getItem("metasafe.theme");
    if (saved === "light" || saved === "dark") return saved;
  } catch {
    // The system theme is available even when storage is disabled.
  }
  return "system";
}

function Checks({ report, t }: { report: Report; t: Messages }) {
  return (
    <ul className="checks">
      {report.checks.map(([key, status]) => {
        const label = t.checks[key as keyof Messages["checks"]] ?? key;
        const details = report.details.filter(([group]) => group === key);
        const statusLabel = <span className={`status ${status}`}><i aria-hidden="true" />{t[status]}</span>;
        return (
          <li key={key}>
            {status === "present" || details.length > 0 ? (
              <details className="metadata-disclosure">
                <summary><span>{label}</span>{statusLabel}<ChevronDown size={13} aria-hidden="true" /></summary>
                {details.length ? (
                  <dl className="metadata-values">
                    {details.map(([, name, value], position) => (
                      <div key={position}><dt>{name}</dt><dd>{value}</dd></div>
                    ))}
                  </dl>
                ) : <p className="panel-note">{t.noDetails}</p>}
                {key === "exif" && report.truncated ? <p className="panel-note">{t.truncated}</p> : null}
              </details>
            ) : (
              <div className="metadata-status"><span>{label}</span>{statusLabel}</div>
            )}
          </li>
        );
      })}
    </ul>
  );
}

function VideoPreview({ token, port, name, t }: { token: string | null; port: number | null; name: string; t: Messages }) {
  const [unavailable, setUnavailable] = useState(false);
  if (unavailable || !token) {
    return (
      <div className="stage-message" role="status">
        <AlertCircle size={22} aria-hidden="true" />
        <strong>{t.noPreview}</strong>
        <p>{t.videoUnsupported}</p>
      </div>
    );
  }
  const src = port === null ? convertFileSrc(token, "metasafe-video") : `http://127.0.0.1:${port}/${token}`;
  return (
    <video src={src} controls preload="metadata"
      aria-label={name} onError={() => setUnavailable(true)} />
  );
}

export default function App() {
  const [theme, setTheme] = useState(preferredTheme);
  const [language, setLanguage] = useState(preferredLanguage);
  const [selection, setSelection] = useState<Selection | null>(null);
  const [index, setIndex] = useState(0);
  const [loaded, setLoaded] = useState<LoadedPreview | null>(null);
  const [noReencode, setNoReencode] = useState(false);
  const [stripH264Sei, setStripH264Sei] = useState(false);
  const [action, setAction] = useState<"open" | "clean" | null>(null);
  const [progress, setProgress] = useState(0);
  const [outcomes, setOutcomes] = useState<Outcome[]>([]);
  const [error, setError] = useState("");
  const reduceMotion = useReducedMotion();
  const t = messages[language];
  const desktop = isTauri();
  const revision = selection?.revision;
  const current = loaded?.revision === revision && loaded?.index === index ? loaded : null;
  const activeName = selection?.names[index];
  const activeVideo = selection?.videos[index] ?? false;
  const count = selection?.names.length ?? 0;
  const hasVideos = selection?.videos.some(Boolean) ?? false;
  const hasPhotos = selection?.videos.includes(false) ?? false;
  const saved = outcomes.filter((item) => item.report).length;

  function applySelection(next: Selection) {
    setSelection(next);
    setIndex(0);
    setLoaded(null);
    setOutcomes([]);
    setStripH264Sei(false);
    setError("");
  }

  useLayoutEffect(() => {
    const systemTheme = window.matchMedia("(prefers-color-scheme: dark)");
    const applyTheme = () => {
      document.documentElement.dataset.theme = theme === "system"
        ? (systemTheme.matches ? "dark" : "light")
        : theme;
    };
    applyTheme();
    if (theme !== "system") return;
    systemTheme.addEventListener("change", applyTheme);
    return () => systemTheme.removeEventListener("change", applyTheme);
  }, [theme]);

  useEffect(() => {
    try {
      localStorage.setItem("metasafe.theme", theme);
    } catch {
      // The theme switch still applies for the current window.
    }
  }, [theme]);

  useEffect(() => {
    document.documentElement.lang = language;
    try {
      localStorage.setItem("metasafe.language", language);
    } catch {
      // The language switch still applies for the current window.
    }
  }, [language]);

  useEffect(() => {
    if (!desktop) return;
    let disposed = false;
    const subscription = listen<Selection>("metasafe-files", ({ payload }) => {
      if (!disposed) applySelection(payload);
    });
    void subscription.catch((reason) => {
      if (!disposed) setError(String(reason));
    });
    return () => {
      disposed = true;
      void subscription.then((unlisten) => unlisten(), () => {});
    };
  }, [desktop]);

  useEffect(() => {
    if (revision === undefined || !activeName) return;
    let cancelled = false;
    void invoke<Preview>("preview_file", { revision, index }).then(
      (data) => { if (!cancelled) setLoaded({ revision, index, data }); },
      (reason) => { if (!cancelled) setLoaded({ revision, index, error: String(reason) }); },
    );
    return () => { cancelled = true; };
  }, [revision, index, activeName]);

  async function openFiles() {
    setAction("open");
    setError("");
    try {
      const chosen = await invoke<Selection | null>("choose_files");
      if (chosen?.names.length) applySelection(chosen);
    } catch (reason) {
      setError(String(reason));
    } finally {
      setAction(null);
    }
  }

  async function cleanFiles() {
    if (revision === undefined) return;
    setAction("clean");
    setProgress(0);
    setError("");
    const channel = new Channel<number>();
    channel.onmessage = setProgress;
    try {
      const results = await invoke<Outcome[] | null>("clean_files", {
        revision, progress: channel, noReencode, stripH264Sei: hasVideos && stripH264Sei,
      });
      if (results) setOutcomes(results);
    } catch (reason) {
      setError(String(reason));
    } finally {
      setAction(null);
    }
  }

  const ThemeIcon = theme === "light" ? Sun : theme === "dark" ? Moon : Monitor;
  const entrance = reduceMotion ? false : { opacity: 0, y: 5 };
  const transition = { duration: reduceMotion ? 0 : 0.18 };

  return (
    <div className="app">
      <header className="app-bar">
        <div className="brand"><span className="brand-mark"><ShieldCheck size={19} /></span>MetaSafe</div>
        <div className="bar-actions">
          <label className="theme-picker">
            <ThemeIcon size={14} aria-hidden="true" />
            <select aria-label={t.theme} value={theme} onChange={(event) => setTheme(event.target.value as Theme)}>
              <option value="system">{t.system}</option>
              <option value="light">{t.light}</option>
              <option value="dark">{t.dark}</option>
            </select>
          </label>
          <div className="languages" aria-label="Language">
            <button lang="en" aria-pressed={language === "en"} onClick={() => setLanguage("en")}>EN</button>
            <button lang="ru" aria-pressed={language === "ru"} onClick={() => setLanguage("ru")}>RU</button>
          </div>
        </div>
      </header>

      <motion.main initial={entrance} animate={{ opacity: 1, y: 0 }} transition={transition}>
        <h1>{t.workspace}</h1>
        {error ? <p role="alert" className="error-banner"><AlertCircle size={15} />{error}</p> : null}

        <div className="workspace">
          <section className="panel file-panel" aria-labelledby="files-title">
            <div className="panel-header">
              <h2 id="files-title">{t.files}{count > 0 ? <span>{count}</span> : null}</h2>
              {count > 0 ? (
                <button className="button file-open" aria-label={t.open} title={t.open}
                  disabled={!desktop || action !== null} onClick={openFiles}>
                  <Plus size={15} aria-hidden="true" />
                </button>
              ) : <span>{count}</span>}
            </div>
            {selection ? (
              <ul className="file-list">
                {selection.names.map((name, position) => (
                  <li key={position}>
                    <button className="file-item" aria-current={index === position ? "true" : undefined}
                      onClick={() => setIndex(position)} title={name}>
                      {selection.videos[position] ? <Video size={15} /> : <FileImage size={15} />}<span>{name}</span>
                    </button>
                  </li>
                ))}
              </ul>
            ) : <p className="panel-note">{t.noFiles}</p>}
          </section>

          <section className="panel preview-panel" aria-labelledby="preview-title">
            <div className="panel-header">
              <h2 id="preview-title">{t.preview}</h2>
              {activeName ? <span className="filename" title={activeName}>{activeName}</span> : null}
            </div>
            <div className="stage">
              {activeVideo && current?.data ? (
                <VideoPreview key={`${revision}:${index}`} token={current.data.video} port={current.data.video_port}
                  name={activeName ?? t.preview} t={t} />
              ) : current?.data?.image ? (
                <motion.img key={`${revision}:${index}`} src={current.data.image} alt={activeName}
                  initial={reduceMotion ? false : { opacity: 0 }} animate={{ opacity: 1 }} transition={transition} />
              ) : activeName ? (
                <div className="stage-message" role={current?.error ? "alert" : "status"}>
                  {current?.error ? <AlertCircle size={22} /> : <Image size={22} />}
                  <strong>{current?.error ? t.noPreview : t.loading}</strong>
                  {current?.error ? <p>{current.error}</p> : null}
                </div>
              ) : (
                <div className="stage-message">
                  <span className="drop-icon"><FileImage size={27} strokeWidth={1.4} /></span>
                  <strong>{t.emptyTitle}</strong>
                  <p>{desktop ? t.emptyText : t.browser}</p>
                  <button className="button primary" disabled={!desktop || action !== null} onClick={openFiles}>
                    <Upload size={15} />{t.open}
                  </button>
                </div>
              )}
            </div>
          </section>

          <section className="panel metadata-panel" aria-labelledby="metadata-title">
            <div className="panel-header"><h2 id="metadata-title">{t.metadata}</h2>{current?.data ? <span>{current.data.report.format}</span> : null}</div>
            {current?.data ? (
              <>
                <Checks key={`${revision}:${index}`} report={current.data.report} t={t} />
                {current.data.report.single_frame_export ? <p className="panel-note">{t.singleFrame}</p> : null}
                {current.data.report.orientation_unverified ? <p className="panel-note warning">{t.orientation}</p> : null}
                {activeVideo ? <p className="panel-note">{t.videoScope}</p> : null}

              </>
            ) : <p className="panel-note">{activeName && !current ? t.loading : t.emptyMeta}</p>}
          </section>
        </div>

        <div className="action-bar">
          <span className="local"><LockKeyhole size={14} />{t.local}</span>
          <div className="clean-action">
            {hasPhotos ? <label className="encoding-option" title={t.keepEncodingHint}>
              <input type="checkbox" checked={noReencode} disabled={!count || action !== null}
                onChange={(event) => setNoReencode(event.target.checked)} />
              {t.keepEncoding}
            </label> : null}
            {hasVideos ? <div className="video-option">
              <label className="encoding-option">
                <input type="checkbox" checked={stripH264Sei} disabled={action !== null}
                  onChange={(event) => setStripH264Sei(event.target.checked)} aria-describedby="strip-sei-hint" />
                {t.stripSei}
              </label>
              <small id="strip-sei-hint">{t.stripSeiHint}</small>
            </div> : null}
            {action === "clean" ? <span role="status">{t.working} {progress}/{count}</span> : <span>{t.original}</span>}
            <button className="button primary" disabled={!count || action !== null} onClick={cleanFiles}>
              <Sparkles size={15} />{count > 1 ? t.batch : t.clean}
            </button>
          </div>
        </div>

        {outcomes.length ? (
          <motion.section className="panel outcomes" aria-labelledby="results-title"
            initial={entrance} animate={{ opacity: 1, y: 0 }} transition={transition}>
            <div className="panel-header">
              <h2 id="results-title">{t.results} <span>{saved}/{outcomes.length}</span></h2>
              {saved ? <button className="button" onClick={() => { void invoke("show_output").catch((reason) => setError(String(reason))); }}>
                <FolderOpen size={15} />{t.folder}
              </button> : null}
            </div>
            {outcomes.map((item, position) => (
              <details className="outcome" key={position}>
                <summary>
                  <span className={`status ${item.report ? "clear" : "unknown"}`}>
                    {item.report ? <Check size={15} /> : <AlertCircle size={15} />}
                    {item.report ? t.saved : t.failed}
                  </span>
                  <span className="filename" title={item.source}>{item.report ? item.destination : item.source}</span>
                  <ChevronDown size={14} />
                </summary>
                {item.report ? <Checks report={item.report} t={t} /> : <p className="panel-note warning">{item.error}</p>}
              </details>
            ))}
          </motion.section>
        ) : null}
      </motion.main>
    </div>
  );
}
