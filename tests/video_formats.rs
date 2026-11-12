use std::{fs, path::Path, process::Command};

use metasafe::core::{self, CleanMode};
use serde_json::Value;

fn run(program: &str, args: &[&str], path: &Path) -> Vec<u8> {
    let output = Command::new(program).args(args).arg(path).output().unwrap();
    assert!(
        output.status.success(),
        "{program}: {}",
        String::from_utf8_lossy(&output.stderr)
    );
    output.stdout
}

fn streams(path: &Path) -> Value {
    serde_json::from_slice(&run(
        "ffprobe",
        &[
            "-v",
            "error",
            "-show_streams",
            "-show_format",
            "-of",
            "json",
        ],
        path,
    ))
    .unwrap()
}

fn packet_hash(path: &Path, track: &str) -> String {
    let output = run(
        "ffprobe",
        &[
            "-v",
            "error",
            "-select_streams",
            track,
            "-show_packets",
            "-show_entries",
            "packet=data_hash",
            "-show_data_hash",
            "sha256",
            "-of",
            "csv=p=0",
        ],
        path,
    );
    assert!(!output.is_empty());
    String::from_utf8(output).unwrap()
}

fn fixture(source: &Path, subtitles: &Path, video: &str, audio: &str, subtitle: &str) {
    run(
        "ffmpeg",
        &[
            "-nostdin",
            "-v",
            "error",
            "-f",
            "lavfi",
            "-i",
            "color=c=blue:s=64x64:r=10:d=1",
            "-f",
            "lavfi",
            "-i",
            "sine=f=440:d=1",
            "-i",
            subtitles.to_str().unwrap(),
            "-map",
            "0:v",
            "-map",
            "1:a",
            "-map",
            "2:s",
            "-c:v",
            video,
            "-threads",
            "1",
            "-c:a",
            audio,
            "-c:s",
            subtitle,
            "-metadata",
            "title=MetaSafe synthetic private title",
            "-metadata:s:s:0",
            "language=eng",
        ],
        source,
    );
}

#[test]
#[ignore = "requires ffmpeg and ffprobe with libx264, libvpx-vp9 and libopus"]
fn batch_copies_mkv_and_webm_packets_and_retains_subtitles() {
    let directory = tempfile::tempdir().unwrap();
    let subtitles = directory.path().join("captions.srt");
    fs::write(
        &subtitles,
        "1\n00:00:00,000 --> 00:00:00,800\nSynthetic caption\n",
    )
    .unwrap();
    let mkv = directory.path().join("source.mkv");
    let webm = directory.path().join("source.webm");
    fixture(&mkv, &subtitles, "libx264", "aac", "srt");
    fixture(&webm, &subtitles, "libvpx-vp9", "libopus", "webvtt");
    let paths = [mkv, webm];
    let originals = paths.each_ref().map(|path| fs::read(path).unwrap());
    let report = core::batch_clean(&paths, directory.path(), |_| {}).unwrap();
    assert_eq!(report.items.len(), 2);
    for ((source, original), item) in paths.iter().zip(originals).zip(report.items) {
        let output = directory.path().join(format!(
            "source_clean.{}",
            source.extension().unwrap().to_str().unwrap()
        ));
        assert!(item.result.unwrap().passed());
        assert!(core::verify(&output).unwrap().passed());
        assert_eq!(fs::read(source).unwrap(), original);
        let probe = streams(&output);
        let tracks = probe["streams"].as_array().unwrap();
        assert_eq!(
            tracks
                .iter()
                .map(|track| track["codec_type"].as_str().unwrap())
                .collect::<Vec<_>>(),
            ["video", "audio", "subtitle"]
        );
        assert!(
            !probe["format"]["tags"]
                .to_string()
                .contains("MetaSafe synthetic private title")
        );
        for track in ["v:0", "a:0", "s:0"] {
            assert_eq!(
                packet_hash(source, track),
                packet_hash(&output, track),
                "{source:?} {track}"
            );
        }
    }
}

