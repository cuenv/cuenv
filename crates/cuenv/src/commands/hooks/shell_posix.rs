//! POSIX shell-local environment restoration helpers.

pub(super) const RUNTIME: &str = r#"
__cuenv_error() {
    builtin printf '%s\n' 'cuenv cannot restore this shell environment; start a fresh shell or explicitly clear the inherited environment' >&2
    return 1
}

__cuenv_safe_scope() {
    builtin local __cuenv_frame __cuenv_previous= __cuenv_needs_hook=0
    if builtin [ -n "${ZSH_VERSION-}" ]; then
        for __cuenv_frame in "${funcstack[@]}"; do
            if builtin [ "$__cuenv_needs_hook" = 1 ]; then
                builtin [ "$__cuenv_frame" = __cuenv_hook ] || return 1
                __cuenv_needs_hook=0
            fi
            case "$__cuenv_frame" in
                __cuenv_safe_scope|__cuenv_apply|__cuenv_unload|__cuenv_hook|__cuenv_run|__cuenv_produce|cuenv) ;;
                '(eval)')
                    case "$__cuenv_previous" in __cuenv_apply|__cuenv_unload) __cuenv_needs_hook=1 ;; *) return 1 ;; esac
                    ;;
                *) return 1 ;;
            esac
            __cuenv_previous=$__cuenv_frame
        done
        builtin [ "$__cuenv_needs_hook" = 0 ] || return 1
    else
        for __cuenv_frame in "${FUNCNAME[@]}"; do
            case "$__cuenv_frame" in
                __cuenv_safe_scope|__cuenv_apply|__cuenv_unload|__cuenv_hook|__cuenv_run|__cuenv_produce|cuenv) ;;
                *) return 1 ;;
            esac
        done
    fi
}

__cuenv_valid_name() {
    case "$1" in
        [Bb][Aa][Ss][Hh]_*|[Zz][Ss][Hh]_*|[Ff][Ii][Ss][Hh]_*|PS1|PS2|PS3|PS4|ENV|SHELLOPTS|BASHOPTS|BASHPID|ZDOTDIR|PWD|OLDPWD|LINENO|SECONDS|RANDOM|SRANDOM|EPOCHSECONDS|EPOCHREALTIME|SHLVL|PPID|UID|EUID|GROUPS|FUNCNAME|ARGV|STATUS|PIPESTATUS|HISTORY|VERSION|PROMPT_COMMAND|OPTIND|OPTARG|IFS|_) return 1 ;;
        ''|[0-9]*|*[!A-Za-z0-9_]*|__[Cc][Uu][Ee][Nn][Vv]_*|[Cc][Uu][Ee][Nn][Vv]_[Mm][Aa][Nn][Aa][Gg][Ee][Dd]_[Vv][Aa][Rr][Ss]|[Cc][Uu][Ee][Nn][Vv]_[Hh][Oo][Oo][Kk]_[Ss][Ee][Ss][Ss][Ii][Oo][Nn]|[Cc][Uu][Ee][Nn][Vv]_[Ss][Hh][Ee][Ll][Ll]_[Ii][Nn][Tt][Ee][Gg][Rr][Aa][Tt][Ii][Oo][Nn]|[Cc][Uu][Ee][Nn][Vv]_[Ll][Oo][Aa][Dd][Ee][Dd]_[Dd][Ii][Rr]|[Cc][Uu][Ee][Nn][Vv]_[Pp][Ee][Nn][Dd][Ii][Nn][Gg]_[Aa][Pp][Pp][Rr][Oo][Vv][Aa][Ll]_[Dd][Ii][Rr]) return 1 ;;
    esac
}

