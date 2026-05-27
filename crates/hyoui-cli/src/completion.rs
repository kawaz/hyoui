//! Shell completion script generation.
//!
//! Each function returns a self-contained completion script for the
//! corresponding shell. The scripts are intentionally hand-written rather
//! than generated, so the supported subcommand/option surface evolves in
//! lock-step with [`hyoui::cli`]. Future stages (`send`, `detach`) are
//! listed pre-emptively so users get tab-completion on day one.

use hyoui::cli::Shell;

/// Render the completion script for `shell` as plain text suitable to be
/// `eval`-ed or sourced.
pub fn script(shell: Shell) -> String {
    match shell {
        Shell::Bash => bash().to_string(),
        Shell::Zsh => zsh().to_string(),
        Shell::Fish => fish().to_string(),
        // `Shell` is `#[non_exhaustive]`; a future variant will surface here.
        _ => format!("# hyoui: completion: unsupported shell variant ({shell})"),
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
        COMPREPLY=( $(compgen -W "run attach list kill status tail wait screen input completion send detach --help -h --version -V" -- "$cur") )
        return 0
    fi

    # Detect `screen` sub-subcommand (= `screen dump` / `screen snapshot`).
    if [[ "$sub" == "screen" ]]; then
        local screen_sub=""
        for (( i=1; i < cword; i++ )); do
            local w="${words[i]}"
            if [[ "$w" == "screen" ]]; then
                # find first non-flag after `screen`
                local j
                for (( j=i+1; j < cword; j++ )); do
                    case "${words[j]}" in
                        -*) ;;
                        *) screen_sub="${words[j]}"; break ;;
                    esac
                done
                break
            fi
        done
        if [[ -z "$screen_sub" ]]; then
            COMPREPLY=( $(compgen -W "dump snapshot --help -h" -- "$cur") )
            return 0
        fi
        case "$screen_sub" in
            dump)
                case "$prev" in
                    --socket) _filedir 2>/dev/null || COMPREPLY=( $(compgen -f -- "$cur") ); return 0 ;;
                    --format) COMPREPLY=( $(compgen -W "ansi binary cbor" -- "$cur") ); return 0 ;;
                    --layer) COMPREPLY=( $(compgen -W "visible scrollback both" -- "$cur") ); return 0 ;;
                    --output) _filedir 2>/dev/null || COMPREPLY=( $(compgen -f -- "$cur") ); return 0 ;;
                    --rect|--timeout) return 0 ;;
                esac
                COMPREPLY=( $(compgen -W "--socket --format --layer --rect --output --timeout --help -h" -- "$cur") )
                return 0 ;;
            snapshot)
                case "$prev" in
                    --socket) _filedir 2>/dev/null || COMPREPLY=( $(compgen -f -- "$cur") ); return 0 ;;
                    --include) COMPREPLY=( $(compgen -W "screen cursor size mode title" -- "$cur") ); return 0 ;;
                    --output) _filedir 2>/dev/null || COMPREPLY=( $(compgen -f -- "$cur") ); return 0 ;;
                    --timeout) return 0 ;;
                esac
                COMPREPLY=( $(compgen -W "--socket --include --output --timeout --help -h" -- "$cur") )
                return 0 ;;
        esac
    fi

    # `input` の spec prefix を current word の状態に応じて補完。
    if [[ "$sub" == "input" ]]; then
        case "$prev" in
            --socket) _filedir 2>/dev/null || COMPREPLY=( $(compgen -f -- "$cur") ); return 0 ;;
            --timeout|--lock-token|--max-file-bytes) return 0 ;;
        esac
        # spec prefix の途中 (= "text:" / "key:" 等) に来たら value 部分は補完しない
        # (= 任意文字列 / regex / path)。ただし "file:" の場合は path 補完を提供。
        case "$cur" in
            file:*)
                local p="${cur#file:}"
                COMPREPLY=( $(compgen -f -- "$p") )
                # `file:` prefix を残す形で再構成
                local i
                for (( i=0; i < ${#COMPREPLY[@]}; i++ )); do
                    COMPREPLY[$i]="file:${COMPREPLY[$i]}"
                done
                return 0 ;;
            key:*)
                local k="${cur#key:}"
                COMPREPLY=( $(compgen -W "Enter Return Tab Esc Escape Backspace Delete Space Up Down Left Right Home End PageUp PageDown F1 F2 F3 F4 F5 F6 F7 F8 F9 F10 F11 F12" -- "$k") )
                local i
                for (( i=0; i < ${#COMPREPLY[@]}; i++ )); do
                    COMPREPLY[$i]="key:${COMPREPLY[$i]}"
                done
                return 0 ;;
        esac
        COMPREPLY=( $(compgen -W "--socket --timeout --lock-token --max-file-bytes --help -h text: hex: file: paste: key: wait: wait-idle:" -- "$cur") )
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
        attach)
            case "$prev" in
                --socket) _filedir 2>/dev/null || COMPREPLY=( $(compgen -f -- "$cur") ); return 0 ;;
                --mode) COMPREPLY=( $(compgen -W "rw ro rw-no-leader" -- "$cur") ); return 0 ;;
            esac
            COMPREPLY=( $(compgen -W "--socket --mode --exclusive --detach-others --help -h" -- "$cur") )
            return 0 ;;
        list)
            COMPREPLY=( $(compgen -W "--help -h" -- "$cur") )
            return 0 ;;
        kill)
            case "$prev" in
                --socket) _filedir 2>/dev/null || COMPREPLY=( $(compgen -f -- "$cur") ); return 0 ;;
                --signal) COMPREPLY=( $(compgen -W "SIGHUP SIGINT SIGQUIT SIGABRT SIGKILL SIGUSR1 SIGUSR2 SIGTERM SIGCONT SIGTSTP SIGCHLD" -- "$cur") ); return 0 ;;
            esac
            COMPREPLY=( $(compgen -W "--socket --signal --help -h" -- "$cur") )
            return 0 ;;
        status)
            case "$prev" in
                --socket) _filedir 2>/dev/null || COMPREPLY=( $(compgen -f -- "$cur") ); return 0 ;;
            esac
            COMPREPLY=( $(compgen -W "--socket --help -h" -- "$cur") )
            return 0 ;;
        tail)
            case "$prev" in
                --socket) _filedir 2>/dev/null || COMPREPLY=( $(compgen -f -- "$cur") ); return 0 ;;
                --since|--last-bytes) return 0 ;;
            esac
            COMPREPLY=( $(compgen -W "--socket --follow --strip-ansi --since --last-bytes --help -h" -- "$cur") )
            return 0 ;;
        wait)
            case "$prev" in
                --socket) _filedir 2>/dev/null || COMPREPLY=( $(compgen -f -- "$cur") ); return 0 ;;
                --timeout) return 0 ;;
            esac
            COMPREPLY=( $(compgen -W "--socket --timeout --no-strip-escapes --newline-convert-lf --help -h text: pattern: wait: wait-idle:" -- "$cur") )
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
                attach)
                    _arguments \
                        '--socket=[Explicit socket path]:socket:_files' \
                        '--mode=[Operating mode]:mode:(rw ro rw-no-leader)' \
                        '--exclusive[Demand exclusive ownership]' \
                        '--detach-others[Drop other clients on connect]' \
                        '(-h --help)'{-h,--help}'[Show help]' \
                        '*:session id:'
                    ;;
                list)
                    _arguments '(-h --help)'{-h,--help}'[Show help]'
                    ;;
                kill)
                    _arguments \
                        '--socket=[Explicit socket path]:socket:_files' \
                        '--signal=[Signal name (SIG-prefix uppercase, DR-0012)]:signal:(SIGHUP SIGINT SIGQUIT SIGABRT SIGKILL SIGUSR1 SIGUSR2 SIGTERM SIGCONT SIGTSTP SIGCHLD)' \
                        '(-h --help)'{-h,--help}'[Show help]' \
                        '*:session id:'
                    ;;
                status)
                    _arguments \
                        '--socket=[Explicit socket path]:socket:_files' \
                        '(-h --help)'{-h,--help}'[Show help]' \
                        '*:session id:'
                    ;;
                tail)
                    _arguments \
                        '--socket=[Explicit socket path]:socket:_files' \
                        '--follow[Continue streaming live output]' \
                        '--strip-ansi[Strip ANSI escapes in output]' \
                        '--since=[Drop chunks older than DUR (e.g. 500ms / 2s / 1m)]:duration:' \
                        '--last-bytes=[Trim to last N bytes]:bytes:' \
                        '(-h --help)'{-h,--help}'[Show help]' \
                        '*:session id:'
                    ;;
                wait)
                    _arguments \
                        '--socket=[Explicit socket path]:socket:_files' \
                        '--timeout=[Absolute timeout (e.g. 5s / 30s)]:duration:' \
                        '--no-strip-escapes[Do not strip ANSI escapes before matching]' \
                        '--newline-convert-lf[Convert CRLF to LF before matching]' \
                        '(-h --help)'{-h,--help}'[Show help]' \
                        '*:positional (predicate or session-id):'
                    ;;
                screen)
                    _hyoui_screen
                    ;;
                input)
                    _hyoui_input
                    ;;
            esac
            ;;
    esac
}

