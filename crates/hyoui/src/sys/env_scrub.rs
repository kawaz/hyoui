//! Child PTY env scrub (DR-0024、DR-0023 を Superseded).
//!
//! 親 hyoui ホスト process (例: Claude Code session) が export している
//! Internal Context env が、`hyoui run -- <cmd>` 経由で子 process に POSIX
//! fork→exec で素通しで漏れる現象を解消するための target-aware env scrub。
//!
//! DR-0024 redesign で:
//!
//! - CLI flag は `--no-scrub-env` 1 個のみ
//! - kill/keep glob の user 設定は `~/.config/hyoui/config.toml` 経由
//! - target 推定は `command[0]` の basename のみ (env wrapper unwrap なし)
//!
//! 親 (= `hyoui-cli` main) で [`resolve_plan`] により kill/keep glob patterns を
//! 解決して `DaemonizeInit` に詰め、daemon child (= `run_daemon_child`) が
//! [`apply`] を呼んで自 process の environ から match する env を削除する。
//! environ 削除した状態で `Session::start` が fork+execvp するので、子 PTY が
//! 継承する environ から該当 env が除外される。
//!
//! 詳細は `docs/decisions/DR-0024-env-scrub-config-file.md` を参照。

use crate::config::{Config, TargetConfig};
use crate::sys::env::remove_var_at_startup;

/// 削除対象から強制的に除外する env 名 prefix (= hyoui 自身が DR-0018 / DR-0020
/// 等で意図的に子へ注入する env を保護する)。
pub const PROTECTED_PREFIX: &str = "HYOUI_";

/// builtin kill/keep default を持つ target 名一覧。
///
/// [`builtin_kill_defaults`] / [`builtin_keep_defaults`] が非空を返す target を
/// 列挙する (= `hyoui config show` が builtin を注記として展開するための索引)。
/// builtin を足すときは match arm と本定数を同時に更新する (= 対応は下の
/// `builtin_targets_all_have_defaults` test で機械検証)。
pub const BUILTIN_TARGETS: &[&str] = &["claude"];

/// target ごとの組み込み default kill_glob patterns (DR-0024 §4)。
///
/// 出典:
/// - `CLAUDECODE` / `CLAUDE_CODE_*` / `CLAUDE_JOB_DIR` / `CLAUDE_PLUGIN_DATA`:
///   Claude Code 公式 env-vars docs "Claude Internal Context" セクション
///   (= 子プロセスへ auto-export と明記)
/// - `AI_AGENT`: Vercel `@vercel/detect-agent` convention (Claude Code バイナリ内
///   で `claude-code_<version>_agent` を hardcoded export)
pub fn builtin_kill_defaults(target: &str) -> &'static [&'static str] {
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

/// target ごとの組み込み default keep_glob patterns (DR-0024 §4 = 現状空)。
///
/// 将来 builtin で保護したい pattern が出てきたらここに追加する。
pub fn builtin_keep_defaults(_target: &str) -> &'static [&'static str] {
    &[]
}

/// `command[0]` から target 名を推定する (= basename 抽出)。
///
/// 推定に失敗した (= 空 / 非 UTF-8) 場合は `None` を返す。env wrapper
/// (= `hyoui run -- env FOO=bar claude`) では `env` が target になる
/// = builtin 未登録なので scrub なし。env wrapper サポートは DR-0024 で対象外
/// が確定 (user は wrapper を使わず素直に target を書く)。
pub fn infer_target(command: &[String]) -> Option<String> {
    let first = command.first()?;
    let basename = std::path::Path::new(first).file_name()?.to_str()?;
    if basename.is_empty() {
        return None;
    }
    Some(basename.to_string())
}