# Writes to dynamically scoped result variables declared by the caller.
__cuenv_inspect() {
    builtin local __cuenv_decl __cuenv_flags
    builtin eval '__cuenv_present=${'"$1"'+1}'
    builtin eval '__cuenv_value=${'"$1"'-}'
    __cuenv_exported=0
    __cuenv_readonly=0
    __cuenv_unsupported=0
    if __cuenv_decl=$(builtin typeset -p "$1" 2>/dev/null); then
        case "$__cuenv_decl" in
            export\ *) __cuenv_exported=1 ;;
        esac
        __cuenv_flags=${__cuenv_decl#* }
        while :; do
            builtin local __cuenv_flag=${__cuenv_flags%% *}
            case "$__cuenv_flag" in
                -*)
                    case "$__cuenv_flag" in *x*) __cuenv_exported=1 ;; esac
                    case "$__cuenv_flag" in *r*) __cuenv_readonly=1 ;; esac
                    case "${__cuenv_flag#-}" in *[!xgT-]*) __cuenv_unsupported=1 ;; esac
                    case "$__cuenv_flag" in
                        *T*)
                            if builtin [ -z "${ZSH_VERSION-}" ] || builtin [ "$1" != PATH ]; then
                                __cuenv_unsupported=1
                            elif builtin [ "${#path[@]}" = 0 ]; then
                                __cuenv_unsupported=1
                            else
                                builtin local __cuenv_path_element
                                for __cuenv_path_element in "${path[@]}"; do
                                    case "$__cuenv_path_element" in *:*) __cuenv_unsupported=1 ;; esac
                                done
                            fi
                            ;;
                    esac
                    ;;
                *) break ;;
            esac
            case "$__cuenv_flags" in *' '*) __cuenv_flags=${__cuenv_flags#* } ;; *) break ;; esac
        done
    fi
}

__cuenv_unexport() {
    if builtin [ -n "${ZSH_VERSION-}" ]; then
        builtin typeset -g +x "$1"
    else
        builtin export -n "$1"
    fi
}

__cuenv_put() {
    builtin printf -v "$1" '%s' "$2" && __cuenv_unexport "$1"
}

