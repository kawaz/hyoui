//! Shell completion script generation.
//!
//! Each function returns a self-contained completion script for the
//! corresponding shell. The scripts are intentionally hand-written rather
//! than generated, so the supported subcommand/option surface evolves in
//! lock-step with [`hyoui::cli`].
//!
//! The subcommand / option / enum-value surface is kept in sync with the
//! parser via the single-source-of-truth constants in [`hyoui::cli`]
//! (`IMPLEMENTED_TOP_LEVEL_SUBCOMMANDS`, `RESERVED_TOP_LEVEL_SUBCOMMANDS`,
//! `SCREEN_SUBCOMMANDS`, `LOCK_SUBCOMMANDS`, `RECORD_SUBCOMMANDS`,
//! `SNAPSHOT_INCLUDE_VALUES`, ...). The completion tests assert that every
//! element of those constants appears in all three shell scripts and that no
//! reserved subcommand (`send` / `tx`) leaks into the candidates.
//!
//! Reserved-but-unimplemented subcommands are intentionally **not** offered:
//! `parse_args` returns a "reserved but not yet implemented" error for them, so
//! completing them would mislead users.

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
        # Top-level: implemented subcommands + global flags.
        # (reserved subcommands send/tx are intentionally omitted.)
        COMPREPLY=( $(compgen -W "run attach list kill status set tail wait screen input lock unlock detach record upgrade config completion --help -h --version -V" -- "$cur") )
        return 0
    fi

    # Helper: find first non-flag word after a given subcommand name.
    _hyoui_child_of() {
        local parent="$1"
        local k m child=""
        for (( k=1; k < cword; k++ )); do
            if [[ "${words[k]}" == "$parent" ]]; then
                for (( m=k+1; m < cword; m++ )); do
                    case "${words[m]}" in
                        -*) ;;
                        *) child="${words[m]}"; break ;;
                    esac
                done
                break
            fi
        done
        printf '%s' "$child"
    }

    # Detect `screen` sub-subcommand (= `screen dump` / `screen snapshot`).
    if [[ "$sub" == "screen" ]]; then
        local screen_sub
        screen_sub="$(_hyoui_child_of screen)"
        if [[ -z "$screen_sub" ]]; then
            COMPREPLY=( $(compgen -W "dump snapshot --help -h" -- "$cur") )
            return 0
        fi
        case "$screen_sub" in
            dump)
                case "$prev" in
                    --socket) _filedir 2>/dev/null || COMPREPLY=( $(compgen -f -- "$cur") ); return 0 ;;
                    --format) COMPREPLY=( $(compgen -W "ansi binary cbor text/plain" -- "$cur") ); return 0 ;;
                    --layer) COMPREPLY=( $(compgen -W "visible scrollback both" -- "$cur") ); return 0 ;;
                    --output) _filedir 2>/dev/null || COMPREPLY=( $(compgen -f -- "$cur") ); return 0 ;;
                    --namespace|--index|--rect|--timeout) return 0 ;;
                esac
                COMPREPLY=( $(compgen -W "--socket --namespace --index --format --layer --rect --output --timeout --help -h" -- "$cur") )
                return 0 ;;
            snapshot)
                case "$prev" in
                    --socket) _filedir 2>/dev/null || COMPREPLY=( $(compgen -f -- "$cur") ); return 0 ;;
                    --include) COMPREPLY=( $(compgen -W "cells cursor mode style scrollback windowsize buffer sequenceno" -- "$cur") ); return 0 ;;
                    --format) COMPREPLY=( $(compgen -W "cbor json" -- "$cur") ); return 0 ;;
                    --output) _filedir 2>/dev/null || COMPREPLY=( $(compgen -f -- "$cur") ); return 0 ;;
                    --namespace|--index|--timeout) return 0 ;;
                esac
                COMPREPLY=( $(compgen -W "--socket --namespace --index --include --format --output --timeout --help -h" -- "$cur") )
                return 0 ;;
        esac
        return 0
    fi

    # Detect `lock` sub-subcommand (= `lock acquire` / `lock release`).
    if [[ "$sub" == "lock" ]]; then
        local lock_sub
        lock_sub="$(_hyoui_child_of lock)"
        if [[ -z "$lock_sub" ]]; then
            COMPREPLY=( $(compgen -W "acquire release --help -h" -- "$cur") )
            return 0
        fi
        case "$lock_sub" in
            acquire)
                case "$prev" in
                    --socket) _filedir 2>/dev/null || COMPREPLY=( $(compgen -f -- "$cur") ); return 0 ;;
                    --mode) COMPREPLY=( $(compgen -W "wait fail" -- "$cur") ); return 0 ;;
                    --namespace|--index|--timeout) return 0 ;;
                esac
                COMPREPLY=( $(compgen -W "--socket --namespace --index --mode --timeout --help -h" -- "$cur") )
                return 0 ;;
            release)
                case "$prev" in
                    --socket) _filedir 2>/dev/null || COMPREPLY=( $(compgen -f -- "$cur") ); return 0 ;;
                    --namespace|--index|--token) return 0 ;;
                esac
                COMPREPLY=( $(compgen -W "--socket --namespace --index --token --help -h" -- "$cur") )
                return 0 ;;
        esac
        return 0
    fi

    # Detect `record` sub-subcommand (= `record start` / `record stop` / `record list`).
    if [[ "$sub" == "record" ]]; then
        local record_sub
        record_sub="$(_hyoui_child_of record)"
        if [[ -z "$record_sub" ]]; then
            COMPREPLY=( $(compgen -W "start stop list --help -h" -- "$cur") )
            return 0
        fi
        case "$record_sub" in
            start)
                case "$prev" in
                    --socket|--output) _filedir 2>/dev/null || COMPREPLY=( $(compgen -f -- "$cur") ); return 0 ;;
                    --format) COMPREPLY=( $(compgen -W "jsonl raw" -- "$cur") ); return 0 ;;
                    --input-secrecy) COMPREPLY=( $(compgen -W "record-all never-record-stdin" -- "$cur") ); return 0 ;;
                    --namespace|--index|--max-bytes|--max-duration|--prompt-pattern) return 0 ;;
                esac
                COMPREPLY=( $(compgen -W "--socket --namespace --index --output --stdin --stdout --both --format --max-bytes --max-duration --input-secrecy --prompt-pattern --help -h" -- "$cur") )
                return 0 ;;
            stop)
                case "$prev" in
                    --socket) _filedir 2>/dev/null || COMPREPLY=( $(compgen -f -- "$cur") ); return 0 ;;
                    --namespace|--index|--id) return 0 ;;
                esac
                COMPREPLY=( $(compgen -W "--socket --namespace --index --id --all --help -h" -- "$cur") )
                return 0 ;;
            list)
                case "$prev" in
                    --socket) _filedir 2>/dev/null || COMPREPLY=( $(compgen -f -- "$cur") ); return 0 ;;
                    --format) COMPREPLY=( $(compgen -W "table jsonl" -- "$cur") ); return 0 ;;
                    --namespace|--index) return 0 ;;
                esac
                COMPREPLY=( $(compgen -W "--socket --namespace --index --format --help -h" -- "$cur") )
                return 0 ;;
        esac
        return 0
    fi

    # Detect `config` sub-subcommand (= `config path` / `config show`).
    if [[ "$sub" == "config" ]]; then
        local config_sub
        config_sub="$(_hyoui_child_of config)"
        if [[ -z "$config_sub" ]]; then
            COMPREPLY=( $(compgen -W "path show --help -h" -- "$cur") )
            return 0
        fi
        COMPREPLY=( $(compgen -W "--help -h" -- "$cur") )
        return 0
    fi

    # `input` の spec prefix を current word の状態に応じて補完。
    if [[ "$sub" == "input" ]]; then
        case "$prev" in
            --socket) _filedir 2>/dev/null || COMPREPLY=( $(compgen -f -- "$cur") ); return 0 ;;
            --namespace|--index|--timeout|--lock-token|--max-file-bytes) return 0 ;;
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
        COMPREPLY=( $(compgen -W "--socket --namespace --index --timeout --lock-token --max-file-bytes --help -h text: hex: file: paste: key: wait: wait-idle:" -- "$cur") )
        return 0
    fi

    case "$sub" in
        run)
            case "$prev" in
                --on-child-suspend)
                    COMPREPLY=( $(compgen -W "notify auto-resume" -- "$cur") ); return 0 ;;
                --stdin-eof)
                    COMPREPLY=( $(compgen -W "detach send-eof" -- "$cur") ); return 0 ;;
                --socket)
                    _filedir 2>/dev/null || COMPREPLY=( $(compgen -f -- "$cur") ); return 0 ;;
                --namespace|--timeout|--idle-timeout|--until|--size|--cols|--rows|--scrollback-rows)
                    return 0 ;;
            esac
            case "$cur" in
                --on-child-suspend=*)
                    COMPREPLY=( $(compgen -W "notify auto-resume" -- "${cur#*=}") ); return 0 ;;
                --stdin-eof=*)
                    COMPREPLY=( $(compgen -W "detach send-eof" -- "${cur#*=}") ); return 0 ;;
            esac
            COMPREPLY=( $(compgen -W "--socket --namespace --timeout --idle-timeout --until --on-child-suspend --stdin-eof --scrollback-rows --size --cols --rows --help -h --" -- "$cur") )
            return 0 ;;
        completion)
            COMPREPLY=( $(compgen -W "bash zsh fish --help -h" -- "$cur") )
            return 0 ;;
        attach)
            case "$prev" in
                --socket) _filedir 2>/dev/null || COMPREPLY=( $(compgen -f -- "$cur") ); return 0 ;;
                --mode) COMPREPLY=( $(compgen -W "rw ro rw-no-leader" -- "$cur") ); return 0 ;;
                --stdin-eof) COMPREPLY=( $(compgen -W "detach send-eof" -- "$cur") ); return 0 ;;
                --namespace|--index) return 0 ;;
            esac
            case "$cur" in
                --stdin-eof=*)
                    COMPREPLY=( $(compgen -W "detach send-eof" -- "${cur#*=}") ); return 0 ;;
            esac
            COMPREPLY=( $(compgen -W "--socket --namespace --index --mode --stdin-eof --exclusive --detach-others --quiet --help -h" -- "$cur") )
            return 0 ;;
        list)
            case "$cur" in
                --format=*)
                    COMPREPLY=( $(compgen -W "plain jsonl" -- "${cur#*=}") ); return 0 ;;
            esac
            COMPREPLY=( $(compgen -W "--namespace --all-namespaces --prune-stale --format --help -h" -- "$cur") )
            return 0 ;;
        kill)
            case "$prev" in
                --socket) _filedir 2>/dev/null || COMPREPLY=( $(compgen -f -- "$cur") ); return 0 ;;
                --signal) COMPREPLY=( $(compgen -W "SIGHUP SIGINT SIGQUIT SIGABRT SIGKILL SIGUSR1 SIGUSR2 SIGTERM SIGCONT SIGTSTP SIGCHLD" -- "$cur") ); return 0 ;;
                --namespace|--index) return 0 ;;
            esac
            COMPREPLY=( $(compgen -W "--socket --namespace --index --all --signal --wait --kill-on-timeout --no-terminate --help -h" -- "$cur") )
            return 0 ;;
        status)
            case "$prev" in
                --socket) _filedir 2>/dev/null || COMPREPLY=( $(compgen -f -- "$cur") ); return 0 ;;
                --format) COMPREPLY=( $(compgen -W "plain json" -- "$cur") ); return 0 ;;
                --namespace|--index) return 0 ;;
            esac
            COMPREPLY=( $(compgen -W "--socket --namespace --index --format --help -h" -- "$cur") )
            return 0 ;;
        set)
            case "$prev" in
                --socket) _filedir 2>/dev/null || COMPREPLY=( $(compgen -f -- "$cur") ); return 0 ;;
                --namespace|--index) return 0 ;;
            esac
            # key=value 位置引数の補完: 既知 key を `key=` 形まで、値も候補に出す。
            case "$cur" in
                on-child-suspend=*) COMPREPLY=( $(compgen -W "on-child-suspend=notify on-child-suspend=auto-resume" -- "$cur") ); return 0 ;;
            esac
            COMPREPLY=( $(compgen -W "--socket --namespace --index --help -h on-child-suspend=" -- "$cur") )
            return 0 ;;
        tail)
            case "$prev" in
                --socket) _filedir 2>/dev/null || COMPREPLY=( $(compgen -f -- "$cur") ); return 0 ;;
                --namespace|--index|--since|--last-bytes) return 0 ;;
            esac
            COMPREPLY=( $(compgen -W "--socket --namespace --index --follow --strip-ansi --since --since-strict --last-bytes --help -h" -- "$cur") )
            return 0 ;;
        wait)
            case "$prev" in
                --socket) _filedir 2>/dev/null || COMPREPLY=( $(compgen -f -- "$cur") ); return 0 ;;
                --namespace|--index|--timeout|--poll-interval) return 0 ;;
            esac
            COMPREPLY=( $(compgen -W "--socket --namespace --index --timeout --poll-interval --help -h" -- "$cur") )
            return 0 ;;
        unlock)
            case "$prev" in
                --socket) _filedir 2>/dev/null || COMPREPLY=( $(compgen -f -- "$cur") ); return 0 ;;
                --namespace|--index|--token) return 0 ;;
            esac
            COMPREPLY=( $(compgen -W "--socket --namespace --index --token --help -h" -- "$cur") )
            return 0 ;;
        detach)
            case "$prev" in
                --socket) _filedir 2>/dev/null || COMPREPLY=( $(compgen -f -- "$cur") ); return 0 ;;
                --namespace|--index) return 0 ;;
            esac
            COMPREPLY=( $(compgen -W "--socket --namespace --index --help -h" -- "$cur") )
            return 0 ;;
        upgrade)
            case "$prev" in
                --socket|--binary) _filedir 2>/dev/null || COMPREPLY=( $(compgen -f -- "$cur") ); return 0 ;;
                --namespace|--index) return 0 ;;
            esac
            COMPREPLY=( $(compgen -W "--socket --namespace --index --binary --skip-version-check --help -h" -- "$cur") )
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
                        '--index=[Session selector (1=oldest, -1=newest)]:index:' \
                        '--namespace=[Session namespace (flag > env HYOUI_NAMESPACE > default)]:namespace:' \
                        '--mode=[Operating mode]:mode:(rw ro rw-no-leader)' \
                        '--stdin-eof=[stdin EOF action]:action:(detach send-eof)' \
                        '--exclusive[Deny attach if another rw client is present]' \
                        '--detach-others[Detach other clients on attach (steal)]' \
                        '--quiet[Suppress the detach/peek hint on attach]' \
                        '(-h --help)'{-h,--help}'[Show help]' \
                        '*:session id:'
                    ;;
                list)
                    _arguments \
                        '--namespace=[Show only this namespace]:namespace:' \
                        '--all-namespaces[List sessions across all namespaces (adds NS column)]' \
                        '--prune-stale[Unlink stale sockets]' \
                        '--format=[Output format]:format:(plain jsonl)' \
                        '(-h --help)'{-h,--help}'[Show help]'
                    ;;
                kill)
                    _arguments \
                        '--socket=[Explicit socket path]:socket:_files' \
                        '--index=[Session selector (1=oldest, -1=newest)]:index:' \
                        '--namespace=[Session namespace (flag > env HYOUI_NAMESPACE > default)]:namespace:' \
                        '--all[Kill all live sessions]' \
                        '--signal=[Signal name (SIG-prefix uppercase, DR-0012)]:signal:(SIGHUP SIGINT SIGQUIT SIGABRT SIGKILL SIGUSR1 SIGUSR2 SIGTERM SIGCONT SIGTSTP SIGCHLD)' \
                        '--wait=[Wait for child exit (bare=10s default, =DUR to override)]:duration:' \
                        '--kill-on-timeout[Escalate to SIGKILL when --wait times out]' \
                        '--no-terminate[Send signal only, keep session alive]' \
                        '(-h --help)'{-h,--help}'[Show help]' \
                        '*:session id:'
                    ;;
                status)
                    _arguments \
                        '--socket=[Explicit socket path]:socket:_files' \
                        '--index=[Session selector (1=oldest, -1=newest)]:index:' \
                        '--namespace=[Session namespace (flag > env HYOUI_NAMESPACE > default)]:namespace:' \
                        '--format=[Output format]:format:(plain json)' \
                        '(-h --help)'{-h,--help}'[Show help]' \
                        '*:session id:'
                    ;;
                set)
                    _arguments \
                        '--socket=[Explicit socket path]:socket:_files' \
                        '--index=[Session selector (1=oldest, -1=newest)]:index:' \
                        '--namespace=[Session namespace (flag > env HYOUI_NAMESPACE > default)]:namespace:' \
                        '(-h --help)'{-h,--help}'[Show help]' \
                        '*:session id or key=value:(on-child-suspend=notify on-child-suspend=auto-resume)'
                    ;;
                tail)
                    _arguments \
                        '--socket=[Explicit socket path]:socket:_files' \
                        '--index=[Session selector (1=oldest, -1=newest)]:index:' \
                        '--namespace=[Session namespace (flag > env HYOUI_NAMESPACE > default)]:namespace:' \
                        '--follow[Continue streaming live output]' \
                        '--strip-ansi[Strip ANSI escapes in output]' \
                        '--since=[Drop chunks older than DUR (e.g. 500ms / 2s / 1m)]:duration:' \
                        '--since-strict[Exit non-zero if --since range was evicted]' \
                        '--last-bytes=[Trim to last N bytes]:bytes:' \
                        '(-h --help)'{-h,--help}'[Show help]' \
                        '*:session id:'
                    ;;
                wait)
                    _arguments \
                        '--socket=[Explicit socket path]:socket:_files' \
                        '--index=[Session selector (1=oldest, -1=newest)]:index:' \
                        '--namespace=[Session namespace (flag > env HYOUI_NAMESPACE > default)]:namespace:' \
                        '--timeout=[Absolute timeout (e.g. 5s / 30s)]:duration:' \
                        '--poll-interval=[Snapshot polling interval (default 100ms)]:duration:' \
                        '(-h --help)'{-h,--help}'[Show help]' \
                        '*:positional (session-id then regex pattern):'
                    ;;
                screen)
                    _hyoui_screen
                    ;;
                input)
                    _hyoui_input
                    ;;
                lock)
                    _hyoui_lock
                    ;;
                unlock)
                    _arguments \
                        '--socket=[Explicit socket path]:socket:_files' \
                        '--index=[Session selector (1=oldest, -1=newest)]:index:' \
                        '--namespace=[Session namespace (flag > env HYOUI_NAMESPACE > default)]:namespace:' \
                        '--token=[Lock token to release]:token:' \
                        '(-h --help)'{-h,--help}'[Show help]' \
                        '*:session id:'
                    ;;
                detach)
                    _arguments \
                        '--socket=[Explicit socket path]:socket:_files' \
                        '--index=[Session selector (1=oldest, -1=newest)]:index:' \
                        '--namespace=[Session namespace (flag > env HYOUI_NAMESPACE > default)]:namespace:' \
                        '(-h --help)'{-h,--help}'[Show help]' \
                        '*:session id:'
                    ;;
                upgrade)
                    _arguments \
                        '--socket=[Explicit socket path]:socket:_files' \
                        '--index=[Session selector (1=oldest, -1=newest)]:index:' \
                        '--namespace=[Session namespace (flag > env HYOUI_NAMESPACE > default)]:namespace:' \
                        '--binary=[Override the daemon exec target path]:binary:_files' \
                        '--skip-version-check[Skip <binary> --version pre-check (test only)]' \
                        '(-h --help)'{-h,--help}'[Show help]' \
                        '*:session id:'
                    ;;
                record)
                    _hyoui_record
                    ;;
                config)
                    _hyoui_config
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
        'set:Change a runtime setting (set <session> <key>=<value>)'
        'tail:Stream scrollback / live output'
        'wait:Wait until predicate matches'
        'screen:Dump / inspect virtual screen state (dump, snapshot)'
        'input:Send input via spec list (DR-0006 §8)'
        'lock:Acquire / release a session lock (acquire, release)'
        'unlock:Release a session lock (= lock release alias)'
        'detach:Detach all attached clients from a session'
        'record:Record tty I/O timeline (start, stop, list)'
        'upgrade:Trigger daemon graceful self-exec upgrade (DR-0028)'
        'config:Inspect the user config file (path, show)'
        'completion:Print a shell completion script'
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
                        '--index=[Session selector (1=oldest, -1=newest)]:index:' \
                        '--namespace=[Session namespace (flag > env HYOUI_NAMESPACE > default)]:namespace:' \
                        '--format=[Output format]:format:(ansi binary cbor text/plain)' \
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
                        '--index=[Session selector (1=oldest, -1=newest)]:index:' \
                        '--namespace=[Session namespace (flag > env HYOUI_NAMESPACE > default)]:namespace:' \
                        '--include=[Snapshot components to include]:component:(cells cursor mode style scrollback windowsize buffer sequenceno)' \
                        '--format=[Output format]:format:(cbor json)' \
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
        'snapshot:Take a structured snapshot (cells + cursor + mode + ...)'
    )
    _describe -t commands 'hyoui screen subcommand' subs
}

