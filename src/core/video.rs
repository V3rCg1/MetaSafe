use std::{
    fs::File,
    io::{self, BufReader, Read, Seek, Write},
    path::{Path, PathBuf},
    process::{Command, Stdio},
    sync::mpsc,
    thread,
    time::Duration,
};

use serde_json::Value;
use tempfile::{NamedTempFile, TempDir};
use wait_timeout::ChildExt;

use super::{
    Error,
    batch::valid_output_name,
    metadata::{Check, MetadataKind as Kind, Report, Status},
    open_regular_file,
};

pub const EXTENSIONS: &[&str] = &["mp4", "mov", "mkv", "webm"];

const MAX_VIDEO_BYTES: u64 = 2 * 1024 * 1024 * 1024;
const MAX_TOOL_OUTPUT: u64 = 2 * 1024 * 1024;
const MAX_STREAMS: usize = 32;
const PROBE_TIMEOUT: Duration = Duration::from_secs(30);
const REMUX_TIMEOUT: Duration = Duration::from_secs(180);

#[derive(Clone, Copy, PartialEq, Eq)]
enum Container {
    Mp4,
    Mov,
    Mkv,
    Webm,
}

impl Container {
    fn from_path(path: &Path) -> Option<Self> {
        match path.extension()?.to_str()?.to_ascii_lowercase().as_str() {
            "mp4" => Some(Self::Mp4),
            "mov" => Some(Self::Mov),
            "mkv" => Some(Self::Mkv),
            "webm" => Some(Self::Webm),
            _ => None,
        }
    }

    fn extension(self) -> &'static str {
        match self {
            Self::Mp4 => "mp4",
            Self::Mov => "mov",
            Self::Mkv => "mkv",
            Self::Webm => "webm",
        }
    }

    fn is_quicktime(self) -> bool {
        matches!(self, Self::Mp4 | Self::Mov)
    }

    fn demuxer(self) -> &'static str {
        if self.is_quicktime() {
            "mov"
        } else {
            "matroska"
        }
    }

    fn muxer(self) -> &'static str {
        if self == Self::Mkv {
            "matroska"
        } else {
            self.extension()
        }
    }

    fn input_options(self, command: &mut Command) {
        command.args(["-protocol_whitelist", "file", "-f", self.demuxer()]);
        if self.is_quicktime() {
            command.args([
                "-enable_drefs",
                "0",
                "-use_absolute_path",
                "0",
                "-export_all",
                "1",
                "-export_xmp",
                "1",
            ]);
        }
    }
}

pub fn is_video(path: &Path) -> bool {
    Container::from_path(path).is_some()
}

pub fn inspect(path: &Path) -> Result<Report, Error> {
    let container = Container::from_path(path).ok_or(Error::UnsupportedFormat)?;
    let stage = stage_source(path, container)?;
    let probe = probe(stage.path(), &input_name(container), container)?;
    Ok(report(&probe, container))
}

pub fn verify(path: &Path) -> Result<Report, Error> {
    inspect(path)
}

pub fn clean(source: &Path, destination: &Path) -> Result<Report, Error> {
    clean_with_options(source, destination, false)
}

/// Stream-copy supported media and remove container metadata. The optional
/// H.264 filter drops all SEI NAL units, rather than attempting to classify
/// proprietary payloads. The report covers the container, not the bitstream.
pub fn clean_with_options(
    source: &Path,
    destination: &Path,
    strip_h264_sei: bool,
) -> Result<Report, Error> {
    let container = Container::from_path(source).ok_or(Error::UnsupportedFormat)?;
    if Container::from_path(destination) != Some(container) {
        return Err(video_error("choose the same video extension for the copy"));
    }
    if !destination.file_name().is_some_and(valid_output_name) {
        return Err(Error::InvalidDestinationName(destination.to_owned()));
    }

    let stage = stage_source(source, container)?;
    let input = input_name(container);
    let before = probe(stage.path(), &input, container)?;
    let tracks = media_tracks(&before, container, strip_h264_sei)?;
    let output = format!("cleaned.{}", container.extension());
    remux(
        stage.path(),
        &input,
        &output,
        container,
        &tracks,
        strip_h264_sei,
    )?;
    let staged_path = stage.path().join(&output);
    let (mut staged_file, staged_meta) = open_regular_file(&staged_path)?;
    if staged_meta.len() >= MAX_VIDEO_BYTES {
        return Err(video_error("the temporary video reached the output limit"));
    }
    let after = probe(stage.path(), &output, container)?;
    verify_tracks(&before, &tracks, &after)?;
    let report = report(&after, container);
    if !report.passed() {
        return Err(Error::VerificationFailed);
    }
    publish(&mut staged_file, staged_meta.len(), destination)?;
    Ok(report)
}

