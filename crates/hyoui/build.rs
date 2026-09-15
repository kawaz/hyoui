use std::path::PathBuf;
use std::process::Command;

fn run(program: &str, args: &[&str]) -> Option<String> {
    let output = Command::new(program).args(args).output().ok()?;
    if !output.status.success() {
        return None;
    }
    let text = String::from_utf8(output.stdout).ok()?;
    Some(text.trim_end().to_string())
}

fn git_path(name: &str) -> Option<String> {
    run("git", &["rev-parse", "--git-path", name])
}

/// jj workspace の repo ディレクトリ。`.jj/repo` は dir か、実体 path を書いた file。
fn jj_repo_dir() -> Option<PathBuf> {
    let workspace = PathBuf::from(run("jj", &["--ignore-working-copy", "root"])?);
    let dot_jj = workspace.join(".jj");
    let repo = dot_jj.join("repo");
    if repo.is_dir() {
        return Some(repo);
    }
    let target = std::fs::read_to_string(&repo).ok()?;
    let resolved = dot_jj.join(target.trim());
    Some(resolved.canonicalize().unwrap_or(resolved))
}

/// jj の @- (= 今ビルドしているコードの commit) と、jj が最後に snapshot した working copy の dirty。
fn jj_build_id() -> Option<String> {
    let mut build_id = run(
        "jj",
        &[
            "--ignore-working-copy",
            "log",
            "-r",
            "@-",
            "--no-graph",
            "-T",
            "commit_id.short(8)",
        ],
    )?;
    if build_id.is_empty() {
        return None;
    }
    let dirty = run("jj", &["--ignore-working-copy", "diff", "--summary"])
        .is_some_and(|summary| !summary.is_empty());
    if dirty {
        build_id.push_str("-dirty");
    }
    Some(build_id)
}

fn git_build_id() -> Option<String> {
    let mut build_id = run("git", &["rev-parse", "--short", "HEAD"])?;
    if build_id.is_empty() {
        return None;
    }
    let dirty = run("git", &["status", "--porcelain", "--untracked-files=no"])
        .is_some_and(|status| !status.is_empty());
    if dirty {
        build_id.push_str("-dirty");
    }
    Some(build_id)
}

fn main() {
    println!("cargo:rerun-if-env-changed=HYOUI_BUILD_ID");

    let jj_repo = jj_repo_dir();
    if let Some(repo) = &jj_repo {
        // jj の操作 (commit / new / undo) は op head を進めるので、ここを見れば足りる。
        println!(
            "cargo:rerun-if-changed={}",
            repo.join("op_heads/heads").display()
        );
    } else {
        if let Some(head) = git_path("HEAD") {
            println!("cargo:rerun-if-changed={head}");
        }
        if let Some(reference) = run("git", &["symbolic-ref", "-q", "HEAD"])
            .and_then(|reference| git_path(reference.trim()))
        {
            println!("cargo:rerun-if-changed={reference}");
        }
    }

    let build_id = std::env::var_os("HYOUI_BUILD_ID")
        .and_then(|value| value.into_string().ok())
        .filter(|value| !value.is_empty())
        .or_else(|| jj_repo.and_then(|_| jj_build_id()))
        .or_else(git_build_id);

    if let Some(build_id) = build_id {
        println!("cargo:rustc-env=HYOUI_BUILD_ID={build_id}");
    }
}