_hyoui_lock() {
    local context state state_descr line
    typeset -A opt_args
    _arguments -C \
        '1: :_hyoui_lock_subcommands' \
        '*::arg:->lock_args'
    case $state in
        lock_args)
            case $line[1] in
                acquire)
                    _arguments \
                        '--socket=[Explicit socket path]:socket:_files' \
                        '--index=[Session selector (1=oldest, -1=newest)]:index:' \
                        '--namespace=[Session namespace (flag > env HYOUI_NAMESPACE > default)]:namespace:' \
                        '--mode=[Behavior when held]:mode:(wait fail)' \
                        '--timeout=[Acquire timeout]:duration:' \
                        '(-h --help)'{-h,--help}'[Show help]' \
                        '*:session id:'
                    ;;
                release)
                    _arguments \
                        '--socket=[Explicit socket path]:socket:_files' \
                        '--index=[Session selector (1=oldest, -1=newest)]:index:' \
                        '--namespace=[Session namespace (flag > env HYOUI_NAMESPACE > default)]:namespace:' \
                        '--token=[Lock token to release]:token:' \
                        '(-h --help)'{-h,--help}'[Show help]' \
                        '*:session id:'
                    ;;
            esac
            ;;
    esac
}

_hyoui_lock_subcommands() {
    local -a subs
    subs=(
        'acquire:Acquire a lock, print token to stdout, hold connection'
        'release:Release a lock by token'
    )
    _describe -t commands 'hyoui lock subcommand' subs
}

