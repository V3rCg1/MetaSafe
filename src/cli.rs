use std::{
    fmt::{self, Display},
    io::{self, Write},
    path::{Path, PathBuf},
    process::ExitCode,
};

use clap::{Parser, Subcommand};
use metasafe::core::{
    self, CleanMode,
    metadata::{MetadataKind, Report},
};

#[derive(Parser)]
#[command(
    name = "metasafe",
    version,
    about = "Inspect, clean, and verify photo and video metadata locally"
)]
struct Cli {
    #[command(subcommand)]
    command: Command,
}

#[derive(Subcommand)]
enum Command {
    /// Show supported metadata checks and available fields.
    Inspect {
        /// Path to a photo or video.
        file: PathBuf,
    },
    /// Write and verify a cleaned copy at a new path.
    Clean {
        /// Path to the source photo or video.
        file: PathBuf,
        /// Path to the cleaned photo or video copy. Existing files are never replaced.
        #[arg(long)]
        output: PathBuf,
        /// Keep the original encoding of a static JPEG, PNG or WebP.
        #[arg(long)]
        no_reencode: bool,
        /// Remove all H.264 SEI messages. Reject detected HDR and embedded captions.
        #[arg(long)]
        strip_h264_sei: bool,
    },
    /// Exit with 0 if all checks pass, 1 if findings remain, or 2 on error.
    Verify {
        /// Path to a photo or video to verify.
        file: PathBuf,
    },
    /// Clean several photos or videos into a folder. Exit 1 if any file fails.
    Batch {
        /// Input photo and video paths.
        #[arg(required = true)]
        files: Vec<PathBuf>,
        /// Existing output folder. Existing files are never replaced.
        #[arg(long)]
        output_dir: PathBuf,
        /// Keep each supported file's original encoding.
        #[arg(long)]
        no_reencode: bool,
        /// Remove all H.264 SEI messages. Reject detected HDR and embedded captions.
        #[arg(long)]
        strip_h264_sei: bool,
    },
}

pub fn run() -> ExitCode {
    let command = Cli::parse().command;
    let result = match &command {
        Command::Inspect { file } => core::inspect(file),
        Command::Clean {
            file,
            output,
            no_reencode,
            strip_h264_sei,
        } => core::clean_with_options(file, output, clean_mode(*no_reencode), *strip_h264_sei),
        Command::Verify { file } => core::verify(file),
        Command::Batch {
            files,
            output_dir,
            no_reencode,
            strip_h264_sei,
        } => return run_batch(files, output_dir, clean_mode(*no_reencode), *strip_h264_sei),
    };

    let report = match result {
        Ok(report) => report,
        Err(error) => {
            return command_error(&error);
        }
    };

    if let Err(error) = write_report(&mut io::stdout().lock(), &command, &report) {
        if error.kind() != io::ErrorKind::BrokenPipe {
            let _ = writeln!(io::stderr().lock(), "Error writing report: {error}");
        }
        return ExitCode::from(2);
    }

    if matches!(command, Command::Verify { .. }) && !report.passed() {
        return ExitCode::from(1);
    }

    ExitCode::SUCCESS
}

fn command_error(error: &core::Error) -> ExitCode {
    let _ = writeln!(
        io::stderr().lock(),
        "Error: {}",
        TerminalText(&error.to_string())
    );
    ExitCode::from(2)
}

fn clean_mode(no_reencode: bool) -> CleanMode {
    if no_reencode {
        CleanMode::KeepEncoding
    } else {
        CleanMode::Reencode
    }
}