_hyoui_subcommands() {
    local -a subs
    subs=(
        'run:Run a command inside a PTY as a transparent proxy'
        'attach:Attach to a running session'
        'list:List daemon sessions'
        'kill:Send signal to a session and terminate it'
        'status:Print session status'
        'tail:Stream scrollback / live output'
        'wait:Wait until predicate matches'
        'screen:Dump / inspect virtual screen state (dump, snapshot)'
        'input:Send input via spec list (DR-0006 §8)'
        'completion:Print a shell completion script'
        'send:(reserved) Send input to a running session'
        'detach:(reserved) Detach helper'
    )
    _describe -t commands 'hyoui subcommand' subs
}

_hyoui_screen() {
    local context state state_descr line
    typeset -A opt_args
    _arguments -C \
        '1: :_hyoui_screen_subcommands' \
        '*::arg:->screen_args'
    case $state in
        screen_args)
            case $line[1] in
                dump)
                    _arguments \
                        '--socket=[Explicit socket path]:socket:_files' \
                        '--format=[Output format]:format:(ansi binary cbor)' \
                        '--layer=[Layer to dump]:layer:(visible scrollback both)' \
                        '--rect=[Sub-rectangle x,y,w,h]:rect:' \
                        '--output=[Output file path]:file:_files' \
                        '--timeout=[Response timeout]:duration:' \
                        '(-h --help)'{-h,--help}'[Show help]' \
                        '*:session id:'
                    ;;
                snapshot)
                    _arguments \
                        '--socket=[Explicit socket path]:socket:_files' \
                        '*--include=[Snapshot components to include]:component:(screen cursor size mode title)' \
                        '--output=[Output file path]:file:_files' \
                        '--timeout=[Response timeout]:duration:' \
                        '(-h --help)'{-h,--help}'[Show help]' \
                        '*:session id:'
                    ;;
            esac
            ;;
    esac
}

