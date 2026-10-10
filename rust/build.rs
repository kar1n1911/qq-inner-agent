use std::process::Command;
fn main() {
    // Track revision changes, including refs in linked worktrees.
    println!("cargo:rerun-if-changed=../.git");
    let path = Command::new("git")
        .args(["rev-parse", "--git-path", "HEAD"])
        .output()
        .unwrap();
    println!(
        "cargo:rerun-if-changed={}",
        String::from_utf8_lossy(&path.stdout).trim()
    );
    let refs = Command::new("git")
        .args(["rev-parse", "--git-common-dir"])
        .output()
        .unwrap();
    println!(
        "cargo:rerun-if-changed={}/refs",
        String::from_utf8_lossy(&refs.stdout).trim()
    );
    println!(
        "cargo:rerun-if-changed={}/packed-refs",
        String::from_utf8_lossy(&refs.stdout).trim()
    );
    println!("cargo:rerun-if-env-changed=QQ_BUILD_REVISION");
    let revision = std::env::var("QQ_BUILD_REVISION").unwrap_or_else(|_| {
        Command::new("git")
            .args(["rev-parse", "HEAD"])
            .output()
            .ok()
            .filter(|o| o.status.success())
            .map(|o| String::from_utf8_lossy(&o.stdout).trim().to_owned())
            .unwrap_or_else(|| "unknown".into())
    });
    println!("cargo:rustc-env=QQ_BUILD_REVISION={revision}");
}