fn input_name(container: Container) -> String {
    format!("source.{}", container.extension())
}

fn stage_source(path: &Path, container: Container) -> Result<TempDir, Error> {
    let (file, metadata) = open_regular_file(path)?;
    if metadata.len() >= MAX_VIDEO_BYTES {
        return Err(video_error("videos must be smaller than 2 GiB"));
    }
    let mut input = BufReader::new(file.take(MAX_VIDEO_BYTES));
    let mut header = [0_u8; 12];
    input.read_exact(&mut header).map_err(video_io)?;
    let valid = if container.is_quicktime() {
        matches!(
            &header[4..8],
            b"ftyp" | b"moov" | b"mdat" | b"wide" | b"free" | b"skip"
        )
    } else {
        header.starts_with(&[0x1a, 0x45, 0xdf, 0xa3])
    };
    if !valid {
        return Err(video_error(
            "the file does not have the expected video container signature",
        ));
    }
    let mut builder = tempfile::Builder::new();
    builder.prefix("metasafe-video-");
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        builder.permissions(std::fs::Permissions::from_mode(0o700));
    }
    let stage = builder.tempdir().map_err(video_io)?;
    let mut snapshot = File::create(stage.path().join(input_name(container))).map_err(video_io)?;
    snapshot.write_all(&header).map_err(video_io)?;
    let copied = io::copy(&mut input, &mut snapshot).map_err(video_io)?;
    if copied + header.len() as u64 != metadata.len() {
        return Err(video_error("the input size changed during processing"));
    }
    Ok(stage)
}

fn probe(directory: &Path, input: &str, container: Container) -> Result<Value, Error> {
    let mut command = tool("ffprobe");
    command.current_dir(directory).args(["-v", "error"]);
    container.input_options(&mut command);
    command
        .args([
            "-show_error",
            "-show_format",
            "-show_streams",
            "-show_chapters",
            "-of",
            "json",
        ])
        .arg(input);
    let result = run(command, "ffprobe", PROBE_TIMEOUT)?;
    let probe: Value =
        serde_json::from_slice(&result).map_err(|_| video_error("invalid ffprobe report"))?;
    let streams = streams(&probe)?;
    if !streams
        .iter()
        .any(|stream| kind(stream) == "video" && !is_cover(stream))
    {
        return Err(video_error("no video track was found"));
    }
    Ok(probe)
}

fn streams(probe: &Value) -> Result<&[Value], Error> {
    let streams = probe["streams"]
        .as_array()
        .ok_or_else(|| video_error("ffprobe returned no stream inventory"))?;
    if streams.len() > MAX_STREAMS {
        return Err(video_error("the file contains more than 32 tracks"));
    }
    Ok(streams)
}

fn kind(stream: &Value) -> &str {
    stream["codec_type"].as_str().unwrap_or_default()
}

fn is_cover(stream: &Value) -> bool {
    ["attached_pic", "timed_thumbnails", "still_image"]
        .iter()
        .any(|key| stream["disposition"][key].as_u64() == Some(1))
}

fn media_tracks(
    probe: &Value,
    container: Container,
    strip_h264_sei: bool,
) -> Result<Vec<&Value>, Error> {
    let mut tracks = Vec::new();
    for stream in streams(probe)? {
        let codec = stream["codec_name"].as_str().unwrap_or_default();
        match kind(stream) {
            "data" => continue,
            "video" if is_cover(stream) => continue,
            "attachment" => {
                return Err(video_error(
                    "files with attached fonts or other attachments require separate handling",
                ));
            }
            "video" | "audio" | "subtitle" if !codec.is_empty() => {}
            _ => return Err(video_error("the file has an unsupported track")),
        }
        if container == Container::Webm
            && !matches!(
                (kind(stream), codec),
                ("video", "vp8" | "vp9" | "av1")
                    | ("audio", "vorbis" | "opus")
                    | ("subtitle", "webvtt")
            )
        {
            return Err(video_error("a track codec is incompatible with WebM"));
        }
        if strip_h264_sei
            && kind(stream) == "video"
            && (codec != "h264" || has_hdr_or_captions(stream))
        {
            return Err(video_error(
                "SEI removal requires H.264 without detected HDR or embedded captions",
            ));
        }
        tracks.push(stream);
    }
    Ok(tracks)
}

