//! First-run setup: install the virtual audio device and give its endpoint our name.
//!
//! The work happens in `resources/setup.ps1`. Installing a driver needs admin rights, so that run
//! goes through a UAC prompt; naming the endpoint doesn't, so it runs quietly.

use std::os::windows::process::CommandExt;
use std::path::PathBuf;
use std::process::Command;

const CREATE_NO_WINDOW: u32 = 0x0800_0000;

pub trait Setup: Send + Sync {
    /// Installs the driver (UAC prompt) and names the endpoint. Returns what was done, line by line.
    fn install_device(&self) -> Result<Vec<String>, String>;
    /// Names the endpoint of an already installed driver. No prompt.
    fn name_device(&self) -> Result<Vec<String>, String>;
}

pub struct ScriptSetup {
    pub script: PathBuf,
    pub package: PathBuf,
}

/// Quotes a value as a PowerShell single-quoted string.
fn quoted(value: &str) -> String {
    format!("'{}'", value.replace('\'', "''"))
}

/// The command that starts the script elevated and waits for it. 1223 is ERROR_CANCELLED: the user
/// said no to the UAC prompt.
fn elevated_command(script: &str, package: &str, result: &str) -> String {
    let args = ["-NoProfile", "-ExecutionPolicy", "Bypass", "-File", &format!("\"{script}\""), "-Pkg", &format!("\"{package}\""), "-Result", &format!("\"{result}\"")];
    let list = args.iter().map(|a| quoted(a)).collect::<Vec<_>>().join(",");
    format!("try {{ $p = Start-Process powershell.exe -Verb RunAs -Wait -PassThru -WindowStyle Hidden -ArgumentList {list}; exit $p.ExitCode }} catch {{ exit 1223 }}")
}

impl ScriptSetup {
    fn run(&self, elevated: bool) -> Result<Vec<String>, String> {
        let result = std::env::temp_dir().join("freeseries-setup-result.txt");
        let _ = std::fs::remove_file(&result);
        let (script, package, result_arg) = (self.script.to_string_lossy(), self.package.to_string_lossy(), result.to_string_lossy());

        let mut powershell = Command::new("powershell.exe");
        powershell.creation_flags(CREATE_NO_WINDOW).args(["-NoProfile", "-ExecutionPolicy", "Bypass"]);
        if elevated {
            powershell.args(["-Command", &elevated_command(&script, &package, &result_arg)]);
        } else {
            powershell.args(["-File", &script, "-Pkg", &package, "-Result", &result_arg, "-RenameOnly"]);
        }
        let status = powershell.status().map_err(|e| format!("Couldn't start PowerShell: {e}"))?;
        if status.code() == Some(1223) {
            return Err("Setup needs administrator rights to install the audio device. Nothing was changed.".into());
        }

        // The script reports through the file: the elevated run is a different process tree.
        let report = std::fs::read_to_string(&result).map_err(|_| "Setup didn't finish, and left no report. Nothing may have been changed.".to_string())?;
        let mut lines = report.trim_start_matches('\u{feff}').lines().map(str::to_string);
        let verdict = lines.next().unwrap_or_default();
        let detail: Vec<String> = lines.collect();
        for line in &detail {
            log::info!("setup: {line}");
        }
        match verdict.strip_prefix("FAILED: ") {
            None if verdict == "OK" => Ok(detail),
            Some(reason) => Err(reason.to_string()),
            None => Err(format!("Setup reported something unexpected: {verdict}")),
        }
    }
}

impl Setup for ScriptSetup {
    fn install_device(&self) -> Result<Vec<String>, String> {
        self.run(true)
    }

    fn name_device(&self) -> Result<Vec<String>, String> {
        self.run(false)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn elevated_command_survives_spaces_and_apostrophes() {
        let cmd = elevated_command(r"C:\Users\Dara O'Neil\App Data\setup.ps1", r"C:\pkg dir", r"C:\Temp\r.txt");
        // Paths with spaces stay one argument (inner double quotes), apostrophes are doubled.
        assert!(cmd.contains(r#"'"C:\Users\Dara O''Neil\App Data\setup.ps1"'"#), "{cmd}");
        assert!(cmd.contains(r#"'-Pkg','"C:\pkg dir"'"#), "{cmd}");
        assert!(cmd.contains("-Verb RunAs -Wait"));
        assert!(cmd.ends_with("catch { exit 1223 }"));
    }
}