__cuenv_validate() {
    builtin local __cuenv_expected=$1 __cuenv_rest __cuenv_nonce __cuenv_project __cuenv_names
    builtin local __cuenv_name __cuenv_field __cuenv_key __cuenv_present __cuenv_value
    builtin local __cuenv_exported __cuenv_readonly __cuenv_unsupported
    builtin [ "${CUENV_MANAGED_VARS-}" = "$__cuenv_expected" ] || return 1
    builtin [ "${__cuenv_private_token-}" = "$__cuenv_expected" ] || return 1
    builtin [ -n "$__cuenv_expected" ] || return 0
    case "$__cuenv_expected" in v1:*:*:*) ;; *) return 1 ;; esac
    __cuenv_rest=${__cuenv_expected#v1:}
    __cuenv_nonce=${__cuenv_rest%%:*}; __cuenv_rest=${__cuenv_rest#*:}
    __cuenv_project=${__cuenv_rest%%:*}; __cuenv_names=${__cuenv_rest#*:}
    case "$__cuenv_nonce" in *[!0-9a-f-]*|'') return 1 ;; esac
    case "$__cuenv_project" in *[!0-9a-f]*|'') return 1 ;; esac
    builtin [ "${#__cuenv_project}" -eq 64 ] || return 1
    builtin [ "${#__cuenv_expected}" -le 24576 ] || return 1
    builtin [ -n "$__cuenv_names" ] || return 1
    __cuenv_nonce=${__cuenv_nonce//-/}
    builtin [ "${#__cuenv_nonce}" -eq 32 ] || return 1
    __cuenv_rest=$__cuenv_names
    while :; do
        __cuenv_name=${__cuenv_rest%%,*}
        __cuenv_valid_name "$__cuenv_name" || return 1
        for __cuenv_field in p x v a q s; do
            __cuenv_key=__cuenv_${__cuenv_nonce}_${__cuenv_field}_${__cuenv_name}
            __cuenv_inspect "$__cuenv_key"
            builtin [ "${__cuenv_present-}" = 1 ] && builtin [ "$__cuenv_exported" = 0 ] && builtin [ "$__cuenv_readonly" = 0 ] || return 1
        done
        case "$__cuenv_rest" in *,*) __cuenv_rest=${__cuenv_rest#*,} ;; *) break ;; esac
    done
}

__cuenv_token() {
    builtin local __cuenv_next
    if builtin [ -z "$__cuenv_names" ]; then
        builtin unset CUENV_MANAGED_VARS __cuenv_private_token || return 1
    else
        __cuenv_next=v1:${__cuenv_uuid}:${__cuenv_project}:${__cuenv_names}
        __cuenv_put __cuenv_private_token "$__cuenv_next" || return 1
        builtin export CUENV_MANAGED_VARS="$__cuenv_next" || return 1
    fi
}

__cuenv_remove_name() {
    builtin local __cuenv_rest=$__cuenv_names __cuenv_part __cuenv_next=
    while builtin [ -n "$__cuenv_rest" ]; do
        __cuenv_part=${__cuenv_rest%%,*}
        if builtin [ "$__cuenv_part" != "$1" ]; then
            if builtin [ -n "$__cuenv_next" ]; then __cuenv_next=$__cuenv_next,$__cuenv_part; else __cuenv_next=$__cuenv_part; fi
        fi
        case "$__cuenv_rest" in *,*) __cuenv_rest=${__cuenv_rest#*,} ;; *) break ;; esac
    done
    __cuenv_names=$__cuenv_next
}

__cuenv_restore_one() {
    builtin local __cuenv_key __cuenv_present __cuenv_value __cuenv_exported __cuenv_readonly __cuenv_unsupported
    builtin local __cuenv_was_present __cuenv_was_exported __cuenv_old __cuenv_applied __cuenv_pending __cuenv_stage
    __cuenv_key=__cuenv_${__cuenv_nonce}_p_$1; builtin eval '__cuenv_was_present=${'"$__cuenv_key"'-}'
    __cuenv_key=__cuenv_${__cuenv_nonce}_x_$1; builtin eval '__cuenv_was_exported=${'"$__cuenv_key"'-}'
    __cuenv_key=__cuenv_${__cuenv_nonce}_v_$1; builtin eval '__cuenv_old=${'"$__cuenv_key"'-}'
    __cuenv_key=__cuenv_${__cuenv_nonce}_a_$1; builtin eval '__cuenv_applied=${'"$__cuenv_key"'-}'
    __cuenv_key=__cuenv_${__cuenv_nonce}_q_$1; builtin eval '__cuenv_pending=${'"$__cuenv_key"'-}'
    __cuenv_key=__cuenv_${__cuenv_nonce}_s_$1; builtin eval '__cuenv_stage=${'"$__cuenv_key"'-}'
    __cuenv_inspect "$1"
    if builtin [ "${__cuenv_present-}" = 1 ] && builtin [ "$__cuenv_exported" = 1 ] && { builtin [ "$__cuenv_value" = "$__cuenv_applied" ] || { builtin [ "$__cuenv_stage" = 1 ] && builtin [ "$__cuenv_value" = "$__cuenv_pending" ]; }; }; then
        if builtin [ "$__cuenv_was_present" = 1 ]; then
            builtin printf -v "$1" '%s' "$__cuenv_old" || return 1
            if builtin [ "$__cuenv_was_exported" = 1 ]; then builtin export "$1" || return 1; else __cuenv_unexport "$1" || return 1; fi
        else
            builtin unset "$1" || return 1
            if builtin [ "$__cuenv_was_exported" = 1 ]; then builtin export "$1" || return 1; fi
        fi
    fi
    __cuenv_remove_name "$1"
    __cuenv_token || return 1
    builtin unset "__cuenv_${__cuenv_nonce}_p_$1" "__cuenv_${__cuenv_nonce}_x_$1" "__cuenv_${__cuenv_nonce}_v_$1" "__cuenv_${__cuenv_nonce}_a_$1" "__cuenv_${__cuenv_nonce}_q_$1" "__cuenv_${__cuenv_nonce}_s_$1"
}

__cuenv_preflight_target() {
    builtin local __cuenv_present __cuenv_value __cuenv_exported __cuenv_readonly __cuenv_unsupported
    __cuenv_inspect "$1"
    builtin [ "$__cuenv_readonly" = 0 ] && builtin [ "$__cuenv_unsupported" = 0 ]
}

__cuenv_unload_impl() {
    builtin local __cuenv_expected=$1 __cuenv_retry_requested=$2 __cuenv_rest __cuenv_name
    builtin local __cuenv_uuid __cuenv_project __cuenv_names __cuenv_nonce
    __cuenv_validate "$__cuenv_expected" || { __cuenv_error; return 1; }
    builtin [ -n "$__cuenv_expected" ] || return 0
    __cuenv_rest=${__cuenv_expected#v1:}
    __cuenv_uuid=${__cuenv_rest%%:*}; __cuenv_rest=${__cuenv_rest#*:}
    __cuenv_project=${__cuenv_rest%%:*}; __cuenv_names=${__cuenv_rest#*:}
    __cuenv_nonce=${__cuenv_uuid//-/}
    for __cuenv_name in CUENV_MANAGED_VARS CUENV_LOADED_DIR CUENV_PENDING_APPROVAL_DIR __cuenv_private_token; do
        __cuenv_preflight_target "$__cuenv_name" || { __cuenv_error; return 1; }
    done
    __cuenv_rest=$__cuenv_names
    while :; do
        __cuenv_name=${__cuenv_rest%%,*}
        __cuenv_preflight_target "$__cuenv_name" || { __cuenv_error; return 1; }
        case "$__cuenv_rest" in *,*) __cuenv_rest=${__cuenv_rest#*,} ;; *) break ;; esac
    done
    __cuenv_rest=$__cuenv_names
    while :; do
        __cuenv_name=${__cuenv_rest%%,*}
        __cuenv_restore_one "$__cuenv_name" || { __cuenv_error; return 1; }
        case "$__cuenv_rest" in *,*) __cuenv_rest=${__cuenv_rest#*,} ;; *) break ;; esac
    done
    builtin unset CUENV_LOADED_DIR CUENV_PENDING_APPROVAL_DIR || return 1
    __cuenv_retry=$__cuenv_retry_requested
}

__cuenv_unload() {
    case $- in *a*) builtin set +a; builtin local __cuenv_allexport=1 ;; *) builtin local __cuenv_allexport=0 ;; esac
    builtin local __cuenv_trace=0 __cuenv_status
    case $- in *x*) __cuenv_trace=1; builtin set +x ;; esac
    if ! __cuenv_safe_scope; then __cuenv_error; __cuenv_status=1
    elif __cuenv_unload_impl "$@"; then __cuenv_status=0; else __cuenv_status=$?; fi
    if builtin [ "$__cuenv_trace" = 1 ]; then builtin set -x; fi
    if builtin [ "$__cuenv_allexport" = 1 ]; then builtin set -a; fi
    return "$__cuenv_status"
}

__cuenv_apply_impl() {
    builtin local __cuenv_expected=$1 __cuenv_project=$2 __cuenv_uuid=$3
    builtin local __cuenv_nonce=${3//-/} __cuenv_names= __cuenv_old_names= __cuenv_desired= __cuenv_union=
    builtin local __cuenv_rest __cuenv_name __cuenv_field __cuenv_key __cuenv_new_value
    builtin local __cuenv_present __cuenv_value __cuenv_exported __cuenv_readonly __cuenv_unsupported
    builtin local __cuenv_was_present __cuenv_was_exported __cuenv_old __cuenv_applied __cuenv_pending __cuenv_stage
    shift 3
    builtin local -a __cuenv_args
    __cuenv_args=("$@")
    __cuenv_validate "$__cuenv_expected" || { __cuenv_error; return 1; }
    if builtin [ -n "$__cuenv_expected" ]; then
        __cuenv_rest=${__cuenv_expected#v1:}
        builtin [ "${__cuenv_rest%%:*}" = "$__cuenv_uuid" ] || { __cuenv_error; return 1; }
        __cuenv_rest=${__cuenv_rest#*:}
        builtin [ "${__cuenv_rest%%:*}" = "$__cuenv_project" ] || { __cuenv_error; return 1; }
        __cuenv_names=${__cuenv_rest#*:}; __cuenv_old_names=$__cuenv_names
    fi
    __cuenv_union=$__cuenv_names
    __cuenv_rest=$__cuenv_old_names
    while builtin [ -n "$__cuenv_rest" ]; do
        __cuenv_name=${__cuenv_rest%%,*}
        __cuenv_preflight_target "$__cuenv_name" || { __cuenv_error; return 1; }
        case "$__cuenv_rest" in *,*) __cuenv_rest=${__cuenv_rest#*,} ;; *) break ;; esac
    done
    for __cuenv_name in CUENV_MANAGED_VARS CUENV_LOADED_DIR CUENV_PENDING_APPROVAL_DIR __cuenv_private_token; do
        __cuenv_preflight_target "$__cuenv_name" || { __cuenv_error; return 1; }
    done
    builtin [ $(( $# % 2 )) = 0 ] || { __cuenv_error; return 1; }
    while builtin [ "$#" -gt 0 ]; do
        __cuenv_name=$1; shift 2
        __cuenv_valid_name "$__cuenv_name" && __cuenv_preflight_target "$__cuenv_name" || { __cuenv_error; return 1; }
        case ",$__cuenv_desired," in *,"$__cuenv_name",*) __cuenv_error; return 1 ;; esac
        if builtin [ -n "$__cuenv_desired" ]; then __cuenv_desired=$__cuenv_desired,$__cuenv_name; else __cuenv_desired=$__cuenv_name; fi
        case ",$__cuenv_union," in
            *,"$__cuenv_name",*) ;;
            *)
                for __cuenv_field in p x v a q s; do
                    __cuenv_key=__cuenv_${__cuenv_nonce}_${__cuenv_field}_${__cuenv_name}
                    __cuenv_inspect "$__cuenv_key"
                    builtin [ -z "${__cuenv_present-}" ] && builtin [ "$__cuenv_readonly" = 0 ] || { __cuenv_error; return 1; }
                done
                if builtin [ -n "$__cuenv_union" ]; then __cuenv_union=$__cuenv_union,$__cuenv_name; else __cuenv_union=$__cuenv_name; fi
                ;;
        esac
    done
    builtin [ "${#__cuenv_union}" -le 24400 ] || { __cuenv_error; return 1; }
    __cuenv_rest=$__cuenv_old_names
    while builtin [ -n "$__cuenv_rest" ]; do
        __cuenv_name=${__cuenv_rest%%,*}
        case ",$__cuenv_desired," in
            *,"$__cuenv_name",*) ;;
            *) __cuenv_restore_one "$__cuenv_name" || { __cuenv_error; return 1; } ;;
        esac
        case "$__cuenv_rest" in *,*) __cuenv_rest=${__cuenv_rest#*,} ;; *) break ;; esac
    done
    builtin set -- "${__cuenv_args[@]}"
    while builtin [ "$#" -gt 0 ]; do
        __cuenv_name=$1; __cuenv_new_value=$2; shift 2
        __cuenv_inspect "$__cuenv_name"
        __cuenv_was_present=${__cuenv_present:-0}; __cuenv_was_exported=$__cuenv_exported; __cuenv_old=$__cuenv_value
        case ",$__cuenv_old_names," in
            *,"$__cuenv_name",*)
                __cuenv_key=__cuenv_${__cuenv_nonce}_a_$__cuenv_name; builtin eval '__cuenv_applied=${'"$__cuenv_key"'-}'
                __cuenv_key=__cuenv_${__cuenv_nonce}_q_$__cuenv_name; builtin eval '__cuenv_pending=${'"$__cuenv_key"'-}'
                __cuenv_key=__cuenv_${__cuenv_nonce}_s_$__cuenv_name; builtin eval '__cuenv_stage=${'"$__cuenv_key"'-}'
                if builtin [ "$__cuenv_was_present" != 1 ] || builtin [ "$__cuenv_was_exported" != 1 ] || { builtin [ "$__cuenv_old" != "$__cuenv_applied" ] && { builtin [ "$__cuenv_stage" != 1 ] || builtin [ "$__cuenv_old" != "$__cuenv_pending" ]; }; }; then
                    __cuenv_put "__cuenv_${__cuenv_nonce}_p_$__cuenv_name" "$__cuenv_was_present" || return 1
                    __cuenv_put "__cuenv_${__cuenv_nonce}_x_$__cuenv_name" "$__cuenv_was_exported" || return 1
                    __cuenv_put "__cuenv_${__cuenv_nonce}_v_$__cuenv_name" "$__cuenv_old" || return 1
                fi
                ;;
            *)
                __cuenv_put "__cuenv_${__cuenv_nonce}_p_$__cuenv_name" "$__cuenv_was_present" || return 1
                __cuenv_put "__cuenv_${__cuenv_nonce}_x_$__cuenv_name" "$__cuenv_was_exported" || return 1
                __cuenv_put "__cuenv_${__cuenv_nonce}_v_$__cuenv_name" "$__cuenv_old" || return 1
                __cuenv_put "__cuenv_${__cuenv_nonce}_a_$__cuenv_name" "$__cuenv_old" || return 1
                __cuenv_put "__cuenv_${__cuenv_nonce}_q_$__cuenv_name" "$__cuenv_new_value" || return 1
                __cuenv_put "__cuenv_${__cuenv_nonce}_s_$__cuenv_name" 1 || return 1
                if builtin [ -n "$__cuenv_names" ]; then __cuenv_names=$__cuenv_names,$__cuenv_name; else __cuenv_names=$__cuenv_name; fi
                __cuenv_token || return 1
                ;;
        esac
        __cuenv_put "__cuenv_${__cuenv_nonce}_q_$__cuenv_name" "$__cuenv_new_value" || return 1
        __cuenv_put "__cuenv_${__cuenv_nonce}_s_$__cuenv_name" 1 || return 1
        builtin export "$__cuenv_name=$__cuenv_new_value" || return 1
        __cuenv_put "__cuenv_${__cuenv_nonce}_a_$__cuenv_name" "$__cuenv_new_value" || return 1
        __cuenv_put "__cuenv_${__cuenv_nonce}_s_$__cuenv_name" 0 || return 1
    done
}

__cuenv_apply() {
    case $- in *a*) builtin set +a; builtin local __cuenv_allexport=1 ;; *) builtin local __cuenv_allexport=0 ;; esac
    builtin local __cuenv_trace=0 __cuenv_status
    case $- in *x*) __cuenv_trace=1; builtin set +x ;; esac
    if ! __cuenv_safe_scope; then __cuenv_error; __cuenv_status=1
    elif __cuenv_apply_impl "$@"; then __cuenv_status=0; else __cuenv_status=$?; fi
    if builtin [ "$__cuenv_trace" = 1 ]; then builtin set -x; fi
    if builtin [ "$__cuenv_allexport" = 1 ]; then builtin set -a; fi
    return "$__cuenv_status"
}