/// 親側で config + builtin defaults を合成して `ScrubPlan` を解決する。
///
/// 返り値が `None` の場合 = scrub 完全無効 (= `--no-scrub-env` または config の
/// `scrub_env_enabled = false`)。
///
/// 解決規則 (DR-0024 §3):
///
/// 1. `no_scrub_env || !config.scrub_env_enabled` → `None`
/// 2. target = [`infer_target`] で argv basename を推定。失敗 (= 空 / 非 UTF-8) →
///    no-op の空 `ScrubPlan` を返す (= scrub は機能するが削除候補なし)
/// 3. config から target lookup (= 未指定なら `TargetConfig::default`)
/// 4. `inherit_builtin = true` → builtin kill/keep + user kill/keep を concat
/// 5. `inherit_builtin = false` → user kill/keep のみ
/// 6. dedup したうえで `ScrubPlan` に詰める
pub fn resolve_plan(no_scrub_env: bool, config: &Config, command: &[String]) -> Option<ScrubPlan> {
    if no_scrub_env || !config.scrub_env.enabled {
        return None;
    }
    let target = infer_target(command);
    let target_cfg = target
        .as_deref()
        .and_then(|t| config.scrub_env.targets.get(t));
    let default_cfg = TargetConfig::default();
    let cfg = target_cfg.unwrap_or(&default_cfg);

    let mut patterns: Vec<String> = Vec::new();
    let mut keep: Vec<String> = Vec::new();

    if cfg.inherit_builtin
        && let Some(t) = target.as_deref()
    {
        for p in builtin_kill_defaults(t) {
            patterns.push((*p).to_string());
        }
        for p in builtin_keep_defaults(t) {
            keep.push((*p).to_string());
        }
    }
    for p in &cfg.kill_glob {
        if !patterns.iter().any(|x| x == p) {
            patterns.push(p.clone());
        }
    }
    for p in &cfg.keep_glob {
        if !keep.iter().any(|x| x == p) {
            keep.push(p.clone());
        }
    }

    Some(ScrubPlan { patterns, keep })
}