#[test]
#[ignore = "requires ffmpeg and ffprobe with libx264 and filter_units"]
fn h264_sei_removal_is_opt_in_and_preserves_subtitles() {
    let directory = tempfile::tempdir().unwrap();
    let subtitles = directory.path().join("captions.srt");
    fs::write(
        &subtitles,
        "1\n00:00:00,000 --> 00:00:00,800\nSynthetic caption\n",
    )
    .unwrap();
    let source = directory.path().join("source.mkv");
    let output = directory.path().join("filtered.mkv");
    fixture(&source, &subtitles, "libx264", "aac", "srt");
    let result = core::clean_with_options(&source, &output, CleanMode::Reencode, true).unwrap();
    assert!(result.passed());
    assert_ne!(packet_hash(&source, "v:0"), packet_hash(&output, "v:0"));
    assert_eq!(packet_hash(&source, "s:0"), packet_hash(&output, "s:0"));
}

#[test]
#[ignore = "requires ffmpeg and ffprobe with libx265"]
fn mkv_preserves_hdr_signal_and_video_packets() {
    let directory = tempfile::tempdir().unwrap();
    let source = directory.path().join("hdr.mkv");
    let output = directory.path().join("hdr_clean.mkv");
    run(
        "ffmpeg",
        &[
            "-nostdin",
            "-v",
            "error",
            "-f",
            "lavfi",
            "-i",
            "color=c=blue:s=64x64:r=10:d=1",
            "-vf",
            "format=yuv420p10le,setparams=color_primaries=bt2020:color_trc=smpte2084:colorspace=bt2020nc",
            "-c:v",
            "libx265",
            "-x265-params",
            "pools=1:frame-threads=1:hdr10=1:master-display=G(13250,34500)B(7500,3000)R(34000,16000)WP(15635,16450)L(10000000,50):max-cll=1000,400",
            "-metadata",
            "title=Synthetic HDR",
        ],
        &source,
    );
    let before = frames(&source);
    assert_eq!(before["frames"][0]["color_transfer"], "smpte2084");
    assert!(
        before["frames"][0]["side_data_list"]
            .to_string()
            .contains("Mastering display metadata")
    );
    assert!(core::clean(&source, &output).unwrap().passed());
    let after = frames(&output);
    for key in [
        "color_space",
        "color_primaries",
        "color_transfer",
        "pix_fmt",
        "side_data_list",
    ] {
        assert_eq!(before["frames"][0][key], after["frames"][0][key], "{key}");
    }
    assert_eq!(packet_hash(&source, "v:0"), packet_hash(&output, "v:0"));
}

fn frames(path: &Path) -> Value {
    serde_json::from_slice(&run(
        "ffprobe",
        &[
            "-v",
            "error",
            "-select_streams",
            "v:0",
            "-show_frames",
            "-of",
            "json",
        ],
        path,
    ))
    .unwrap()
}

#[test]
#[ignore = "requires ffmpeg and ffprobe with libx264 and mov_text"]
fn quicktime_formats_keep_subtitles_and_never_overwrite_a_copy() {
    let directory = tempfile::tempdir().unwrap();
    let subtitles = directory.path().join("captions.srt");
    fs::write(
        &subtitles,
        "1\n00:00:00,000 --> 00:00:00,800\nSynthetic caption\n",
    )
    .unwrap();
    for extension in ["mp4", "mov"] {
        let source = directory.path().join(format!("original.{extension}"));
        let output = directory.path().join(format!("cleaned.{extension}"));
        fixture(&source, &subtitles, "libx264", "aac", "mov_text");
        assert!(core::clean(&source, &output).unwrap().passed());
        assert_eq!(packet_hash(&source, "s:0"), packet_hash(&output, "s:0"));
        let cleaned = fs::read(&output).unwrap();
        assert!(matches!(
            core::clean(&source, &output),
            Err(core::Error::OutputExists(_))
        ));
        assert_eq!(fs::read(&output).unwrap(), cleaned);
    }
}
