fn main() {
    // The commit a build came from, for the About section and for bug reports.
    let commit = std::process::Command::new("git")
        .args(["rev-parse", "--short", "HEAD"])
        .output()
        .ok()
        .filter(|out| out.status.success())
        .map(|out| String::from_utf8_lossy(&out.stdout).trim().to_string())
        .unwrap_or_else(|| "unknown".into());
    println!("cargo:rustc-env=FREESERIES_COMMIT={commit}");
    println!("cargo:rerun-if-changed=../../.git/HEAD");
    tauri_build::build()
}