fn run_batch(
    files: &[PathBuf],
    output_dir: &Path,
    mode: CleanMode,
    strip_h264_sei: bool,
) -> ExitCode {
    let mut completed = 0;
    let report =
        match core::batch_clean_with_options(files, output_dir, mode, strip_h264_sei, |_| {
            completed += 1;
            let _ = writeln!(io::stderr().lock(), "[{completed}/{}]", files.len());
        }) {
            Ok(report) => report,
            Err(error) => return command_error(&error),
        };
    let mut stdout = io::stdout().lock();
    let mut stderr = io::stderr().lock();
    let mut passed = 0;
    for item in &report.items {
        let source = item.source.to_string_lossy();
        match &item.result {
            Ok(_) => {
                passed += 1;
                if writeln!(
                    stdout,
                    "Created: {}",
                    TerminalText(&item.destination.to_string_lossy())
                )
                .is_err()
                {
                    return ExitCode::from(2);
                }
            }
            Err(error) => {
                let _ = writeln!(
                    stderr,
                    "Error for {}: {}",
                    TerminalText(&source),
                    TerminalText(&error.to_string())
                );
            }
        }
    }
    if writeln!(stdout, "Created {passed}/{} copies.", report.items.len())
        .and_then(|()| stdout.flush())
        .is_err()
    {
        return ExitCode::from(2);
    }
    ExitCode::from(u8::from(passed != report.items.len()))
}

fn write_report(output: &mut impl Write, command: &Command, report: &Report) -> io::Result<()> {
    let (title, file): (&str, &Path) = match command {
        Command::Inspect { file } => ("Inspection", file),
        Command::Clean { output, .. } => ("Cleaned copy created", output),
        Command::Verify { file } => ("Verification", file),
        Command::Batch { .. } => unreachable!("batch has its own report"),
    };
    writeln!(output, "MetaSafe — {title}")?;
    writeln!(output, "File: {}", TerminalText(&file.to_string_lossy()))?;
    writeln!(output, "Format: {}", report.format)?;
    let video = core::video::is_video(file);
    writeln!(output, "Metadata checks:")?;
    for check in &report.checks {
        let label = match (video, check.kind) {
            (true, MetadataKind::Comment) => "Title, description and comments".to_owned(),
            (true, MetadataKind::Unknown) => "Other container and track metadata".to_owned(),
            _ => check.kind.to_string(),
        };
        writeln!(output, "  {label}: {}", check.status)?;
    }

    if matches!(command, Command::Inspect { .. }) {
        if let Some(orientation) = report.orientation {
            writeln!(output, "EXIF orientation: {orientation}")?;
        }
        if !report.exif_fields.is_empty() {
            writeln!(output, "EXIF fields:")?;
            for field in &report.exif_fields {
                writeln!(
                    output,
                    "  {} (IFD {}): {}",
                    TerminalText(&field.name),
                    field.ifd,
                    TerminalText(&field.value)
                )?;
            }
        }
        if video {
            for field in &report.details {
                writeln!(
                    output,
                    "  {}: {}",
                    TerminalText(&field.name),
                    TerminalText(&field.value)
                )?;
            }
        }
        if report.exif_fields_truncated {
            writeln!(
                output,
                "Some EXIF fields or values were omitted or shortened."
            )?;
        }
    } else {
        let summary = if report.passed() {
            "All supported checks passed."
        } else {
            "Metadata was detected or at least one check was incomplete."
        };
        writeln!(output, "{summary}")?;
        if video {
            writeln!(
                output,
                "Verification covers supported container and stream tags. Encoded content, subtitles and in-stream data can still reveal personal information."
            )?;
        } else {
            writeln!(
                output,
                "Photo content can still reveal personal information. This report does not guarantee anonymity."
            )?;
        }
    }

    output.flush()
}

// image metadata and file names may contain terminal escape sequences or line breaks.
struct TerminalText<'a>(&'a str);

impl Display for TerminalText<'_> {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        for character in self.0.chars() {
            if character.is_control()
                || matches!(
                    character,
                    '\u{061c}' | '\u{200e}' | '\u{200f}' | '\u{2028}'..='\u{202e}' | '\u{2066}'..='\u{2069}'
                )
            {
                write!(formatter, "{}", character.escape_default())?;
            } else {
                write!(formatter, "{character}")?;
            }
        }
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn terminal_text_keeps_unicode_and_escapes_controls() {
        assert_eq!(
            TerminalText("Фото\n\u{1b}[2J\u{202e}").to_string(),
            r"Фото\n\u{1b}[2J\u{202e}"
        );
    }
}
