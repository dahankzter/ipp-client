// SPDX-License-Identifier: GPL-3.0-only

//! Reads the per-user default printer from `lpoptions`.
//!
//! Format is line-oriented: `Default <dest>[/<instance>] [options...]`.
//! We read only the destination name and ignore the option list.

use std::path::{Path, PathBuf};

pub(crate) fn parse_default(contents: &str) -> Option<String> {
    contents
        .lines()
        .filter_map(|line| line.strip_prefix("Default "))
        .filter_map(|rest| rest.split_whitespace().next())
        .map(|dest| dest.split('/').next().unwrap_or(dest).to_string())
        .filter(|dest| !dest.is_empty())
        .next_back()
}

/// Reads the default printer, preferring the user file over the system file.
pub fn default_printer_from(user_path: &Path, system_path: &Path) -> Option<String> {
    for path in [user_path, system_path] {
        if let Ok(contents) = std::fs::read_to_string(path)
            && let Some(dest) = parse_default(&contents)
        {
            return Some(dest);
        }
    }
    None
}

fn user_lpoptions() -> PathBuf {
    std::env::var_os("HOME")
        .map(PathBuf::from)
        .unwrap_or_default()
        .join(".cups/lpoptions")
}

/// Reads the default printer from `~/.cups/lpoptions`, then `/etc/cups/lpoptions`.
pub fn default_printer() -> Option<String> {
    default_printer_from(&user_lpoptions(), Path::new("/etc/cups/lpoptions"))
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::io::Write;

    #[test]
    fn reads_the_default_destination() {
        assert_eq!(
            parse_default("Default HP-8210 media=a4 sides=one-sided\n").as_deref(),
            Some("HP-8210")
        );
    }

    #[test]
    fn strips_the_instance_suffix() {
        assert_eq!(
            parse_default("Default HP-8210/duplex media=a4\n").as_deref(),
            Some("HP-8210")
        );
    }

    #[test]
    fn ignores_dest_lines_and_blank_lines() {
        let contents = "\nDest Other-Printer media=a4\nDefault HP-8210\n";
        assert_eq!(parse_default(contents).as_deref(), Some("HP-8210"));
    }

    #[test]
    fn malformed_input_yields_none() {
        assert_eq!(parse_default(""), None);
        assert_eq!(parse_default("Default\n"), None);
        assert_eq!(parse_default("garbage garbage\n"), None);
    }

    #[test]
    fn user_file_wins_over_system_file() {
        let dir = std::env::temp_dir().join("cups-client-lpoptions-test");
        std::fs::create_dir_all(&dir).unwrap();
        let user = dir.join("user");
        let system = dir.join("system");
        write!(std::fs::File::create(&user).unwrap(), "Default UserChoice\n").unwrap();
        write!(std::fs::File::create(&system).unwrap(), "Default SystemChoice\n").unwrap();

        assert_eq!(default_printer_from(&user, &system).as_deref(), Some("UserChoice"));

        std::fs::remove_file(&user).unwrap();
        assert_eq!(default_printer_from(&user, &system).as_deref(), Some("SystemChoice"));

        std::fs::remove_dir_all(&dir).unwrap();
    }

    #[test]
    fn no_files_at_all_yields_none() {
        let missing = std::path::Path::new("/nonexistent/lpoptions");
        assert_eq!(default_printer_from(missing, missing), None);
    }
}