_hyoui_screen_subcommands() {
    local -a subs
    subs=(
        'dump:Dump current screen / scrollback as ANSI / binary / CBOR'
        'snapshot:Take a structured snapshot (screen + cursor + size + mode)'
    )
    _describe -t commands 'hyoui screen subcommand' subs
}

_hyoui_input() {
    # `input` の positional は <session> <spec>...。spec は order-preserved。
    # spec prefix を候補に出し、`file:` だけ path 補完を効かせる。
    _arguments \
        '--socket=[Explicit socket path]:socket:_files' \
        '--timeout=[Per-spec timeout (e.g. 5s)]:duration:' \
        '--lock-token=[Explicit lock token (overrides HYOUI_LOCK_TOKEN)]:token:' \
        '--max-file-bytes=[Max bytes for file: spec (0 = unlimited)]:bytes:' \
        '(-h --help)'{-h,--help}'[Show help]' \
        '*::spec:_hyoui_input_spec'
}

_hyoui_input_spec() {
    # current word が `file:...` なら path 補完、`key:...` なら key name 補完、
    # それ以外なら prefix 候補のみ。
    case $words[$CURRENT] in
        file:*)
            local p=${words[$CURRENT]#file:}
            _path_files -W / -g '*' && return 0
            ;;
        key:*)
            local k=${words[$CURRENT]#key:}
            local -a keys
            keys=(Enter Return Tab Esc Escape Backspace Delete Space Up Down Left Right Home End PageUp PageDown F1 F2 F3 F4 F5 F6 F7 F8 F9 F10 F11 F12)
            compadd -P 'key:' -- $keys
            return 0
            ;;
    esac
    local -a prefixes
    prefixes=(
        'text\::Direct UTF-8 text (no bracketed paste)'
        'hex\::Hex-encoded binary bytes'
        'file\::File content as bytes'
        'paste\::Bracketed paste wrap'
        'key\::Symbolic key name'
        'wait\::Wait until regex matches visible state'
        'wait-idle\::Wait until input idle for duration'
    )
    _describe -t specs 'input spec prefix' prefixes
}