_hyoui_record() {
    local context state state_descr line
    typeset -A opt_args
    _arguments -C \
        '1: :_hyoui_record_subcommands' \
        '*::arg:->record_args'
    case $state in
        record_args)
            case $line[1] in
                start)
                    _arguments \
                        '--socket=[Explicit socket path]:socket:_files' \
                        '--index=[Session selector (1=oldest, -1=newest)]:index:' \
                        '--namespace=[Session namespace (flag > env HYOUI_NAMESPACE > default)]:namespace:' \
                        '--output=[Output file path (absolute)]:file:_files' \
                        '--stdin[Record child PTY input only]' \
                        '--stdout[Record child PTY output only]' \
                        '--both[Record both directions (default, jsonl only)]' \
                        '--format=[Output format]:format:(jsonl raw)' \
                        '--max-bytes=[Recording byte cap (0 disables)]:bytes:' \
                        '--max-duration=[Recording duration cap (0 disables)]:duration:' \
                        '--input-secrecy=[stdin redaction policy (default record-all; redact-after-prompt reserved for Phase 5)]:policy:(record-all never-record-stdin)' \
                        '--prompt-pattern=[Custom prompt detection regex]:regex:' \
                        '(-h --help)'{-h,--help}'[Show help]' \
                        '*:session id:'
                    ;;
                stop)
                    _arguments \
                        '--socket=[Explicit socket path]:socket:_files' \
                        '--index=[Session selector (1=oldest, -1=newest)]:index:' \
                        '--namespace=[Session namespace (flag > env HYOUI_NAMESPACE > default)]:namespace:' \
                        '--id=[record_id to stop]:id:' \
                        '--all[Stop all active records]' \
                        '(-h --help)'{-h,--help}'[Show help]' \
                        '*:session id:'
                    ;;
                list)
                    _arguments \
                        '--socket=[Explicit socket path]:socket:_files' \
                        '--index=[Session selector (1=oldest, -1=newest)]:index:' \
                        '--namespace=[Session namespace (flag > env HYOUI_NAMESPACE > default)]:namespace:' \
                        '--format=[Output format]:format:(table jsonl)' \
                        '(-h --help)'{-h,--help}'[Show help]' \
                        '*:session id:'
                    ;;
            esac
            ;;
    esac
}