fn has_hdr_or_captions(stream: &Value) -> bool {
    matches!(
        stream["color_transfer"].as_str(),
        Some("smpte2084" | "arib-std-b67")
    ) || flag_set(&stream["closed_captions"])
        || stream["side_data_list"].as_array().is_some_and(|items| {
            items.iter().any(|item| {
                let name = item["side_data_type"]
                    .as_str()
                    .unwrap_or_default()
                    .to_ascii_lowercase();
                [
                    "hdr",
                    "mastering display",
                    "content light",
                    "dolby",
                    "dovi",
                    "caption",
                ]
                .iter()
                .any(|term| name.contains(term))
            })
        })
}

const TRACK_DISPOSITIONS: &[&str] = &[
    "default",
    "dub",
    "original",
    "comment",
    "lyrics",
    "karaoke",
    "forced",
    "hearing_impaired",
    "visual_impaired",
    "clean_effects",
    "captions",
    "descriptions",
    "dependent",
    "metadata",
    "non_diegetic",
    "multilayer",
];

fn flag_set(value: &Value) -> bool {
    value.as_bool() == Some(true) || value.as_u64().is_some_and(|value| value != 0)
}

fn track_dispositions(stream: &Value) -> Vec<&'static str> {
    TRACK_DISPOSITIONS
        .iter()
        .copied()
        .filter(|flag| flag_set(&stream["disposition"][*flag]))
        .collect()
}

fn remux(
    directory: &Path,
    input: &str,
    output: &str,
    container: Container,
    tracks: &[&Value],
    strip_h264_sei: bool,
) -> Result<(), Error> {
    let mut command = tool("ffmpeg");
    command
        .current_dir(directory)
        .args(["-v", "error", "-xerror", "-nostdin", "-n"]);
    container.input_options(&mut command);
    command.args(["-i", input]);
    for (position, stream) in tracks.iter().enumerate() {
        let index = stream["index"]
            .as_u64()
            .ok_or_else(|| video_error("ffprobe did not identify a track"))?;
        command.arg("-map").arg(format!("0:{index}"));
        let dispositions = track_dispositions(stream);
        let disposition = if dispositions.is_empty() {
            String::from("0")
        } else {
            dispositions.join("+")
        };
        command
            .arg(format!("-disposition:{position}"))
            .arg(disposition);
        if strip_h264_sei && kind(stream) == "video" {
            command
                .arg(format!("-bsf:{position}"))
                .arg("filter_units=remove_types=6");
        }
    }
    command.args([
        "-map_metadata",
        "-1",
        "-map_metadata:s",
        "-1",
        "-map_chapters",
        "-1",
        "-c",
        "copy",
        "-fflags",
        "+bitexact",
    ]);
    if container.is_quicktime() {
        command.args(["-write_tmcd", "0"]);
    }
    command
        .arg("-fs")
        .arg(MAX_VIDEO_BYTES.to_string())
        .args(["-f", container.muxer(), output]);
    run(command, "ffmpeg", REMUX_TIMEOUT)?;
    Ok(())
}

fn verify_tracks(before: &Value, tracks: &[&Value], after: &Value) -> Result<(), Error> {
    let output_tracks = streams(after)?;
    if tracks.len() != output_tracks.len() {
        return Err(Error::VerificationFailed);
    }
    let keys = [
        "codec_type",
        "codec_name",
        "width",
        "height",
        "pix_fmt",
        "sample_aspect_ratio",
        "color_range",
        "color_space",
        "color_transfer",
        "color_primaries",
        "chroma_location",
        "sample_rate",
        "channels",
        "channel_layout",
        "closed_captions",
        "side_data_list",
    ];
    for (source, output) in tracks.iter().zip(output_tracks) {
        if keys.iter().any(|key| source[*key] != output[*key])
            || track_dispositions(source) != track_dispositions(output)
        {
            return Err(video_error(
                "track, color or orientation properties changed during cleanup",
            ));
        }
    }
    let duration = |probe: &Value| -> Option<f64> {
        let seconds: f64 = probe["format"]["duration"].as_str()?.parse().ok()?;
        (seconds.is_finite() && seconds > 0.0).then_some(seconds)
    };
    match (duration(before), duration(after)) {
        (Some(before), Some(after)) if (before - after).abs() <= 0.25 => Ok(()),
        _ => Err(video_error("could not verify the original video duration")),
    }
}