__cuenv_run() (
    builtin set +a
    builtin set +x
    __cuenv_safe_scope || { __cuenv_error; exit 1; }
    builtin local __cuenv_saved=${CUENV_MANAGED_VARS-} __cuenv_rest __cuenv_name
    builtin local __cuenv_uuid __cuenv_project __cuenv_names __cuenv_nonce
    __cuenv_validate "$__cuenv_saved" || { __cuenv_error; exit 1; }
    if builtin [ -n "$__cuenv_saved" ]; then
        __cuenv_rest=${__cuenv_saved#v1:}
        __cuenv_uuid=${__cuenv_rest%%:*}; __cuenv_rest=${__cuenv_rest#*:}
        __cuenv_project=${__cuenv_rest%%:*}; __cuenv_names=${__cuenv_rest#*:}
        __cuenv_nonce=${__cuenv_uuid//-/}
        __cuenv_rest=$__cuenv_names
        while :; do
            __cuenv_name=${__cuenv_rest%%,*}
            __cuenv_preflight_target "$__cuenv_name" || { __cuenv_error; exit 1; }
            case "$__cuenv_rest" in *,*) __cuenv_rest=${__cuenv_rest#*,} ;; *) break ;; esac
        done
        __cuenv_rest=$__cuenv_names
        while :; do
            __cuenv_name=${__cuenv_rest%%,*}
            __cuenv_restore_one "$__cuenv_name" || { __cuenv_error; exit 1; }
            case "$__cuenv_rest" in *,*) __cuenv_rest=${__cuenv_rest#*,} ;; *) break ;; esac
        done
        CUENV_MANAGED_VARS=$__cuenv_saved CUENV_SHELL_INTEGRATION=1 CUENV_HOOK_SESSION=$__cuenv_session "$__cuenv_binary" "$@"
    else
        CUENV_SHELL_INTEGRATION=1 CUENV_HOOK_SESSION=$__cuenv_session "$__cuenv_binary" "$@"
    fi
)

__cuenv_produce() {
    __cuenv_run export --shell "$__cuenv_shell"
}

__cuenv_needs_session() {
    while builtin [ "$#" -gt 0 ]; do
        case "$1" in
            -L|--level|-e|--env) builtin [ "$#" -ge 2 ] || return 1; shift 2 ;;
            --level=*|--env=*|-L?*|-e?*|--json) shift ;;
            env) shift; case "${1-}" in load|status|inspect|check) return 0 ;; *) return 1 ;; esac ;;
            export) return 0 ;;
            *) return 1 ;;
        esac
    done
    return 1
}

