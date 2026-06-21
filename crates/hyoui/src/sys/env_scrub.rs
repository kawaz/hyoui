//! Child PTY env scrub (DR-0023).
//!
//! 親 hyoui ホスト process (例: Claude Code session) が export している
//! Internal Context env が、`hyoui run -- <cmd>` 経由で子 process に POSIX
//! fork→exec で素通しで漏れる現象を解消するための target-aware env scrub。
//!
//! 親 (= `hyoui-cli` main) で [`resolve_globs`] により kill_glob patterns を
//! 解決して `DaemonizeInit` に詰め、daemon child (= `run_daemon_child`) が
//! [`apply`] を呼んで自 process の environ から match する env を削除する。
//! environ 削除した状態で `Session::start` が fork+execvp するので、子 PTY が
//! 継承する environ から該当 env が除外される。
//!
//! 詳細は `docs/decisions/DR-0023-child-env-scrub.md` を参照。

use crate::sys::env::remove_var_at_startup;

/// 削除対象から強制的に除外する env 名 prefix (= hyoui 自身が DR-0018 / DR-0020
/// 等で意図的に子へ注入する env を保護する)。
pub const PROTECTED_PREFIX: &str = "HYOUI_";

/// target ごとの組み込み default kill_glob patterns (DR-0023 §3)。
///
/// 出典:
/// - `CLAUDECODE` / `CLAUDE_CODE_*` / `CLAUDE_JOB_DIR` / `CLAUDE_PLUGIN_DATA`:
///   Claude Code 公式 env-vars docs "Claude Internal Context" セクション
///   (= 子プロセスへ auto-export と明記)
/// - `AI_AGENT`: Vercel `@vercel/detect-agent` convention (Claude Code バイナリ内
///   で `claude-code_<version>_agent` を hardcoded export)
pub fn builtin_defaults(target: &str) -> &'static [&'static str] {
    match target {
        "claude" => &[
            "CLAUDECODE",
            "CLAUDE_CODE_CHILD_SESSION",
            "CLAUDE_CODE_SESSION_ID",
            "CLAUDE_CODE_AGENT",
            "CLAUDE_CODE_ENTRYPOINT",
            "CLAUDE_CODE_EXECPATH",
            "CLAUDE_JOB_DIR",
            "CLAUDE_PLUGIN_DATA",
            "AI_AGENT",
        ],
        _ => &[],
    }
}

/// `command[0]` から target 名を推定する (= basename 抽出)。
///
/// 推定に失敗した (= 空 / 非 UTF-8) 場合は `None` を返す。env wrapper
/// (例: `hyoui run -- env FOO=bar claude`) では誤推定するため、user は
/// `--scrub-env-target=claude` で明示 override できる (DR-0023 §2)。
pub fn infer_target(command: &[String]) -> Option<String> {
    let first = command.first()?;
    let basename = std::path::Path::new(first).file_name()?.to_str()?;
    if basename.is_empty() {
        return None;
    }
    Some(basename.to_string())
}

/// 親側で組み込み defaults + add + keep を合成して最終 glob patterns を解決する。
///
/// 返り値が `None` の場合 = scrub 完全無効 (= `--no-scrub-env`)。
/// 返り値が `Some(vec)` の場合 (空 vec を含む) = daemon に渡して `apply` させる
/// (= 空 list なら `apply` は何もしない、target builtin なし & add なしの素通し target)。
pub fn resolve_globs(
    no_scrub_env: bool,
    explicit_target: Option<&str>,
    command: &[String],
    add: &[String],
    keep: &[String],
) -> Option<Vec<String>> {
    if no_scrub_env {
        return None;
    }
    let target = explicit_target
        .map(|s| s.to_string())
        .or_else(|| infer_target(command));
    let builtin: Vec<String> = target
        .as_deref()
        .map(builtin_defaults)
        .unwrap_or(&[])
        .iter()
        .map(|s| (*s).to_string())
        .collect();
    let mut patterns: Vec<String> = builtin;
    for a in add {
        if !patterns.iter().any(|p| p == a) {
            patterns.push(a.clone());
        }
    }
    let keep_set: std::collections::HashSet<&str> = keep.iter().map(|s| s.as_str()).collect();
    patterns.retain(|p| !keep_set.contains(p.as_str()));
    Some(patterns)
}

