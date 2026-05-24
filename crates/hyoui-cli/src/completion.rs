//! Shell completion script generation.
//!
//! Each function returns a self-contained completion script for the
//! corresponding shell. The scripts are intentionally hand-written rather
//! than generated, so the supported subcommand/option surface evolves in
//! lock-step with [`hyoui::cli`]. Future stages (`send`, `attach`, `status`)
//! are listed pre-emptively so users get tab-completion on day one.

use hyoui::cli::Shell;

/// Render the completion script for `shell` as plain text suitable to be
/// `eval`-ed or sourced.
pub fn script(shell: Shell) -> String {
    match shell {
        Shell::Bash => bash().to_string(),
        Shell::Zsh => zsh().to_string(),
        Shell::Fish => fish().to_string(),
    }
}

fn bash() -> &'static str {
    r#"# bash completion for hyoui
_hyoui() {
    local cur prev words cword
    _init_completion -n = || return

    # Stop completing once we hit `--`: everything after is the child argv.
    local i
    for (( i=1; i < cword; i++ )); do
        if [[ "${words[i]}" == "--" ]]; then
            return 0
        fi
    done

    # Find the subcommand (first non-flag word after argv[0]).
    local sub=""
    for (( i=1; i < cword; i++ )); do
        local w="${words[i]}"
        case "$w" in
            -*) ;;
            *) sub="$w"; break ;;
        esac
    done

    if [[ -z "$sub" ]]; then
        # Top-level: subcommands + global flags.
        COMPREPLY=( $(compgen -W "run completion send attach status --help -h --version -V" -- "$cur") )
        return 0
    fi

    case "$sub" in
        run)
            case "$prev" in
                --mode)
                    COMPREPLY=( $(compgen -W "interactive headless" -- "$cur") ); return 0 ;;
                --on-child-suspend)
                    COMPREPLY=( $(compgen -W "follow auto-resume" -- "$cur") ); return 0 ;;
                --on-parent-suspend)
                    COMPREPLY=( $(compgen -W "transparent decouple" -- "$cur") ); return 0 ;;
                --socket)
                    _filedir 2>/dev/null || COMPREPLY=( $(compgen -f -- "$cur") ); return 0 ;;
                --timeout|--idle-timeout|--until|--size|--cols|--rows)
                    return 0 ;;
            esac
            case "$cur" in
                --mode=*)
                    COMPREPLY=( $(compgen -W "interactive headless" -- "${cur#*=}") ); return 0 ;;
                --on-child-suspend=*)
                    COMPREPLY=( $(compgen -W "follow auto-resume" -- "${cur#*=}") ); return 0 ;;
                --on-parent-suspend=*)
                    COMPREPLY=( $(compgen -W "transparent decouple" -- "${cur#*=}") ); return 0 ;;
            esac
            COMPREPLY=( $(compgen -W "--mode --socket --timeout --idle-timeout --until --on-child-suspend --on-parent-suspend --size --cols --rows --help -h --" -- "$cur") )
            return 0 ;;
        completion)
            COMPREPLY=( $(compgen -W "bash zsh fish --help -h" -- "$cur") )
            return 0 ;;
        *)
            return 0 ;;
    esac
}
complete -F _hyoui hyoui
"#
}

fn zsh() -> &'static str {
    r#"#compdef hyoui
# zsh completion for hyoui

_hyoui() {
    local context state state_descr line
    typeset -A opt_args

    _arguments -C \
        '(- *)'{-h,--help}'[Show help and exit]' \
        '(- *)'{-V,--version}'[Show version and exit]' \
        '1: :_hyoui_subcommands' \
        '*::arg:->args'

    case $state in
        args)
            case $line[1] in
                run)
                    _hyoui_run
                    ;;
                completion)
                    _arguments \
                        '1:shell:(bash zsh fish)' \
                        '(-h --help)'{-h,--help}'[Show help]'
                    ;;
            esac
            ;;
    esac
}

_hyoui_subcommands() {
    local -a subs
    subs=(
        'run:Run a command inside a PTY as a transparent proxy'
        'completion:Print a shell completion script'
        'send:(reserved) Send input to a running session'
        'attach:(reserved) Attach to a running session'
        'status:(reserved) Show session status'
    )
    _describe -t commands 'hyoui subcommand' subs
}

_hyoui_run() {
    _arguments \
        '--mode=[Operating mode]:mode:(interactive headless)' \
        '--socket=[Unix socket path]:socket:_files' \
        '--timeout=[Overall timeout in seconds]:seconds:' \
        '--idle-timeout=[Output idle timeout in seconds]:seconds:' \
        '--until=[Terminate when PATTERN appears in output]:pattern:' \
        '--size=[Virtual screen size COLSxROWS]:size:' \
        '--cols=[Virtual screen columns]:cols:' \
        '--rows=[Virtual screen rows]:rows:' \
        '--on-child-suspend=[Action when child is stopped]:action:(follow auto-resume)' \
        '--on-parent-suspend=[Action when parent is stopped]:action:(transparent decouple)' \
        '(-h --help)'{-h,--help}'[Show help]' \
        '*::child command:_normal'
}