__cuenv_install_wrapper() {
    function cuenv {
        if builtin [ "$#" = 3 ] && builtin [ "$1" = shell ] && builtin [ "$2" = session ] && builtin [ "$3" = --current ]; then
            builtin printf '%s\n' "$__cuenv_session"
            return
        fi
        if builtin [ "$#" = 2 ] && builtin [ "$1" = shell ] && builtin [ "$2" = refresh ]; then
            if builtin [ "${__cuenv_owner_pid-}" != "$$" ] || builtin [ "${__cuenv_owner_level-}" != "${BASH_SUBSHELL-${ZSH_SUBSHELL-0}}" ]; then
                __cuenv_error
                return 1
            fi
            __cuenv_hook
            return
        fi
        if __cuenv_needs_session "$@"; then
            if { builtin [ "${__cuenv_owner_pid-}" != "$$" ] || builtin [ "${__cuenv_owner_level-}" != "${BASH_SUBSHELL-${ZSH_SUBSHELL-0}}" ]; } &&
                builtin [ "${CUENV_HOOK_SESSION-}" != "${__cuenv_session-}" ]; then
                __cuenv_error
                return 1
            fi
            __cuenv_run "$@"
        else
            ( builtin unset CUENV_HOOK_SESSION || exit 1; "$__cuenv_binary" "$@" )
        fi
    }
}