_hyoui_record_subcommands() {
    local -a subs
    subs=(
        'start:Start a new record (writes to a file, returns record_id)'
        'stop:Stop a running record (--id <N> or --all)'
        'list:List active records for the session'
    )
    _describe -t commands 'hyoui record subcommand' subs
}

_hyoui_config() {
    local context state state_descr line
    typeset -A opt_args
    _arguments -C \
        '1: :_hyoui_config_subcommands' \
        '*::arg:->config_args'
    case $state in
        config_args)
            _arguments '(-h --help)'{-h,--help}'[Show help]'
            ;;
    esac
}

_hyoui_config_subcommands() {
    local -a subs
    subs=(
        'path:Print the resolved config file path'
        'show:Print the effective configuration as TOML'
    )
    _describe -t commands 'hyoui config subcommand' subs
}

_hyoui_input() {
    # `input` の positional は <session> <spec>...。spec は order-preserved。
    # spec prefix を候補に出し、`file:` だけ path 補完を効かせる。
    _arguments \
        '--socket=[Explicit socket path]:socket:_files' \
        '--index=[Session selector (1=oldest, -1=newest)]:index:' \
                        '--namespace=[Session namespace (flag > env HYOUI_NAMESPACE > default)]:namespace:' \
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
        '--socket=[Unix socket path]:socket:_files' \
        '--namespace=[Session namespace (flag > env HYOUI_NAMESPACE > default)]:namespace:' \
        '--timeout=[Overall timeout (e.g. 30s / 1m / 1h30m)]:duration:' \
        '--idle-timeout=[Output idle timeout (e.g. 500ms / 5s)]:duration:' \
        '--until=[Terminate when PATTERN appears in output]:pattern:' \
        '--size=[Virtual screen size COLSxROWS]:size:' \
        '--cols=[Virtual screen columns]:cols:' \
        '--rows=[Virtual screen rows]:rows:' \
        '--on-child-suspend=[Action when child is stopped]:action:(notify auto-resume)' \
        '--stdin-eof=[stdin EOF action]:action:(detach send-eof)' \
        '--scrollback-rows=[vt100 scrollback ring max rows (default 1000)]:rows:' \
        '(-h --help)'{-h,--help}'[Show help]' \
        '*::child command:_normal'
}

_hyoui "$@"
"#
}

fn fish() -> &'static str {
    r#"# fish completion for hyoui

# Detect whether a known (implemented) subcommand has already been provided.
# reserved subcommands send/tx are intentionally not listed.
function __hyoui_using_subcommand
    set -l cmd (commandline -opc)
    set -e cmd[1]
    for arg in $cmd
        switch $arg
            case run attach list kill status set tail wait screen input lock unlock detach record upgrade config completion
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
            case run attach list kill status set tail wait screen input lock unlock detach record upgrade config completion
                return 1
        end
    end
    return 0
end

# Generic helper: is the parent's child subcommand equal to $argv[2]?
# $argv[1] = parent name, $argv[2] = candidate child name.
function __hyoui_child_using
    set -l cmd (commandline -opc)
    set -e cmd[1]
    set -l seen 0
    for arg in $cmd
        if test $seen -eq 1
            switch $arg
                case '-*'
                    # skip flags between parent and child
                case '*'
                    if test "$arg" = "$argv[2]"
                        return 0
                    end
                    return 1
            end
        end
        if test "$arg" = "$argv[1]"
            set seen 1
        end
    end
    return 1
end

# Has the parent ($argv[1]) been given but no child yet?
function __hyoui_child_none
    set -l cmd (commandline -opc)
    set -e cmd[1]
    set -l seen 0
    for arg in $cmd
        if test $seen -eq 1
            switch $arg
                case '-*'
                case '*'
                    return 1
            end
        end
        if test "$arg" = "$argv[1]"
            set seen 1
        end
    end
    if test $seen -eq 1
        return 0
    end
    return 1
end

# screen 子 subcommand 検出 (= `screen dump` / `screen snapshot`)。
function __hyoui_screen_using_sub
    __hyoui_child_using screen $argv[1]
end

function __hyoui_screen_no_sub
    __hyoui_child_none screen
end

# Top-level: implemented subcommands (reserved send/tx omitted).
complete -c hyoui -n __hyoui_no_subcommand -f -a run        -d 'Run a command inside a PTY as a transparent proxy'
complete -c hyoui -n __hyoui_no_subcommand -f -a attach     -d 'Attach to a running session'
complete -c hyoui -n __hyoui_no_subcommand -f -a list       -d 'List daemon sessions'
complete -c hyoui -n __hyoui_no_subcommand -f -a kill       -d 'Send signal to a session and terminate it'
complete -c hyoui -n __hyoui_no_subcommand -f -a status     -d 'Print session status'
complete -c hyoui -n __hyoui_no_subcommand -f -a set        -d 'Change a runtime setting (set <session> <key>=<value>)'
complete -c hyoui -n __hyoui_no_subcommand -f -a tail       -d 'Stream scrollback / live output'
complete -c hyoui -n __hyoui_no_subcommand -f -a wait       -d 'Wait until predicate matches'
complete -c hyoui -n __hyoui_no_subcommand -f -a screen     -d 'Dump / inspect virtual screen state'
complete -c hyoui -n __hyoui_no_subcommand -f -a input      -d 'Send input via spec list (DR-0006 §8)'
complete -c hyoui -n __hyoui_no_subcommand -f -a lock       -d 'Acquire / release a session lock'
complete -c hyoui -n __hyoui_no_subcommand -f -a unlock     -d 'Release a session lock (= lock release alias)'
complete -c hyoui -n __hyoui_no_subcommand -f -a detach     -d 'Detach all attached clients from a session'
complete -c hyoui -n __hyoui_no_subcommand -f -a record     -d 'Record tty I/O timeline'
complete -c hyoui -n __hyoui_no_subcommand -f -a config     -d 'Inspect the user config file (path, show)'
complete -c hyoui -n __hyoui_no_subcommand -f -a completion -d 'Print a shell completion script'

