//! The commit LightTouch is built from (`lighttouch_core::COMMIT`), for logs and problem reports. A source archive
//! without git can set LIGHTTOUCH_COMMIT; otherwise it is "unknown".
use std::process::Command;

fn git(args: &[&str]) -> Option<String> {
    let output = Command::new("git").args(args).output().ok()?;
    output.status.success().then(|| String::from_utf8_lossy(&output.stdout).trim().to_string())
}

fn main() {
    println!("cargo:rerun-if-env-changed=LIGHTTOUCH_COMMIT");
    let commit = std::env::var("LIGHTTOUCH_COMMIT").ok().filter(|c| !c.is_empty()).or_else(|| {
        let hash = git(&["rev-parse", "--short=10", "HEAD"])?;
        // A commit or checkout moves HEAD's reflog: build again then.
        if let Some(dir) = git(&["rev-parse", "--path-format=absolute", "--git-dir"]) {
            println!("cargo:rerun-if-changed={dir}/HEAD");
            println!("cargo:rerun-if-changed={dir}/logs/HEAD");
            println!("cargo:rerun-if-changed={dir}/index");
        }
        // A build with uncommitted changes says so, so a log never claims a commit it wasn't built from.
        let dirty = git(&["status", "--porcelain", "--untracked-files=no"]).is_some_and(|s| !s.is_empty());
        Some(if dirty { format!("{hash}-modified") } else { hash })
    });
    println!("cargo:rustc-env=LIGHTTOUCH_COMMIT={}", commit.as_deref().unwrap_or("unknown"));
}