__cuenv_hook() {
    case $- in *a*) builtin set +a; builtin local __cuenv_allexport=1 ;; *) builtin local __cuenv_allexport=0 ;; esac
    builtin local __cuenv_trace=0 __cuenv_script __cuenv_status=0 __cuenv_retry=0 __cuenv_prior
    case $- in *x*) __cuenv_trace=1; builtin set +x ;; esac
    if ! __cuenv_safe_scope; then
        __cuenv_error
        if builtin [ "$__cuenv_trace" = 1 ]; then builtin set -x; fi
        if builtin [ "$__cuenv_allexport" = 1 ]; then builtin set -a; fi
        return 1
    fi
    __cuenv_prior=${CUENV_MANAGED_VARS-}
    if __cuenv_script=$(__cuenv_produce); then
        if builtin eval "$__cuenv_script"; then
            if builtin [ "$__cuenv_retry" = 1 ]; then
                __cuenv_retry=0
                if builtin [ "${CUENV_MANAGED_VARS-}" = "$__cuenv_prior" ]; then
                    __cuenv_error; __cuenv_status=1
                elif __cuenv_script=$(__cuenv_produce); then
                    if builtin eval "$__cuenv_script" && builtin [ "$__cuenv_retry" = 0 ]; then :; else __cuenv_error; __cuenv_status=1; fi
                else __cuenv_status=1; fi
            fi
        else __cuenv_status=1; fi
    else __cuenv_status=1; fi
    if builtin [ "$__cuenv_trace" = 1 ]; then builtin set -x; fi
    if builtin [ "$__cuenv_allexport" = 1 ]; then builtin set -a; fi
    return "$__cuenv_status"
}
"#;
