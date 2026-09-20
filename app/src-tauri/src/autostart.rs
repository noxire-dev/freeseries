//! Start with Windows, as a logon-triggered scheduled task.
//!
//! Not the usual `Run` registry key: Explorer holds those back and launches them one after another
//! (measured: 38 s after the desktop appeared), which for an audio pipeline is 38 s of silence.
//! A per-user logon task needs no admin rights and fires as soon as the user logs on.

use std::os::windows::process::CommandExt;
use std::process::Command;

const CREATE_NO_WINDOW: u32 = 0x0800_0000;

pub trait Autostart: Send + Sync {
    fn is_enabled(&self) -> Result<bool, String>;
    fn set_enabled(&self, enabled: bool) -> Result<(), String>;
}

/// The real one: drives Task Scheduler through `schtasks.exe`.
pub struct TaskScheduler {
    /// Task name in Task Scheduler.
    pub task: &'static str,
    /// Appended to the executable path, e.g. `--minimized`.
    pub args: &'static str,
}

fn schtasks(args: &[&str]) -> Result<std::process::Output, String> {
    Command::new("schtasks.exe").args(args).creation_flags(CREATE_NO_WINDOW).output().map_err(|e| format!("cannot run schtasks: {e}"))
}

impl Autostart for TaskScheduler {
    fn is_enabled(&self) -> Result<bool, String> {
        Ok(schtasks(&["/Query", "/TN", self.task])?.status.success())
    }

    fn set_enabled(&self, enabled: bool) -> Result<(), String> {
        let out = if enabled {
            let exe = std::env::current_exe().map_err(|e| e.to_string())?;
            let user = format!("{}\\{}", std::env::var("USERDOMAIN").unwrap_or_default(), std::env::var("USERNAME").unwrap_or_default());
            let xml = task_xml(&exe.to_string_lossy(), self.args, &user);
            // schtasks wants the XML as UTF-16 with a byte-order mark.
            let bytes: Vec<u8> = std::iter::once(0xFEFFu16).chain(xml.encode_utf16()).flat_map(u16::to_le_bytes).collect();
            let file = std::env::temp_dir().join("freeseries-task.xml");
            std::fs::write(&file, bytes).map_err(|e| e.to_string())?;
            let out = schtasks(&["/Create", "/TN", self.task, "/XML", &file.to_string_lossy(), "/F"]);
            let _ = std::fs::remove_file(&file);
            out?
        } else {
            if !self.is_enabled()? {
                return Ok(());
            }
            schtasks(&["/Delete", "/TN", self.task, "/F"])?
        };
        if out.status.success() {
            Ok(())
        } else {
            Err(format!("Task Scheduler refused: {}", String::from_utf8_lossy(&out.stderr).trim()))
        }
    }
}

fn escape(text: &str) -> String {
    text.replace('&', "&amp;").replace('<', "&lt;").replace('>', "&gt;").replace('"', "&quot;")
}

/// Task Scheduler's defaults are wrong for an audio engine, so each one is set on purpose:
/// it would skip the launch on battery, kill the app after 72 hours, and run it below normal priority.
fn task_xml(exe: &str, args: &str, user: &str) -> String {
    format!(
        r#"<?xml version="1.0" encoding="UTF-16"?>
<Task version="1.2" xmlns="http://schemas.microsoft.com/windows/2004/02/mit/task">
  <RegistrationInfo><Description>Starts freeseries in the tray so your audio works right after you log on.</Description></RegistrationInfo>
  <Triggers><LogonTrigger><Enabled>true</Enabled><UserId>{user}</UserId></LogonTrigger></Triggers>
  <Principals><Principal id="Author"><UserId>{user}</UserId><LogonType>InteractiveToken</LogonType><RunLevel>LeastPrivilege</RunLevel></Principal></Principals>
  <Settings>
    <MultipleInstancesPolicy>IgnoreNew</MultipleInstancesPolicy>
    <DisallowStartIfOnBatteries>false</DisallowStartIfOnBatteries>
    <StopIfGoingOnBatteries>false</StopIfGoingOnBatteries>
    <ExecutionTimeLimit>PT0S</ExecutionTimeLimit>
    <Priority>4</Priority>
    <AllowHardTerminate>false</AllowHardTerminate>
    <StartWhenAvailable>true</StartWhenAvailable>
    <IdleSettings><StopOnIdleEnd>false</StopOnIdleEnd><RestartOnIdle>false</RestartOnIdle></IdleSettings>
  </Settings>
  <Actions Context="Author"><Exec><Command>{exe}</Command><Arguments>{args}</Arguments></Exec></Actions>
</Task>"#,
        user = escape(user),
        exe = escape(exe),
        args = escape(args),
    )
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn task_survives_battery_long_uptime_and_odd_paths() {
        let xml = task_xml(r"C:\Users\R&D <me>\freeseries-app.exe", "--minimized", r"PC\R&D");
        assert!(xml.contains("<DisallowStartIfOnBatteries>false</DisallowStartIfOnBatteries>"));
        assert!(xml.contains("<StopIfGoingOnBatteries>false</StopIfGoingOnBatteries>"));
        assert!(xml.contains("<ExecutionTimeLimit>PT0S</ExecutionTimeLimit>"));
        assert!(xml.contains("<LogonTrigger>"));
        assert!(xml.contains(r"<Command>C:\Users\R&amp;D &lt;me&gt;\freeseries-app.exe</Command>"));
        assert!(xml.contains("<Arguments>--minimized</Arguments>"));
        assert!(xml.contains(r"<UserId>PC\R&amp;D</UserId>"));
    }

    /// Touches the real Task Scheduler: `cargo test -- --ignored`. Needs no admin rights.
    #[test]
    #[ignore]
    fn real_task_scheduler_round_trip() {
        let auto = TaskScheduler { task: "freeseries-selftest", args: "--minimized" };
        auto.set_enabled(false).unwrap();
        assert!(!auto.is_enabled().unwrap());
        auto.set_enabled(true).unwrap();
        assert!(auto.is_enabled().unwrap());
        auto.set_enabled(false).unwrap();
        assert!(!auto.is_enabled().unwrap());
    }
}