/// glob match: `*` (= 0 文字以上の任意マッチ) と `?` (= 1 文字マッチ) のみ。
/// 大文字小文字区別あり (= POSIX env 慣習)。escape なし。
pub fn glob_match(pattern: &str, name: &str) -> bool {
    fn rec(p: &[u8], s: &[u8]) -> bool {
        if p.is_empty() {
            return s.is_empty();
        }
        match p[0] {
            b'*' => {
                if rec(&p[1..], s) {
                    return true;
                }
                if s.is_empty() {
                    return false;
                }
                rec(p, &s[1..])
            }
            b'?' => {
                if s.is_empty() {
                    return false;
                }
                rec(&p[1..], &s[1..])
            }
            c => {
                if s.is_empty() || s[0] != c {
                    return false;
                }
                rec(&p[1..], &s[1..])
            }
        }
    }
    rec(pattern.as_bytes(), name.as_bytes())
}

/// env 名が削除対象から保護されているか (= `HYOUI_*`)。
pub fn is_protected(name: &str) -> bool {
    name.starts_with(PROTECTED_PREFIX)
}

/// scrub 結果 = 削除した env 名一覧 + protected で skip した env 名一覧。
#[derive(Debug, Default, Clone, PartialEq, Eq)]
pub struct ScrubResult {
    /// 実際に environ から削除した env 名 (= match かつ非 protected)。
    pub removed: Vec<String>,
    /// match したが `HYOUI_*` protected で削除を skip した env 名。
    pub protected_hits: Vec<String>,
}