/// 親で解決した scrub 計画 (= daemon child に渡す wire 形式)。
///
/// `DaemonizeInit` に乗せて env JSON で運ぶ。`patterns` は env 名と glob match
/// したら削除候補、`keep` は env 名と glob match したら削除を skip。
#[derive(Debug, Clone, Default, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
pub struct ScrubPlan {
    /// 削除候補 kill_glob patterns (= builtin + config user kill_glob)。
    pub patterns: Vec<String>,
    /// 削除を skip する keep_glob patterns (= builtin + config user keep_glob)。
    #[serde(default)]
    pub keep: Vec<String>,
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

/// scrub 結果 = 削除した env 名一覧 + skip 種別ごとの env 名一覧。
#[derive(Debug, Default, Clone, PartialEq, Eq)]
pub struct ScrubResult {
    /// 実際に environ から削除した env 名 (= patterns match かつ keep/protected 非該当)。
    pub removed: Vec<String>,
    /// patterns に match したが keep_glob で削除を skip した env 名。
    pub keep_skips: Vec<String>,
    /// patterns に match したが `HYOUI_*` protected で削除を skip した env 名。
    pub protected_hits: Vec<String>,
}

/// daemon 子 process の environ に対して [`ScrubPlan`] を適用する。
///
/// 判定順:
/// 1. env 名が `plan.patterns` (kill_glob) に 1 つも match しない → 維持
/// 2. env 名が `plan.keep` (keep_glob) に 1 つでも match する → 維持 (= `keep_skips` に積む)
/// 3. env 名が `HYOUI_*` で始まる → 維持 (= `protected_hits` に積む)
/// 4. 上記 1-3 いずれにも該当しなければ削除 (= `removed` に積む)
///
/// 戻り値 [`ScrubResult`] には「削除した env 名」「keep glob で skip した env 名」
/// 「protected (`HYOUI_*`) で skip した env 名」を分けて積む。log 出力 (= stderr) は
/// caller 責務 (= default 無音、必要なら将来 opt-in で出す。DR-0024 §10)。
///
/// # Safety (caller 契約)
/// **process 起動初期 + single-threaded** でのみ呼ぶこと。`Session::start` 前の
/// daemon child から呼ぶ想定 (= [`remove_var_at_startup`] と同じ前提)。
pub fn apply(plan: &ScrubPlan) -> ScrubResult {
    let mut result = ScrubResult::default();
    if plan.patterns.is_empty() {
        return result;
    }
    let env_names: Vec<String> = std::env::vars_os()
        .filter_map(|(k, _)| k.into_string().ok())
        .collect();
    for name in env_names {
        let hit = plan.patterns.iter().any(|p| glob_match(p, &name));
        if !hit {
            continue;
        }
        if plan.keep.iter().any(|p| glob_match(p, &name)) {
            result.keep_skips.push(name);
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
    result.keep_skips.sort();
    result.protected_hits.sort();
    result
}

#[cfg(test)]
mod tests {
    use super::*;

    fn cfg_with(target: &str, t: TargetConfig) -> Config {
        let mut c = Config::default();
        c.scrub_env.targets.insert(target.to_string(), t);
        c
    }

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
    fn builtin_kill_defaults_claude() {
        let d = builtin_kill_defaults("claude");
        assert!(d.contains(&"CLAUDECODE"));
        assert!(d.contains(&"CLAUDE_CODE_SESSION_ID"));
        assert!(d.contains(&"AI_AGENT"));
        assert_eq!(d.len(), 9);
    }

    #[test]
    fn builtin_targets_all_have_defaults() {
        // BUILTIN_TARGETS は「builtin を持つ target の索引」。空を返す target が
        // 混ざっていたら索引が腐っている。
        for t in BUILTIN_TARGETS {
            assert!(
                !builtin_kill_defaults(t).is_empty() || !builtin_keep_defaults(t).is_empty(),
                "BUILTIN_TARGETS lists `{t}` but it has no builtin defaults"
            );
        }
    }

    #[test]
    fn builtin_defaults_unknown_target_is_empty() {
        assert_eq!(builtin_kill_defaults("vim").len(), 0);
        assert_eq!(builtin_kill_defaults("cat").len(), 0);
        assert_eq!(builtin_kill_defaults("").len(), 0);
        assert_eq!(builtin_keep_defaults("claude").len(), 0);
    }

    #[test]
    fn resolve_plan_no_scrub_flag_disables() {
        let cfg = Config::default();
        let r = resolve_plan(true, &cfg, &["claude".into()]);
        assert_eq!(r, None);
    }

    #[test]
    fn resolve_plan_config_disabled_overrides_builtin() {
        let mut cfg = Config::default();
        cfg.scrub_env.enabled = false;
        let r = resolve_plan(false, &cfg, &["claude".into()]);
        assert_eq!(r, None);
    }

    #[test]
    fn resolve_plan_claude_default_uses_builtin() {
        let cfg = Config::default();
        let r = resolve_plan(false, &cfg, &["claude".into()]).unwrap();
        assert_eq!(r.patterns.len(), 9);
        assert!(r.patterns.contains(&"CLAUDECODE".into()));
        assert!(r.patterns.contains(&"AI_AGENT".into()));
        assert!(r.keep.is_empty());
    }

    #[test]
    fn resolve_plan_unknown_target_no_builtin_no_user() {
        let cfg = Config::default();
        let r = resolve_plan(false, &cfg, &["vim".into()]).unwrap();
        assert!(r.patterns.is_empty());
        assert!(r.keep.is_empty());
    }

    #[test]
    fn resolve_plan_inherit_true_concats_user_kill() {
        let cfg = cfg_with(
            "claude",
            TargetConfig {
                inherit_builtin: true,
                kill_glob: vec!["CMUXMSG_*".into()],
                keep_glob: vec![],
            },
        );
        let r = resolve_plan(false, &cfg, &["claude".into()]).unwrap();
        assert!(r.patterns.contains(&"CLAUDECODE".into()));
        assert!(r.patterns.contains(&"CMUXMSG_*".into()));
        assert_eq!(r.patterns.len(), 10);
    }

    #[test]
    fn resolve_plan_inherit_true_concats_user_keep() {
        let cfg = cfg_with(
            "claude",
            TargetConfig {
                inherit_builtin: true,
                kill_glob: vec![],
                keep_glob: vec!["AI_AGENT".into()],
            },
        );
        let r = resolve_plan(false, &cfg, &["claude".into()]).unwrap();
        assert_eq!(r.patterns.len(), 9); // builtin 9 個
        assert_eq!(r.keep, vec!["AI_AGENT"]);
    }

    #[test]
    fn resolve_plan_inherit_false_drops_builtin() {
        let cfg = cfg_with(
            "claude",
            TargetConfig {
                inherit_builtin: false,
                kill_glob: vec!["MYTOOL_SECRET".into()],
                keep_glob: vec![],
            },
        );
        let r = resolve_plan(false, &cfg, &["claude".into()]).unwrap();
        assert!(!r.patterns.contains(&"CLAUDECODE".into()));
        assert_eq!(r.patterns, vec!["MYTOOL_SECRET"]);
    }

    #[test]
    fn resolve_plan_dedupes_user_against_builtin() {
        let cfg = cfg_with(
            "claude",
            TargetConfig {
                inherit_builtin: true,
                kill_glob: vec!["AI_AGENT".into(), "CMUXMSG_*".into()],
                keep_glob: vec![],
            },
        );
        let r = resolve_plan(false, &cfg, &["claude".into()]).unwrap();
        // AI_AGENT は builtin に既に存在 → 重複しない
        assert_eq!(r.patterns.iter().filter(|p| *p == "AI_AGENT").count(), 1);
        assert!(r.patterns.contains(&"CMUXMSG_*".into()));
    }

    #[test]
    fn resolve_plan_user_only_target_with_no_builtin() {
        // builtin 未登録の target でも config から user 設定を生かせる。
        let cfg = cfg_with(
            "my-tool",
            TargetConfig {
                inherit_builtin: true,
                kill_glob: vec!["MYTOOL_SECRET".into()],
                keep_glob: vec![],
            },
        );
        let r = resolve_plan(false, &cfg, &["my-tool".into()]).unwrap();
        assert_eq!(r.patterns, vec!["MYTOOL_SECRET"]);
    }

    #[test]
    fn resolve_plan_env_wrapper_does_not_unwrap() {
        // DR-0024 §2: env wrapper サポートなし。argv[0] が "env" の場合、target = "env"
        // になり builtin 未登録なので scrub は no-op。user は wrapper を使わず素直に書く。
        let cfg = Config::default();
        let r = resolve_plan(
            false,
            &cfg,
            &["env".into(), "FOO=bar".into(), "claude".into()],
        )
        .unwrap();
        assert!(r.patterns.is_empty());
    }

    #[test]
    fn apply_keep_glob_skips_matching_env() {
        // keep glob `CLAUDE_*` は CLAUDE_ で始まる env 全部の削除を skip する。
        // patterns は AI_AGENT も含むが、AI_AGENT は keep glob `CLAUDE_*` に
        // match しないので削除される。
        let plan = ScrubPlan {
            patterns: vec![
                "CLAUDECODE".into(),
                "CLAUDE_CODE_SESSION_ID".into(),
                "AI_AGENT".into(),
            ],
            keep: vec!["CLAUDE_*".into()],
        };
        // glob_match の単体検証 (= apply は process env を弄るので unit test 不可)。
        assert!(glob_match(&plan.keep[0], "CLAUDE_CODE_SESSION_ID"));
        assert!(!glob_match(&plan.keep[0], "AI_AGENT"));
        assert!(!glob_match(&plan.keep[0], "CLAUDECODE")); // CLAUDE_ プレフィックスなし
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
