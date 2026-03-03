use std::fs::File;
use std::io::{self, BufRead, BufReader, Seek, SeekFrom};
use std::path::Path;
use std::thread;
use std::time::Duration;

use anyhow::{Context, Result, bail};

use super::registry;
use super::types::ComponentName;

/// Show (and optionally follow) logs for a component.
pub fn logs(component: &ComponentName, follow: bool, lines: usize, error: bool) -> Result<()> {
    if *component == ComponentName::All {
        bail!("cannot tail logs for 'all' — specify a single component");
    }

    let key = component.key();
    let reg = registry::load()?;

    let entry = reg
        .processes
        .get(key)
        .with_context(|| format!("{} is not installed", key))?;

    let log_path = if error {
        &entry.stderr_log
    } else {
        &entry.stdout_log
    };

    if !log_path.exists() {
        println!("No log file yet for {} at {}", key, log_path.display());
        return Ok(());
    }

    if follow {
        tail_follow(log_path, lines)
    } else {
        tail_lines(log_path, lines)
    }
}

/// Print the last `n` lines of a file.
fn tail_lines(path: &Path, n: usize) -> Result<()> {
    let file = File::open(path).with_context(|| format!("failed to open {}", path.display()))?;
    let reader = BufReader::new(file);
    let all_lines: Vec<String> = reader.lines().collect::<io::Result<Vec<_>>>()?;

    let start = all_lines.len().saturating_sub(n);
    for line in &all_lines[start..] {
        println!("{}", line);
    }

    Ok(())
}

/// Print the last `n` lines then follow new output.
fn tail_follow(path: &Path, n: usize) -> Result<()> {
    // First show the last N lines
    tail_lines(path, n)?;

    // Then seek to end and poll for new data
    let mut file =
        File::open(path).with_context(|| format!("failed to open {}", path.display()))?;
    file.seek(SeekFrom::End(0))?;
    let mut reader = BufReader::new(file);
    let mut buf = String::new();

    loop {
        buf.clear();
        match reader.read_line(&mut buf) {
            Ok(0) => {
                // No new data — poll
                thread::sleep(Duration::from_millis(200));
            }
            Ok(_) => {
                // Strip trailing newline for cleaner output
                print!("{}", buf);
            }
            Err(e) => {
                bail!("error reading log: {}", e);
            }
        }
    }
}