/// daemon 子 process の environ に対して patterns を適用、削除する。
///
/// 戻り値 [`ScrubResult`] には「削除した env 名」と「protected (`HYOUI_*`) で
/// match したが削除を skip した env 名」を分けて積む。log 出力 (= stderr) は
/// caller 責務 (= default 無音、必要なら `--verbose` 等で出す。DR-0023 §log 規定)。
///
/// # Safety (caller 契約)
/// **process 起動初期 + single-threaded** でのみ呼ぶこと。`Session::start` 前の
/// daemon child から呼ぶ想定 (= [`remove_var_at_startup`] と同じ前提)。
pub fn apply(patterns: &[String]) -> ScrubResult {
    let mut result = ScrubResult::default();
    if patterns.is_empty() {
        return result;
    }
    let env_names: Vec<String> = std::env::vars_os()
        .filter_map(|(k, _)| k.into_string().ok())
        .collect();
    for name in env_names {
        let hit = patterns.iter().any(|p| glob_match(p, &name));
        if !hit {
            continue;
        }
        if is_protected(&name) {
            result.protected_hits.push(name);
            continue;
        }
        remove_var_at_startup(&name);
        result.removed.push(name);
    }
    result.removed.sort();
    result.protected_hits.sort();
    result
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn glob_match_literal() {
        assert!(glob_match("CLAUDECODE", "CLAUDECODE"));
        assert!(!glob_match("CLAUDECODE", "CLAUDECODES"));
        assert!(!glob_match("CLAUDECODE", "claudecode")); // case sensitive
    }

    #[test]
    fn glob_match_star() {
        assert!(glob_match("CLAUDE_*", "CLAUDE_CODE_SESSION_ID"));
        assert!(glob_match("CLAUDE_*", "CLAUDE_"));
        assert!(!glob_match("CLAUDE_*", "CLAUDE"));
        assert!(glob_match("*", "ANYTHING"));
        assert!(glob_match("*", ""));
        assert!(glob_match("*_TOKEN", "GITHUB_TOKEN"));
        assert!(glob_match("*_TOKEN", "_TOKEN"));
        assert!(!glob_match("*_TOKEN", "TOKEN"));
    }

    #[test]
    fn glob_match_question() {
        assert!(glob_match("A?C", "ABC"));
        assert!(!glob_match("A?C", "AC"));
        assert!(!glob_match("A?C", "ABBC"));
    }

    #[test]
    fn glob_match_mixed() {
        assert!(glob_match("CLAUDE_*_?D", "CLAUDE_SESSION_ID"));
        assert!(glob_match("*CODE*", "CLAUDECODE"));
        assert!(glob_match("*CODE*", "PRECODEPOST"));
    }

    #[test]
    fn infer_target_basename() {
        assert_eq!(infer_target(&["claude".into()]), Some("claude".into()));
        assert_eq!(
            infer_target(&["/usr/local/bin/claude".into()]),
            Some("claude".into())
        );
        assert_eq!(
            infer_target(&["claude".into(), "--name".into(), "foo".into()]),
            Some("claude".into())
        );
        assert_eq!(infer_target(&[]), None);
    }

    #[test]
    fn builtin_defaults_claude() {
        let d = builtin_defaults("claude");
        assert!(d.contains(&"CLAUDECODE"));
        assert!(d.contains(&"CLAUDE_CODE_SESSION_ID"));
        assert!(d.contains(&"AI_AGENT"));
        assert_eq!(d.len(), 9);
    }

    #[test]
    fn builtin_defaults_unknown_target() {
        assert_eq!(builtin_defaults("vim").len(), 0);
        assert_eq!(builtin_defaults("cat").len(), 0);
        assert_eq!(builtin_defaults("").len(), 0);
    }

    #[test]
    fn resolve_globs_no_scrub() {
        let r = resolve_globs(true, None, &["claude".into()], &[], &[]);
        assert_eq!(r, None);
    }

    #[test]
    fn resolve_globs_claude_default() {
        let r = resolve_globs(false, None, &["claude".into()], &[], &[]).unwrap();
        assert!(r.contains(&"CLAUDECODE".into()));
        assert!(r.contains(&"AI_AGENT".into()));
        assert_eq!(r.len(), 9);
    }

    #[test]
    fn resolve_globs_explicit_target_overrides_argv() {
        let r = resolve_globs(false, Some("claude"), &["env".into()], &[], &[]).unwrap();
        assert!(r.contains(&"CLAUDE_CODE_SESSION_ID".into()));
    }

    #[test]
    fn resolve_globs_unknown_target_empty() {
        let r = resolve_globs(false, None, &["vim".into()], &[], &[]).unwrap();
        assert!(r.is_empty());
    }

    #[test]
    fn resolve_globs_add_dedupes() {
        let r = resolve_globs(
            false,
            None,
            &["claude".into()],
            &["AI_AGENT".into(), "CMUXMSG_*".into()],
            &[],
        )
        .unwrap();
        // AI_AGENT は builtin に既にあるので重複しない
        assert_eq!(r.iter().filter(|p| *p == "AI_AGENT").count(), 1);
        assert!(r.contains(&"CMUXMSG_*".into()));
    }

    #[test]
    fn resolve_globs_keep_excludes() {
        let r = resolve_globs(false, None, &["claude".into()], &[], &["AI_AGENT".into()]).unwrap();
        assert!(!r.contains(&"AI_AGENT".into()));
        assert!(r.contains(&"CLAUDECODE".into()));
    }

    #[test]
    fn is_protected_hyoui_prefix() {
        assert!(is_protected("HYOUI_SESSION_ID"));
        assert!(is_protected("HYOUI_NAMESPACE"));
        assert!(is_protected("HYOUI_"));
        assert!(!is_protected("CLAUDECODE"));
        assert!(!is_protected("hyoui_lower"));
    }
}