fn report(probe: &Value, container: Container) -> Report {
    let mut report = Report::new();
    report.format = container.extension().to_ascii_uppercase();
    report.checks = [Kind::Gps, Kind::Xmp, Kind::Comment, Kind::Unknown]
        .into_iter()
        .map(|kind| Check {
            kind,
            status: Status::NotDetected,
        })
        .collect();
    add_tags(
        &mut report,
        &probe["format"]["tags"],
        "Container",
        container,
        None,
    );
    if let Some(streams) = probe["streams"].as_array() {
        for stream in streams {
            let label = format!("Track {}", stream["index"].as_u64().unwrap_or_default());
            if is_cover(stream) || !matches!(kind(stream), "video" | "audio" | "subtitle") {
                report.record(Kind::Unknown, Status::Present);
                report.add_detail(
                    Kind::Unknown,
                    &label,
                    if is_cover(stream) {
                        "cover or thumbnail"
                    } else {
                        kind(stream)
                    },
                );
            }
            add_tags(
                &mut report,
                &stream["tags"],
                &label,
                container,
                Some(kind(stream)),
            );
        }
    }
    if let Some(chapters) = probe["chapters"]
        .as_array()
        .filter(|chapters| !chapters.is_empty())
    {
        report.record(Kind::Unknown, Status::Present);
        report.add_detail(Kind::Unknown, "Chapters", &chapters.len().to_string());
        for (index, chapter) in chapters.iter().enumerate() {
            add_tags(
                &mut report,
                &chapter["tags"],
                &format!("Chapter {}", index + 1),
                container,
                None,
            );
        }
    }
    report
}

fn add_tags(
    report: &mut Report,
    tags: &Value,
    label: &str,
    container: Container,
    track: Option<&str>,
) {
    let Some(tags) = tags.as_object() else { return };
    for (name, value) in tags {
        let key = name.to_ascii_lowercase();
        let value = value.as_str().unwrap_or("<unrecognized value>");
        if generated_tag(container, track, &key, value) {
            continue;
        }
        let kind = if key.contains("location") || key.contains("gps") {
            Kind::Gps
        } else if key.contains("xmp") {
            Kind::Xmp
        } else if matches!(key.as_str(), "title" | "description" | "comment") {
            Kind::Comment
        } else {
            Kind::Unknown
        };
        report.record(kind, Status::Present);
        let bounded_name: String = name.chars().take(128).collect();
        report.add_detail(kind, &format!("{label} / {bounded_name}"), value);
    }
}

fn generated_tag(container: Container, track: Option<&str>, key: &str, value: &str) -> bool {
    if !container.is_quicktime() {
        return match (track, key) {
            (None, "encoder") => value == "Lavf",
            (Some(_), "duration") => {
                value.len() == 18
                    && value.bytes().enumerate().all(|(index, byte)| match index {
                        2 | 5 => byte == b':',
                        8 => byte == b'.',
                        _ => byte.is_ascii_digit(),
                    })
            }
            _ => false,
        };
    }
    match (track, key) {
        (None, "major_brand") => is_muxer_brand(value.as_bytes()),
        (None, "compatible_brands") => {
            let (brands, remainder) = value.as_bytes().as_chunks::<4>();
            !brands.is_empty()
                && remainder.is_empty()
                && brands.iter().all(|brand| is_muxer_brand(brand))
        }
        (None, "minor_version") => value == "512",
        (Some(_), "language") => value == "und",
        (Some(_), "vendor_id") => matches!(value, "FFMP" | "[0][0][0][0]"),
        (Some("video"), "handler_name") => value == "VideoHandler",
        (Some("audio"), "handler_name") => value == "SoundHandler",
        (Some("subtitle"), "handler_name") => matches!(value, "SubtitleHandler" | "TextHandler"),
        _ => false,
    }
}