# Top-level global flags.
complete -c hyoui -n __hyoui_no_subcommand -s h -l help    -d 'Show help and exit'
complete -c hyoui -n __hyoui_no_subcommand -s V -l version -d 'Show version and exit'

# `hyoui run` options.
complete -c hyoui -n '__hyoui_using_subcommand run' -l socket            -r -F                          -d 'Unix socket path'
complete -c hyoui -n '__hyoui_using_subcommand run' -l namespace         -x                              -d 'Session namespace (flag > env HYOUI_NAMESPACE > default)'
complete -c hyoui -n '__hyoui_using_subcommand run' -l timeout           -x                              -d 'Overall timeout (e.g. 30s / 1m / 1h30m)'
complete -c hyoui -n '__hyoui_using_subcommand run' -l idle-timeout      -x                              -d 'Output idle timeout (e.g. 500ms / 5s)'
complete -c hyoui -n '__hyoui_using_subcommand run' -l until             -x                              -d 'Terminate when PATTERN appears'
complete -c hyoui -n '__hyoui_using_subcommand run' -l size              -x                              -d 'Virtual screen size COLSxROWS'
complete -c hyoui -n '__hyoui_using_subcommand run' -l cols              -x                              -d 'Virtual screen columns'
complete -c hyoui -n '__hyoui_using_subcommand run' -l rows              -x                              -d 'Virtual screen rows'
complete -c hyoui -n '__hyoui_using_subcommand run' -l on-child-suspend  -x -a 'notify auto-resume'       -d 'Action when child is stopped'
complete -c hyoui -n '__hyoui_using_subcommand run' -l stdin-eof         -x -a 'detach send-eof'          -d 'stdin EOF action'
complete -c hyoui -n '__hyoui_using_subcommand run' -l scrollback-rows   -x                              -d 'vt100 scrollback ring max rows (default 1000)'
complete -c hyoui -n '__hyoui_using_subcommand run' -s h -l help                                          -d 'Show help and exit'

# `hyoui completion` options.
complete -c hyoui -n '__hyoui_using_subcommand completion' -f -a 'bash zsh fish' -d 'Target shell'
complete -c hyoui -n '__hyoui_using_subcommand completion' -s h -l help          -d 'Show help and exit'

# `hyoui attach` options.
complete -c hyoui -n '__hyoui_using_subcommand attach' -l socket         -r -F                        -d 'Explicit socket path'
complete -c hyoui -n '__hyoui_using_subcommand attach' -l index          -x                           -d 'Session selector (1=oldest, -1=newest)'
complete -c hyoui -n '__hyoui_using_subcommand attach' -l namespace -x -d 'Session namespace (flag > env HYOUI_NAMESPACE > default)'
complete -c hyoui -n '__hyoui_using_subcommand attach' -l mode           -x -a 'rw ro rw-no-leader'   -d 'Operating mode'
complete -c hyoui -n '__hyoui_using_subcommand attach' -l stdin-eof      -x -a 'detach send-eof'        -d 'stdin EOF action'
complete -c hyoui -n '__hyoui_using_subcommand attach' -l exclusive                                     -d 'Deny attach if another rw client is present'
complete -c hyoui -n '__hyoui_using_subcommand attach' -l detach-others                                 -d 'Detach other clients on attach (steal)'
complete -c hyoui -n '__hyoui_using_subcommand attach' -l quiet                                          -d 'Suppress the detach/peek hint on attach'
complete -c hyoui -n '__hyoui_using_subcommand attach' -s h -l help                                    -d 'Show help and exit'

# `hyoui list` options.
complete -c hyoui -n '__hyoui_using_subcommand list' -l namespace -x         -d 'Show only this namespace'
complete -c hyoui -n '__hyoui_using_subcommand list' -l all-namespaces       -d 'List sessions across all namespaces (adds NS column)'
complete -c hyoui -n '__hyoui_using_subcommand list' -l prune-stale          -d 'Unlink stale sockets'
complete -c hyoui -n '__hyoui_using_subcommand list' -l format -x -a 'plain jsonl' -d 'Output format'
complete -c hyoui -n '__hyoui_using_subcommand list' -s h -l help            -d 'Show help and exit'

# `hyoui kill` options.
complete -c hyoui -n '__hyoui_using_subcommand kill' -l socket -r -F -d 'Explicit socket path'
complete -c hyoui -n '__hyoui_using_subcommand kill' -l index  -x    -d 'Session selector (1=oldest, -1=newest)'
complete -c hyoui -n '__hyoui_using_subcommand kill' -l namespace -x -d 'Session namespace (flag > env HYOUI_NAMESPACE > default)'
complete -c hyoui -n '__hyoui_using_subcommand kill' -l all          -d 'Kill all live sessions'
complete -c hyoui -n '__hyoui_using_subcommand kill' -l signal -x -a 'SIGHUP SIGINT SIGQUIT SIGABRT SIGKILL SIGUSR1 SIGUSR2 SIGTERM SIGCONT SIGTSTP SIGCHLD' -d 'Signal name (SIG-prefix uppercase, DR-0012)'
complete -c hyoui -n '__hyoui_using_subcommand kill' -l wait -x      -d 'Wait for child exit (bare=10s default, =DUR to override)'
complete -c hyoui -n '__hyoui_using_subcommand kill' -l kill-on-timeout -d 'Escalate to SIGKILL when --wait times out'
complete -c hyoui -n '__hyoui_using_subcommand kill' -l no-terminate -d 'Send signal only, keep session alive'
complete -c hyoui -n '__hyoui_using_subcommand kill' -s h -l help    -d 'Show help and exit'

# `hyoui status` options.
complete -c hyoui -n '__hyoui_using_subcommand status' -l socket -r -F -d 'Explicit socket path'
complete -c hyoui -n '__hyoui_using_subcommand status' -l index  -x    -d 'Session selector (1=oldest, -1=newest)'
complete -c hyoui -n '__hyoui_using_subcommand status' -l namespace -x -d 'Session namespace (flag > env HYOUI_NAMESPACE > default)'
complete -c hyoui -n '__hyoui_using_subcommand status' -l format -x -a 'plain json' -d 'Output format'
complete -c hyoui -n '__hyoui_using_subcommand status' -s h -l help    -d 'Show help and exit'

# `hyoui set` options + key=value 候補。
complete -c hyoui -n '__hyoui_using_subcommand set' -l socket -r -F -d 'Explicit socket path'
complete -c hyoui -n '__hyoui_using_subcommand set' -l index  -x    -d 'Session selector (1=oldest, -1=newest)'
complete -c hyoui -n '__hyoui_using_subcommand set' -l namespace -x -d 'Session namespace (flag > env HYOUI_NAMESPACE > default)'
complete -c hyoui -n '__hyoui_using_subcommand set' -s h -l help    -d 'Show help and exit'
complete -c hyoui -n '__hyoui_using_subcommand set' -f -a 'on-child-suspend=notify on-child-suspend=auto-resume' -d 'Runtime setting key=value'

# `hyoui tail` options.
complete -c hyoui -n '__hyoui_using_subcommand tail' -l socket          -r -F  -d 'Explicit socket path'
complete -c hyoui -n '__hyoui_using_subcommand tail' -l index           -x      -d 'Session selector (1=oldest, -1=newest)'
complete -c hyoui -n '__hyoui_using_subcommand tail' -l namespace -x -d 'Session namespace (flag > env HYOUI_NAMESPACE > default)'
complete -c hyoui -n '__hyoui_using_subcommand tail' -l follow                  -d 'Continue streaming live output'
complete -c hyoui -n '__hyoui_using_subcommand tail' -l strip-ansi              -d 'Strip ANSI escapes in output'
complete -c hyoui -n '__hyoui_using_subcommand tail' -l since           -x      -d 'Drop chunks older than DUR (500ms / 2s / 1m)'
complete -c hyoui -n '__hyoui_using_subcommand tail' -l since-strict            -d 'Exit non-zero if --since range was evicted'
complete -c hyoui -n '__hyoui_using_subcommand tail' -l last-bytes      -x      -d 'Trim to last N bytes'
complete -c hyoui -n '__hyoui_using_subcommand tail' -s h -l help               -d 'Show help and exit'