_hyoui_run() {
    _arguments \
        '--mode=[Operating mode]:mode:(interactive headless)' \
        '--socket=[Unix socket path]:socket:_files' \
        '--timeout=[Overall timeout (e.g. 30s / 1m / 1h30m)]:duration:' \
        '--idle-timeout=[Output idle timeout (e.g. 500ms / 5s)]:duration:' \
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
            case run attach list kill status tail wait screen input completion send detach
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
            case run attach list kill status tail wait screen input completion send detach
                return 1
        end
    end
    return 0
end

# screen の子 subcommand 検出 (= `screen dump` / `screen snapshot`)。
function __hyoui_screen_using_sub
    set -l cmd (commandline -opc)
    set -e cmd[1]
    set -l seen_screen 0
    for arg in $cmd
        if test $seen_screen -eq 1
            switch $arg
                case dump snapshot
                    if test "$arg" = "$argv[1]"
                        return 0
                    end
                    return 1
            end
        end
        if test "$arg" = "screen"
            set seen_screen 1
        end
    end
    return 1
end

function __hyoui_screen_no_sub
    set -l cmd (commandline -opc)
    set -e cmd[1]
    set -l seen_screen 0
    for arg in $cmd
        if test $seen_screen -eq 1
            switch $arg
                case dump snapshot
                    return 1
            end
        end
        if test "$arg" = "screen"
            set seen_screen 1
        end
    end
    if test $seen_screen -eq 1
        return 0
    end
    return 1
end

# Top-level: subcommands.
complete -c hyoui -n __hyoui_no_subcommand -f -a run        -d 'Run a command inside a PTY as a transparent proxy'
complete -c hyoui -n __hyoui_no_subcommand -f -a attach     -d 'Attach to a running session'
complete -c hyoui -n __hyoui_no_subcommand -f -a list       -d 'List daemon sessions'
complete -c hyoui -n __hyoui_no_subcommand -f -a kill       -d 'Send signal to a session and terminate it'
complete -c hyoui -n __hyoui_no_subcommand -f -a status     -d 'Print session status'
complete -c hyoui -n __hyoui_no_subcommand -f -a tail       -d 'Stream scrollback / live output'
complete -c hyoui -n __hyoui_no_subcommand -f -a wait       -d 'Wait until predicate matches'
complete -c hyoui -n __hyoui_no_subcommand -f -a screen     -d 'Dump / inspect virtual screen state'
complete -c hyoui -n __hyoui_no_subcommand -f -a input      -d 'Send input via spec list (DR-0006 §8)'
complete -c hyoui -n __hyoui_no_subcommand -f -a completion -d 'Print a shell completion script'
complete -c hyoui -n __hyoui_no_subcommand -f -a send       -d '(reserved) Send input to a running session'
complete -c hyoui -n __hyoui_no_subcommand -f -a detach     -d '(reserved) Detach helper'

# Top-level global flags.
complete -c hyoui -n __hyoui_no_subcommand -s h -l help    -d 'Show help and exit'
complete -c hyoui -n __hyoui_no_subcommand -s V -l version -d 'Show version and exit'

# `hyoui run` options.
complete -c hyoui -n '__hyoui_using_subcommand run' -l mode              -x -a 'interactive headless' -d 'Operating mode'
complete -c hyoui -n '__hyoui_using_subcommand run' -l socket            -r -F                          -d 'Unix socket path'
complete -c hyoui -n '__hyoui_using_subcommand run' -l timeout           -x                              -d 'Overall timeout (e.g. 30s / 1m / 1h30m)'
complete -c hyoui -n '__hyoui_using_subcommand run' -l idle-timeout      -x                              -d 'Output idle timeout (e.g. 500ms / 5s)'
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

# `hyoui attach` options.
complete -c hyoui -n '__hyoui_using_subcommand attach' -l socket         -r -F                        -d 'Explicit socket path'
complete -c hyoui -n '__hyoui_using_subcommand attach' -l mode           -x -a 'rw ro rw-no-leader'   -d 'Operating mode'
complete -c hyoui -n '__hyoui_using_subcommand attach' -l exclusive                                    -d 'Demand exclusive ownership'
complete -c hyoui -n '__hyoui_using_subcommand attach' -l detach-others                                -d 'Drop other clients on connect'
complete -c hyoui -n '__hyoui_using_subcommand attach' -s h -l help                                    -d 'Show help and exit'

# `hyoui list` options.
complete -c hyoui -n '__hyoui_using_subcommand list' -s h -l help -d 'Show help and exit'

# `hyoui kill` options.
complete -c hyoui -n '__hyoui_using_subcommand kill' -l socket -r -F -d 'Explicit socket path'
complete -c hyoui -n '__hyoui_using_subcommand kill' -l signal -x -a 'SIGHUP SIGINT SIGQUIT SIGABRT SIGKILL SIGUSR1 SIGUSR2 SIGTERM SIGCONT SIGTSTP SIGCHLD' -d 'Signal name (SIG-prefix uppercase, DR-0012)'
complete -c hyoui -n '__hyoui_using_subcommand kill' -s h -l help    -d 'Show help and exit'

# `hyoui status` options.
complete -c hyoui -n '__hyoui_using_subcommand status' -l socket -r -F -d 'Explicit socket path'
complete -c hyoui -n '__hyoui_using_subcommand status' -s h -l help    -d 'Show help and exit'

# `hyoui tail` options.
complete -c hyoui -n '__hyoui_using_subcommand tail' -l socket          -r -F  -d 'Explicit socket path'
complete -c hyoui -n '__hyoui_using_subcommand tail' -l follow                  -d 'Continue streaming live output'
complete -c hyoui -n '__hyoui_using_subcommand tail' -l strip-ansi              -d 'Strip ANSI escapes in output'
complete -c hyoui -n '__hyoui_using_subcommand tail' -l since           -x      -d 'Drop chunks older than DUR (500ms / 2s / 1m)'
complete -c hyoui -n '__hyoui_using_subcommand tail' -l last-bytes      -x      -d 'Trim to last N bytes'
complete -c hyoui -n '__hyoui_using_subcommand tail' -s h -l help               -d 'Show help and exit'

# `hyoui wait` options.
complete -c hyoui -n '__hyoui_using_subcommand wait' -l socket            -r -F  -d 'Explicit socket path'
complete -c hyoui -n '__hyoui_using_subcommand wait' -l timeout           -x      -d 'Absolute timeout (5s / 30s)'
complete -c hyoui -n '__hyoui_using_subcommand wait' -l no-strip-escapes          -d 'Do not strip ANSI escapes before matching'
complete -c hyoui -n '__hyoui_using_subcommand wait' -l newline-convert-lf        -d 'Convert CRLF to LF before matching'
complete -c hyoui -n '__hyoui_using_subcommand wait' -s h -l help                 -d 'Show help and exit'

# `hyoui screen` 子 subcommand
complete -c hyoui -n __hyoui_screen_no_sub -f -a dump     -d 'Dump screen / scrollback as ANSI / binary / CBOR'
complete -c hyoui -n __hyoui_screen_no_sub -f -a snapshot -d 'Take a structured snapshot (screen + cursor + size + mode)'

# `hyoui screen dump` options
complete -c hyoui -n '__hyoui_screen_using_sub dump' -l socket  -r -F                          -d 'Explicit socket path'
complete -c hyoui -n '__hyoui_screen_using_sub dump' -l format  -x -a 'ansi binary cbor'       -d 'Output format'
complete -c hyoui -n '__hyoui_screen_using_sub dump' -l layer   -x -a 'visible scrollback both' -d 'Layer to dump'
complete -c hyoui -n '__hyoui_screen_using_sub dump' -l rect    -x                              -d 'Sub-rectangle x,y,w,h'
complete -c hyoui -n '__hyoui_screen_using_sub dump' -l output  -r -F                          -d 'Output file path'
complete -c hyoui -n '__hyoui_screen_using_sub dump' -l timeout -x                              -d 'Response timeout'
complete -c hyoui -n '__hyoui_screen_using_sub dump' -s h -l help                               -d 'Show help and exit'

# `hyoui screen snapshot` options
complete -c hyoui -n '__hyoui_screen_using_sub snapshot' -l socket  -r -F                                -d 'Explicit socket path'
complete -c hyoui -n '__hyoui_screen_using_sub snapshot' -l include -x -a 'screen cursor size mode title' -d 'Snapshot components to include'
complete -c hyoui -n '__hyoui_screen_using_sub snapshot' -l output  -r -F                                -d 'Output file path'
complete -c hyoui -n '__hyoui_screen_using_sub snapshot' -l timeout -x                                    -d 'Response timeout'
complete -c hyoui -n '__hyoui_screen_using_sub snapshot' -s h -l help                                     -d 'Show help and exit'

# `hyoui input` options + spec prefix
complete -c hyoui -n '__hyoui_using_subcommand input' -l socket          -r -F  -d 'Explicit socket path'
complete -c hyoui -n '__hyoui_using_subcommand input' -l timeout         -x      -d 'Per-spec timeout (e.g. 5s)'
complete -c hyoui -n '__hyoui_using_subcommand input' -l lock-token      -x      -d 'Explicit lock token (overrides HYOUI_LOCK_TOKEN)'
complete -c hyoui -n '__hyoui_using_subcommand input' -l max-file-bytes  -x      -d 'Max bytes for file: spec (0 = unlimited)'
complete -c hyoui -n '__hyoui_using_subcommand input' -s h -l help              -d 'Show help and exit'
complete -c hyoui -n '__hyoui_using_subcommand input' -f -a 'text\: hex\: file\: paste\: key\: wait\: wait-idle\:' -d 'Input spec prefix'
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
    fn completion_all_shells_mention_implemented_subcommands() {
        for sh in [Shell::Bash, Shell::Zsh, Shell::Fish] {
            let s = script(sh);
            for sub in [
                "run", "attach", "list", "kill", "status", "tail", "wait", "screen", "input",
            ] {
                assert!(s.contains(sub), "shell {sh:?} missing `{sub}`");
            }
        }
    }

    #[test]
    fn completion_all_shells_mention_screen_subsubcommands() {
        for sh in [Shell::Bash, Shell::Zsh, Shell::Fish] {
            let s = script(sh);
            assert!(s.contains("dump"), "shell {sh:?} missing `dump`");
            assert!(s.contains("snapshot"), "shell {sh:?} missing `snapshot`");
        }
    }

    #[test]
    fn completion_all_shells_mention_input_spec_prefixes() {
        for sh in [Shell::Bash, Shell::Zsh, Shell::Fish] {
            let s = script(sh);
            // bash/fish は `text:` 形式、zsh は `text\:` (= escape) 形式で出る。
            // どちらの形式でも prefix 文字列が含まれていればよい。
            for prefix in ["text", "hex", "file", "paste", "key", "wait-idle"] {
                assert!(
                    s.contains(prefix),
                    "shell {sh:?} missing spec prefix `{prefix}`"
                );
            }
        }
    }

    #[test]
    fn completion_all_shells_mention_reserved_subcommands() {
        for sh in [Shell::Bash, Shell::Zsh, Shell::Fish] {
            let s = script(sh);
            assert!(s.contains("send"), "shell {sh:?} missing reserved `send`");
            assert!(
                s.contains("detach"),
                "shell {sh:?} missing reserved `detach`"
            );
        }
    }
}
