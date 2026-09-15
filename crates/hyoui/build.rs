use std::process::Command;

fn git_path(name: &str) -> Option<String> {
    let output = Command::new("git")
        .args(["rev-parse", "--git-path", name])
        .output()
        .ok()?;
    if !output.status.success() {
        return None;
    }
    String::from_utf8(output.stdout)
        .ok()
        .map(|path| path.trim().to_string())
}

fn jj_has_changes() -> Option<bool> {
    let output = Command::new("jj")
        .args(["diff", "--summary"])
        .output()
        .ok()?;
    output.status.success().then(|| !output.stdout.is_empty())
}

fn main() {
    println!("cargo:rerun-if-env-changed=HYOUI_BUILD_ID");
    if let Some(head) = git_path("HEAD") {
        println!("cargo:rerun-if-changed={head}");
    }
    if let Ok(reference) = Command::new("git")
        .args(["symbolic-ref", "-q", "HEAD"])
        .output()
        && reference.status.success()
        && let Ok(reference) = String::from_utf8(reference.stdout)
        && let Some(reference) = git_path(reference.trim())
    {
        println!("cargo:rerun-if-changed={reference}");
    }

    if let Some(build_id) = std::env::var_os("HYOUI_BUILD_ID").and_then(|v| v.into_string().ok()) {
        println!("cargo:rustc-env=HYOUI_BUILD_ID={build_id}");
        return;
    }

    let Ok(output) = Command::new("git")
        .args(["rev-parse", "--short", "HEAD"])
        .output()
    else {
        return;
    };
    if !output.status.success() {
        return;
    }
    let Ok(mut build_id) = String::from_utf8(output.stdout) else {
        return;
    };
    build_id.truncate(build_id.trim_end().len());
    if build_id.is_empty() {
        return;
    }

    let dirty = Command::new("git")
        .args(["status", "--porcelain", "--untracked-files=no"])
        .output()
        .ok()
        .filter(|output| output.status.success())
        .map(|output| !output.stdout.is_empty())
        .or_else(jj_has_changes)
        .unwrap_or(false);
    if dirty {
        build_id.push_str("-dirty");
    }
    println!("cargo:rustc-env=HYOUI_BUILD_ID={build_id}");
}