fn is_muxer_brand(value: &[u8]) -> bool {
    matches!(
        value,
        b"isom" | b"iso2" | b"iso4" | b"iso5" | b"iso6" | b"avc1" | b"mp41" | b"mp42" | b"qt  "
    )
}

fn publish(staged_file: &mut File, length: u64, destination: &Path) -> Result<(), Error> {
    let directory = destination
        .parent()
        .filter(|parent| !parent.as_os_str().is_empty())
        .unwrap_or_else(|| Path::new("."));
    let write_error = |source: io::Error| match source.kind() {
        io::ErrorKind::AlreadyExists => Error::OutputExists(destination.to_owned()),
        _ => Error::Write {
            path: destination.to_owned(),
            source,
        },
    };
    let mut temporary = NamedTempFile::new_in(directory).map_err(write_error)?;
    if io::copy(&mut staged_file.take(length + 1), &mut temporary).map_err(write_error)? != length {
        return Err(Error::VerificationFailed);
    }
    temporary.as_file().sync_all().map_err(write_error)?;
    temporary
        .persist_noclobber(destination)
        .map_err(|error| write_error(error.error))?;
    let (mut published, metadata) = open_regular_file(destination)?;
    staged_file.rewind().map_err(video_io)?;
    let mismatch = || Error::PublishedOutputChanged(destination.to_owned());
    if metadata.len() != length {
        return Err(mismatch());
    }
    let (mut expected, mut actual) = ([0_u8; 8192], [0_u8; 8192]);
    let mut remaining = length;
    while remaining > 0 {
        let size = remaining.min(expected.len() as u64) as usize;
        staged_file
            .read_exact(&mut expected[..size])
            .map_err(video_io)?;
        published
            .read_exact(&mut actual[..size])
            .map_err(|_| mismatch())?;
        if expected[..size] != actual[..size] {
            return Err(mismatch());
        }
        remaining -= size as u64;
    }
    if published.read(&mut actual[..1]).map_err(|_| mismatch())? != 0 {
        return Err(mismatch());
    }
    Ok(())
}

fn tool(name: &str) -> Command {
    if let Some(path) = std::env::var_os(format!("METASAFE_{}", name.to_ascii_uppercase()))
        .map(PathBuf::from)
        .filter(|path| path.is_absolute())
    {
        return Command::new(path);
    }
    #[cfg(target_os = "macos")]
    for directory in ["/opt/homebrew/bin", "/usr/local/bin"] {
        let path = Path::new(directory).join(name);
        if path.is_file() {
            return Command::new(path);
        }
    }
    Command::new(name)
}

fn run(mut command: Command, name: &'static str, timeout: Duration) -> Result<Vec<u8>, Error> {
    let mut child = command
        .stdin(Stdio::null())
        .stdout(Stdio::piped())
        .stderr(Stdio::null())
        .spawn()
        .map_err(|error| {
            if error.kind() == io::ErrorKind::NotFound {
                Error::MissingTool(name)
            } else {
                video_io(error)
            }
        })?;
    let stdout = child.stdout.take().expect("piped stdout");
    let (send, receive) = mpsc::sync_channel(1);
    thread::spawn(move || {
        let mut bytes = Vec::new();
        let result = stdout
            .take(MAX_TOOL_OUTPUT + 1)
            .read_to_end(&mut bytes)
            .map(|_| bytes);
        let _ = send.send(result);
    });
    let status = match child.wait_timeout(timeout) {
        Ok(Some(status)) => status,
        result => {
            let _ = child.kill();
            let _ = child.wait();
            return Err(match result {
                Err(error) => video_io(error),
                _ => video_error(format!("{name} timed out")),
            });
        }
    };
    let output = receive
        .recv_timeout(Duration::from_secs(2))
        .map_err(|_| video_error(format!("{name} output did not close")))?
        .map_err(video_io)?;
    if output.len() as u64 > MAX_TOOL_OUTPUT {
        return Err(video_error(format!("{name} output exceeded its limit")));
    }
    if !status.success() {
        return Err(video_error(format!(
            "{name} could not process the video ({status})"
        )));
    }
    Ok(output)
}

fn video_io(error: io::Error) -> Error {
    video_error(error.to_string())
}

fn video_error(message: impl Into<String>) -> Error {
    Error::Video(message.into())
}