_hyoui "$@"
"#
}

fn fish() -> &'static str {
    r#"# fish completion for hyoui

# Detect whether a known subcommand has already been provided.
function __hyoui_using_subcommand
    set -l cmd (commandline -opc)
    set -e cmd[1]
    for arg in $cmd
        switch $arg
            case run completion send attach status
                if test "$arg" = "$argv[1]"
                    return 0
                end
                return 1
        end
    end
    return 1
end

function __hyoui_no_subcommand
    set -l cmd (commandline -opc)
    set -e cmd[1]
    for arg in $cmd
        switch $arg
            case run completion send attach status
                return 1
        end
    end
    return 0
end

# Top-level: subcommands.
complete -c hyoui -n __hyoui_no_subcommand -f -a run        -d 'Run a command inside a PTY as a transparent proxy'
complete -c hyoui -n __hyoui_no_subcommand -f -a completion -d 'Print a shell completion script'
complete -c hyoui -n __hyoui_no_subcommand -f -a send       -d '(reserved) Send input to a running session'
complete -c hyoui -n __hyoui_no_subcommand -f -a attach     -d '(reserved) Attach to a running session'
complete -c hyoui -n __hyoui_no_subcommand -f -a status     -d '(reserved) Show session status'

# Top-level global flags.
complete -c hyoui -n __hyoui_no_subcommand -s h -l help    -d 'Show help and exit'
complete -c hyoui -n __hyoui_no_subcommand -s V -l version -d 'Show version and exit'

# `hyoui run` options.
complete -c hyoui -n '__hyoui_using_subcommand run' -l mode              -x -a 'interactive headless' -d 'Operating mode'
complete -c hyoui -n '__hyoui_using_subcommand run' -l socket            -r -F                          -d 'Unix socket path'
complete -c hyoui -n '__hyoui_using_subcommand run' -l timeout           -x                              -d 'Overall timeout in seconds'
complete -c hyoui -n '__hyoui_using_subcommand run' -l idle-timeout      -x                              -d 'Output idle timeout in seconds'
complete -c hyoui -n '__hyoui_using_subcommand run' -l until             -x                              -d 'Terminate when PATTERN appears'
complete -c hyoui -n '__hyoui_using_subcommand run' -l size              -x                              -d 'Virtual screen size COLSxROWS'
complete -c hyoui -n '__hyoui_using_subcommand run' -l cols              -x                              -d 'Virtual screen columns'
complete -c hyoui -n '__hyoui_using_subcommand run' -l rows              -x                              -d 'Virtual screen rows'
complete -c hyoui -n '__hyoui_using_subcommand run' -l on-child-suspend  -x -a 'follow auto-resume'      -d 'Action when child is stopped'
complete -c hyoui -n '__hyoui_using_subcommand run' -l on-parent-suspend -x -a 'transparent decouple'    -d 'Action when parent is stopped'
complete -c hyoui -n '__hyoui_using_subcommand run' -s h -l help                                          -d 'Show help and exit'

# `hyoui completion` options.
complete -c hyoui -n '__hyoui_using_subcommand completion' -f -a 'bash zsh fish' -d 'Target shell'
complete -c hyoui -n '__hyoui_using_subcommand completion' -s h -l help          -d 'Show help and exit'
"#
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn completion_bash_contains_run_subcommand() {
        let s = script(Shell::Bash);
        assert!(s.contains("complete -F _hyoui"));
        assert!(s.contains("run"));
        assert!(s.contains("completion"));
        assert!(s.contains("--mode"));
    }

    #[test]
    fn completion_zsh_starts_with_compdef() {
        let s = script(Shell::Zsh);
        assert!(s.starts_with("#compdef hyoui"));
        assert!(s.contains("_arguments"));
        assert!(s.contains("interactive headless"));
    }

    #[test]
    fn completion_fish_uses_complete_c() {
        let s = script(Shell::Fish);
        assert!(s.contains("complete -c hyoui"));
        assert!(s.contains("bash zsh fish"));
        assert!(s.contains("--mode") || s.contains(" mode "));
    }

    #[test]
    fn completion_all_shells_mention_reserved_subcommands() {
        for sh in [Shell::Bash, Shell::Zsh, Shell::Fish] {
            let s = script(sh);
            assert!(s.contains("send"), "shell {sh:?} missing `send`");
            assert!(s.contains("attach"), "shell {sh:?} missing `attach`");
            assert!(s.contains("status"), "shell {sh:?} missing `status`");
        }
    }
}