# `hyoui wait` options.
complete -c hyoui -n '__hyoui_using_subcommand wait' -l socket            -r -F  -d 'Explicit socket path'
complete -c hyoui -n '__hyoui_using_subcommand wait' -l index             -x      -d 'Session selector (1=oldest, -1=newest)'
complete -c hyoui -n '__hyoui_using_subcommand wait' -l namespace -x -d 'Session namespace (flag > env HYOUI_NAMESPACE > default)'
complete -c hyoui -n '__hyoui_using_subcommand wait' -l timeout           -x      -d 'Absolute timeout (5s / 30s)'
complete -c hyoui -n '__hyoui_using_subcommand wait' -l poll-interval      -x      -d 'Snapshot polling interval (default 100ms)'
complete -c hyoui -n '__hyoui_using_subcommand wait' -s h -l help                 -d 'Show help and exit'

# `hyoui screen` 子 subcommand
complete -c hyoui -n __hyoui_screen_no_sub -f -a dump     -d 'Dump screen / scrollback as ANSI / binary / CBOR'
complete -c hyoui -n __hyoui_screen_no_sub -f -a snapshot -d 'Take a structured snapshot (cells + cursor + mode + ...)'

# `hyoui screen dump` options
complete -c hyoui -n '__hyoui_screen_using_sub dump' -l socket  -r -F                          -d 'Explicit socket path'
complete -c hyoui -n '__hyoui_screen_using_sub dump' -l index   -x                              -d 'Session selector (1=oldest, -1=newest)'
complete -c hyoui -n '__hyoui_screen_using_sub dump' -l namespace -x -d 'Session namespace (flag > env HYOUI_NAMESPACE > default)'
complete -c hyoui -n '__hyoui_screen_using_sub dump' -l format  -x -a 'ansi binary cbor text/plain' -d 'Output format'
complete -c hyoui -n '__hyoui_screen_using_sub dump' -l layer   -x -a 'visible scrollback both' -d 'Layer to dump'
complete -c hyoui -n '__hyoui_screen_using_sub dump' -l rect    -x                              -d 'Sub-rectangle x,y,w,h'
complete -c hyoui -n '__hyoui_screen_using_sub dump' -l output  -r -F                          -d 'Output file path'
complete -c hyoui -n '__hyoui_screen_using_sub dump' -l timeout -x                              -d 'Response timeout'
complete -c hyoui -n '__hyoui_screen_using_sub dump' -s h -l help                               -d 'Show help and exit'

# `hyoui screen snapshot` options
complete -c hyoui -n '__hyoui_screen_using_sub snapshot' -l socket  -r -F                                -d 'Explicit socket path'
complete -c hyoui -n '__hyoui_screen_using_sub snapshot' -l index   -x                                    -d 'Session selector (1=oldest, -1=newest)'
complete -c hyoui -n '__hyoui_screen_using_sub snapshot' -l namespace -x -d 'Session namespace (flag > env HYOUI_NAMESPACE > default)'
complete -c hyoui -n '__hyoui_screen_using_sub snapshot' -l include -x -a 'cells cursor mode style scrollback windowsize buffer sequenceno' -d 'Snapshot components to include'
complete -c hyoui -n '__hyoui_screen_using_sub snapshot' -l format  -x -a 'cbor json'                     -d 'Output format'
complete -c hyoui -n '__hyoui_screen_using_sub snapshot' -l output  -r -F                                -d 'Output file path'
complete -c hyoui -n '__hyoui_screen_using_sub snapshot' -l timeout -x                                    -d 'Response timeout'
complete -c hyoui -n '__hyoui_screen_using_sub snapshot' -s h -l help                                     -d 'Show help and exit'

# `hyoui input` options + spec prefix
complete -c hyoui -n '__hyoui_using_subcommand input' -l socket          -r -F  -d 'Explicit socket path'
complete -c hyoui -n '__hyoui_using_subcommand input' -l index           -x      -d 'Session selector (1=oldest, -1=newest)'
complete -c hyoui -n '__hyoui_using_subcommand input' -l namespace -x -d 'Session namespace (flag > env HYOUI_NAMESPACE > default)'
complete -c hyoui -n '__hyoui_using_subcommand input' -l timeout         -x      -d 'Per-spec timeout (e.g. 5s)'
complete -c hyoui -n '__hyoui_using_subcommand input' -l lock-token      -x      -d 'Explicit lock token (overrides HYOUI_LOCK_TOKEN)'
complete -c hyoui -n '__hyoui_using_subcommand input' -l max-file-bytes  -x      -d 'Max bytes for file: spec (0 = unlimited)'
complete -c hyoui -n '__hyoui_using_subcommand input' -s h -l help              -d 'Show help and exit'
complete -c hyoui -n '__hyoui_using_subcommand input' -f -a 'text\: hex\: file\: paste\: key\: wait\: wait-idle\:' -d 'Input spec prefix'

# `hyoui lock` 子 subcommand
function __hyoui_lock_using_sub
    __hyoui_child_using lock $argv[1]
end
function __hyoui_lock_no_sub
    __hyoui_child_none lock
end
complete -c hyoui -n __hyoui_lock_no_sub -f -a acquire -d 'Acquire a lock, print token, hold connection'
complete -c hyoui -n __hyoui_lock_no_sub -f -a release -d 'Release a lock by token'

# `hyoui lock acquire` options
complete -c hyoui -n '__hyoui_lock_using_sub acquire' -l socket  -r -F           -d 'Explicit socket path'
complete -c hyoui -n '__hyoui_lock_using_sub acquire' -l index   -x              -d 'Session selector (1=oldest, -1=newest)'
complete -c hyoui -n '__hyoui_lock_using_sub acquire' -l namespace -x -d 'Session namespace (flag > env HYOUI_NAMESPACE > default)'
complete -c hyoui -n '__hyoui_lock_using_sub acquire' -l mode    -x -a 'wait fail' -d 'Behavior when held'
complete -c hyoui -n '__hyoui_lock_using_sub acquire' -l timeout -x              -d 'Acquire timeout'
complete -c hyoui -n '__hyoui_lock_using_sub acquire' -s h -l help               -d 'Show help and exit'

# `hyoui lock release` options
complete -c hyoui -n '__hyoui_lock_using_sub release' -l socket -r -F -d 'Explicit socket path'
complete -c hyoui -n '__hyoui_lock_using_sub release' -l index  -x    -d 'Session selector (1=oldest, -1=newest)'
complete -c hyoui -n '__hyoui_lock_using_sub release' -l namespace -x -d 'Session namespace (flag > env HYOUI_NAMESPACE > default)'
complete -c hyoui -n '__hyoui_lock_using_sub release' -l token  -x    -d 'Lock token to release'
complete -c hyoui -n '__hyoui_lock_using_sub release' -s h -l help    -d 'Show help and exit'

# `hyoui unlock` options
complete -c hyoui -n '__hyoui_using_subcommand unlock' -l socket -r -F -d 'Explicit socket path'
complete -c hyoui -n '__hyoui_using_subcommand unlock' -l index  -x    -d 'Session selector (1=oldest, -1=newest)'
complete -c hyoui -n '__hyoui_using_subcommand unlock' -l namespace -x -d 'Session namespace (flag > env HYOUI_NAMESPACE > default)'
complete -c hyoui -n '__hyoui_using_subcommand unlock' -l token  -x    -d 'Lock token to release'
complete -c hyoui -n '__hyoui_using_subcommand unlock' -s h -l help    -d 'Show help and exit'

