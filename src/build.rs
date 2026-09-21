use std::process::Command;

fn main() {
    println!("cargo:rerun-if-changed=.git/HEAD");
    println!("cargo:rerun-if-changed=src/build.rs");
    println!(
        "cargo:rustc-env=GIT_HASH={}",
        git_hash(&["rev-parse", "HEAD"])
    );
    println!(
        "cargo:rustc-env=GIT_HASH_SHORT={}",
        git_hash(&["rev-parse", "--short", "HEAD"])
    );
}

/// Returns `unknown` when git is unavailable or the build is not in a git
/// repository (the container build excludes `.git`).
fn git_hash(args: &[&str]) -> String {
    let Ok(output) = Command::new("git").args(args).output() else {
        return "unknown".to_string();
    };
    if !output.status.success() {
        return "unknown".to_string();
    }

    String::from_utf8(output.stdout)
        .map(|hash| hash.trim().to_string())
        .map_or_else(
            |_| "unknown".to_string(),
            |hash| {
                if hash.is_empty() {
                    "unknown".to_string()
                } else {
                    hash
                }
            },
        )
}
