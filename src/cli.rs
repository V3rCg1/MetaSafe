use std::{
    fmt::{self, Display},
    io::{self, Write},
    path::{Path, PathBuf},
    process::ExitCode,
};

use clap::{Parser, Subcommand};
use metasafe::core::{self, metadata::Report};

#[derive(Parser)]
#[command(
    name = "metasafe",
    version,
    about = "Inspect, clean, and verify JPEG metadata locally"
)]
struct Cli {
    #[command(subcommand)]
    command: Command,
}

#[derive(Subcommand)]
enum Command {
    /// Show supported metadata checks and available EXIF fields.
    Inspect {
        /// Path to the JPEG.
        file: PathBuf,
    },
    /// Write and verify a cleaned copy at a new path.
    Clean {
        /// Path to the source JPEG.
        file: PathBuf,
        /// Path to the new JPEG. Existing files are never replaced.
        #[arg(long)]
        output: PathBuf,
    },
    /// Exit with 0 if all checks pass, 1 if findings remain, or 2 on error.
    Verify {
        /// Path to the JPEG to verify.
        file: PathBuf,
    },
    /// Clean several JPEG files into a folder. Exit 1 if any file fails.
    Batch {
        /// Input JPEG paths.
        #[arg(required = true)]
        files: Vec<PathBuf>,
        /// Existing output folder. Existing files are never replaced.
        #[arg(long)]
        output_dir: PathBuf,
    },
}

pub fn run() -> ExitCode {
    let command = Cli::parse().command;
    let result = match &command {
        Command::Inspect { file } => core::inspect(file),
        Command::Clean { file, output } => core::clean(file, output),
        Command::Verify { file } => core::verify(file),
        Command::Batch { files, output_dir } => return run_batch(files, output_dir),
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

fn run_batch(files: &[PathBuf], output_dir: &Path) -> ExitCode {
    let mut completed = 0;
    let report = match core::batch_clean(files, output_dir, |_| {
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
    writeln!(output, "Format: JPEG")?;
    writeln!(output, "Metadata checks:")?;
    for check in &report.checks {
        writeln!(output, "  {}: {}", check.kind, check.status)?;
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
        writeln!(
            output,
            "Photo content can still reveal personal information. This report does not guarantee anonymity."
        )?;
    }

    output.flush()
}

// JPEG metadata and file names may contain terminal escape sequences or line breaks.
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