# `hyoui detach` (DR-0020 §4)
complete -c hyoui -n '__hyoui_using_subcommand detach' -l socket -r -F -d 'Explicit socket path'
complete -c hyoui -n '__hyoui_using_subcommand detach' -l index  -x    -d 'Session selector (1=oldest, -1=newest)'
complete -c hyoui -n '__hyoui_using_subcommand detach' -l namespace -x -d 'Session namespace (flag > env HYOUI_NAMESPACE > default)'
complete -c hyoui -n '__hyoui_using_subcommand detach' -s h -l help    -d 'Show help and exit'

# `hyoui record` 子 subcommand
function __hyoui_record_using_sub
    __hyoui_child_using record $argv[1]
end
function __hyoui_record_no_sub
    __hyoui_child_none record
end
complete -c hyoui -n __hyoui_record_no_sub -f -a start -d 'Start a new record (returns record_id)'
complete -c hyoui -n __hyoui_record_no_sub -f -a stop  -d 'Stop a running record (--id <N> or --all)'
complete -c hyoui -n __hyoui_record_no_sub -f -a list  -d 'List active records for the session'

# `hyoui record start` options
complete -c hyoui -n '__hyoui_record_using_sub start' -l socket        -r -F  -d 'Explicit socket path'
complete -c hyoui -n '__hyoui_record_using_sub start' -l index         -x      -d 'Session selector (1=oldest, -1=newest)'
complete -c hyoui -n '__hyoui_record_using_sub start' -l namespace -x -d 'Session namespace (flag > env HYOUI_NAMESPACE > default)'
complete -c hyoui -n '__hyoui_record_using_sub start' -l output        -r -F  -d 'Output file path (absolute)'
complete -c hyoui -n '__hyoui_record_using_sub start' -l stdin                 -d 'Record child PTY input only'
complete -c hyoui -n '__hyoui_record_using_sub start' -l stdout                -d 'Record child PTY output only'
complete -c hyoui -n '__hyoui_record_using_sub start' -l both                  -d 'Record both directions (default, jsonl only)'
complete -c hyoui -n '__hyoui_record_using_sub start' -l format -x -a 'jsonl raw' -d 'Output format'
complete -c hyoui -n '__hyoui_record_using_sub start' -l max-bytes    -x       -d 'Recording byte cap (0 disables)'
complete -c hyoui -n '__hyoui_record_using_sub start' -l max-duration -x       -d 'Recording duration cap (0 disables)'
complete -c hyoui -n '__hyoui_record_using_sub start' -l input-secrecy -x -a 'record-all never-record-stdin' -d 'stdin redaction policy (default record-all; redact-after-prompt reserved for Phase 5)'
complete -c hyoui -n '__hyoui_record_using_sub start' -l prompt-pattern -x     -d 'Custom prompt detection regex'
complete -c hyoui -n '__hyoui_record_using_sub start' -s h -l help             -d 'Show help and exit'

# `hyoui record stop` options
complete -c hyoui -n '__hyoui_record_using_sub stop' -l socket -r -F -d 'Explicit socket path'
complete -c hyoui -n '__hyoui_record_using_sub stop' -l index  -x    -d 'Session selector (1=oldest, -1=newest)'
complete -c hyoui -n '__hyoui_record_using_sub stop' -l namespace -x -d 'Session namespace (flag > env HYOUI_NAMESPACE > default)'
complete -c hyoui -n '__hyoui_record_using_sub stop' -l id     -x    -d 'record_id to stop'
complete -c hyoui -n '__hyoui_record_using_sub stop' -l all          -d 'Stop all active records'
complete -c hyoui -n '__hyoui_record_using_sub stop' -s h -l help    -d 'Show help and exit'

# `hyoui record list` options
complete -c hyoui -n '__hyoui_record_using_sub list' -l socket -r -F -d 'Explicit socket path'
complete -c hyoui -n '__hyoui_record_using_sub list' -l index  -x    -d 'Session selector (1=oldest, -1=newest)'
complete -c hyoui -n '__hyoui_record_using_sub list' -l namespace -x -d 'Session namespace (flag > env HYOUI_NAMESPACE > default)'
complete -c hyoui -n '__hyoui_record_using_sub list' -l format -x -a 'table jsonl' -d 'Output format'
complete -c hyoui -n '__hyoui_record_using_sub list' -s h -l help    -d 'Show help and exit'

# `hyoui config` 子 subcommand
function __hyoui_config_using_sub
    __hyoui_child_using config $argv[1]
end
function __hyoui_config_no_sub
    __hyoui_child_none config
end
complete -c hyoui -n __hyoui_config_no_sub -f -a path -d 'Print the resolved config file path'
complete -c hyoui -n __hyoui_config_no_sub -f -a show -d 'Print the effective configuration as TOML'
complete -c hyoui -n '__hyoui_config_using_sub path' -s h -l help -d 'Show help and exit'
complete -c hyoui -n '__hyoui_config_using_sub show' -s h -l help -d 'Show help and exit'
"#
}

#[cfg(test)]
mod tests {
    use super::*;
    use hyoui::cli::{
        CONFIG_SUBCOMMANDS, IMPLEMENTED_TOP_LEVEL_SUBCOMMANDS, LIST_FORMAT_VALUES,
        LOCK_SUBCOMMANDS, RECORD_INPUT_SECRECY_VALUES, RECORD_LIST_FORMAT_VALUES,
        RECORD_START_FORMAT_VALUES, RECORD_SUBCOMMANDS, RESERVED_TOP_LEVEL_SUBCOMMANDS,
        SCREEN_DUMP_FORMAT_VALUES, SCREEN_DUMP_LAYER_VALUES, SCREEN_SNAPSHOT_FORMAT_VALUES,
        SCREEN_SUBCOMMANDS, SNAPSHOT_INCLUDE_VALUES, STATUS_FORMAT_VALUES,
    };

    const ALL_SHELLS: [Shell; 3] = [Shell::Bash, Shell::Zsh, Shell::Fish];

    /// Word-boundary aware containment check.
    ///
    /// `s.contains("list")` matches inside `prune-stale` etc.; for short tokens
    /// like `list` / `stop` we want a real boundary so the SSOT verification does
    /// not pass on incidental substrings. Boundaries are the usual shell-script
    /// delimiters plus `\:` (zsh escaped spec prefixes).
    fn contains_token(s: &str, token: &str) -> bool {
        let is_word = |c: char| c.is_ascii_alphanumeric() || c == '_';
        let bytes = s.as_bytes();
        let mut start = 0;
        while let Some(off) = s[start..].find(token) {
            let i = start + off;
            let before_ok = i == 0 || !is_word(s[..i].chars().next_back().unwrap());
            let after_idx = i + token.len();
            let after_ok =
                after_idx >= bytes.len() || !is_word(s[after_idx..].chars().next().unwrap_or(' '));
            if before_ok && after_ok {
                return true;
            }
            start = i + 1;
        }
        false
    }

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
        assert!(s.contains("rw ro rw-no-leader"));
    }

    #[test]
    fn completion_fish_uses_complete_c() {
        let s = script(Shell::Fish);
        assert!(s.contains("complete -c hyoui"));
        assert!(s.contains("bash zsh fish"));
        assert!(s.contains("--mode") || s.contains(" mode "));
    }

    /// SSOT: every implemented top-level subcommand must appear in all shells.
    #[test]
    fn completion_all_shells_mention_every_implemented_subcommand() {
        for sh in ALL_SHELLS {
            let s = script(sh);
            for sub in IMPLEMENTED_TOP_LEVEL_SUBCOMMANDS {
                assert!(
                    contains_token(&s, sub),
                    "shell {sh:?} missing implemented subcommand `{sub}`"
                );
            }
        }
    }

    /// SSOT (= 廃止物検証): reserved subcommands must NOT appear in any shell.
    ///
    /// `parse_args` returns "reserved but not yet implemented" for these, so
    /// offering them as completion candidates would mislead users.
    ///
    /// Boundaries here are stricter than `contains_token` (= whitespace / quote /
    /// line edge only), so legitimate substrings like `detach-others` (an attach
    /// flag) do not trip the check — only a bare `detach` candidate would.
    #[test]
    fn completion_all_shells_omit_reserved_subcommands() {
        let is_boundary = |c: Option<char>| match c {
            None => true,
            Some(c) => c.is_whitespace() || c == '\'' || c == '"' || c == '`',
        };
        for sh in ALL_SHELLS {
            let s = script(sh);
            for sub in RESERVED_TOP_LEVEL_SUBCOMMANDS {
                let mut start = 0;
                while let Some(off) = s[start..].find(*sub) {
                    let i = start + off;
                    let before = s[..i].chars().next_back();
                    let rest = &s[i + sub.len()..];
                    let after = rest.chars().next();
                    // DR-0019 §5: `--stdin-eof` の値リスト `detach send-eof` に含まれる
                    // `detach` は reserved subcommand `detach` と文字列衝突するが、値
                    // 補完であって subcommand 候補ではない。直後が ` send-eof` なら値
                    // リストの一部として除外する (= false positive 回避)。
                    let is_stdin_eof_value = rest.trim_start().starts_with("send-eof");
                    if !is_stdin_eof_value {
                        assert!(
                            !(is_boundary(before) && is_boundary(after)),
                            "shell {sh:?} leaks reserved subcommand `{sub}` as a bare candidate"
                        );
                    }
                    start = i + 1;
                }
            }
        }
    }

    /// SSOT: legacy / removed wait flags and the removed run flag must be gone.
    #[test]
    fn completion_all_shells_omit_removed_flags() {
        for sh in ALL_SHELLS {
            let s = script(sh);
            for flag in [
                "--no-strip-escapes",   // 旧 wait flag (廃止)
                "--newline-convert-lf", // 旧 wait flag (廃止)
                "--on-parent-suspend",  // DR-0015 で run から廃止
            ] {
                assert!(
                    !s.contains(flag),
                    "shell {sh:?} still offers removed flag `{flag}`"
                );
            }
        }
    }

    /// Does the script offer a long option `name` (without `--`)?
    ///
    /// bash/zsh spell it `--name`; fish declares it as `-l name`. Accept both.
    fn offers_long_opt(s: &str, name: &str) -> bool {
        s.contains(&format!("--{name}")) || s.contains(&format!("-l {name}"))
    }

    /// SSOT: kill must offer the wait/escalation flags (= `--wait` /
    /// `--kill-on-timeout` / `--no-terminate`) in every shell.
    #[test]
    fn completion_all_shells_offer_kill_wait_flags() {
        for sh in ALL_SHELLS {
            let s = script(sh);
            for flag in ["wait", "kill-on-timeout", "no-terminate"] {
                assert!(
                    offers_long_opt(&s, flag),
                    "shell {sh:?} missing kill `--{flag}`"
                );
            }
        }
    }

    /// SSOT: wait must offer the current `--poll-interval` flag.
    #[test]
    fn completion_all_shells_offer_wait_poll_interval() {
        for sh in ALL_SHELLS {
            let s = script(sh);
            assert!(
                offers_long_opt(&s, "poll-interval"),
                "shell {sh:?} missing wait `--poll-interval`"
            );
        }
    }

    #[test]
    fn completion_all_shells_mention_screen_subsubcommands() {
        for sh in ALL_SHELLS {
            let s = script(sh);
            for sub in SCREEN_SUBCOMMANDS {
                assert!(
                    contains_token(&s, sub),
                    "shell {sh:?} missing screen `{sub}`"
                );
            }
        }
    }

    #[test]
    fn completion_all_shells_mention_config_subsubcommands() {
        for sh in ALL_SHELLS {
            let s = script(sh);
            for sub in CONFIG_SUBCOMMANDS {
                assert!(
                    contains_token(&s, sub),
                    "shell {sh:?} missing config `{sub}`"
                );
            }
        }
    }

    #[test]
    fn completion_all_shells_mention_lock_and_record_subsubcommands() {
        for sh in ALL_SHELLS {
            let s = script(sh);
            for sub in LOCK_SUBCOMMANDS {
                assert!(contains_token(&s, sub), "shell {sh:?} missing lock `{sub}`");
            }
            for sub in RECORD_SUBCOMMANDS {
                assert!(
                    contains_token(&s, sub),
                    "shell {sh:?} missing record `{sub}`"
                );
            }
        }
    }

    /// SSOT: every accepted `screen snapshot --include` value must appear and the
    /// old bogus values (`size` / `title`) must be gone.
    #[test]
    fn completion_all_shells_sync_snapshot_include_values() {
        for sh in ALL_SHELLS {
            let s = script(sh);
            for v in SNAPSHOT_INCLUDE_VALUES {
                assert!(
                    contains_token(&s, v),
                    "shell {sh:?} missing snapshot include value `{v}`"
                );
            }
        }
        // 旧 completion の誤値 (parse 実装は受理しない) は include 文脈で出ない。
        for sh in ALL_SHELLS {
            let s = script(sh);
            assert!(
                !s.contains("size mode title"),
                "shell {sh:?} still lists bogus snapshot include set (size/title)"
            );
        }
    }

    /// SSOT: enum-valued flags expose exactly the parser-accepted values.
    #[test]
    fn completion_all_shells_sync_enum_value_sets() {
        for sh in ALL_SHELLS {
            let s = script(sh);
            let groups: &[(&str, &[&str])] = &[
                ("screen dump --format", SCREEN_DUMP_FORMAT_VALUES),
                ("screen dump --layer", SCREEN_DUMP_LAYER_VALUES),
                ("screen snapshot --format", SCREEN_SNAPSHOT_FORMAT_VALUES),
                ("list --format", LIST_FORMAT_VALUES),
                ("status --format", STATUS_FORMAT_VALUES),
                ("record list --format", RECORD_LIST_FORMAT_VALUES),
                ("record start --format", RECORD_START_FORMAT_VALUES),
                ("record start --input-secrecy", RECORD_INPUT_SECRECY_VALUES),
            ];
            for (label, values) in groups {
                for v in *values {
                    assert!(s.contains(v), "shell {sh:?} missing `{v}` for {label}");
                }
            }
        }
    }

    #[test]
    fn completion_all_shells_mention_input_spec_prefixes() {
        for sh in ALL_SHELLS {
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

    /// `--index` selector must be offered by every session-targeted subcommand.
    #[test]
    fn completion_all_shells_offer_index_selector() {
        for sh in ALL_SHELLS {
            let s = script(sh);
            assert!(
                offers_long_opt(&s, "index"),
                "shell {sh:?} missing `--index` selector"
            );
        }
    }

    /// DR-0018: `--namespace` selector and `list --all-namespaces` must be offered.
    ///
    /// `--namespace` is accepted by every session-targeted subcommand (run / attach /
    /// list / kill / status / tail / wait / screen / input / lock / unlock / record),
    /// so each shell script must offer it. `--all-namespaces` is list-only.
    #[test]
    fn completion_all_shells_offer_namespace_selector() {
        for sh in ALL_SHELLS {
            let s = script(sh);
            assert!(
                offers_long_opt(&s, "namespace"),
                "shell {sh:?} missing `--namespace` selector"
            );
            assert!(
                offers_long_opt(&s, "all-namespaces"),
                "shell {sh:?} missing list `--all-namespaces`"
            );
        }
    }
}
