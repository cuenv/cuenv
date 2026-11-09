use crate::cli::ShellType;

pub(super) fn generate_integration(shell: ShellType) -> String {
    match shell {
        ShellType::Fish => generate_fish_integration(),
        ShellType::Bash => generate_posix_integration("bash"),
        ShellType::Zsh => generate_posix_integration("zsh"),
    }
}

fn generate_posix_integration(shell: &str) -> String {
    let mut script = String::from("# cuenv shell integration\n");
    script.push_str(super::shell_posix::RUNTIME);
    script.push_str(
        r#"
__cuenv_initialize() {
    case $- in *a*) builtin set +a; builtin local __cuenv_allexport=1 ;; *) builtin local __cuenv_allexport=0 ;; esac
    builtin local __cuenv_trace=0 __cuenv_bin __cuenv_new_session __cuenv_result=0 __cuenv_existing=
    builtin local __cuenv_level=${BASH_SUBSHELL-${ZSH_SUBSHELL-0}}
    case $- in *x*) __cuenv_trace=1; builtin set +x ;; esac
    if builtin alias cuenv >/dev/null 2>&1; then
        builtin printf '%s\n' 'cuenv shell integration cannot replace an existing cuenv alias' >&2
        __cuenv_result=1
    elif builtin [ "$1" = zsh ]; then
        case "$(builtin whence -w cuenv)" in 'cuenv: function') __cuenv_existing=1 ;; esac
    elif builtin [ "$(builtin type -t cuenv)" = function ]; then
        __cuenv_existing=1
    fi
    if builtin [ "$__cuenv_existing" = 1 ] && builtin [ "${__cuenv_wrapper_installed-}" != 1 ]; then
        builtin printf '%s\n' 'cuenv shell integration cannot replace an existing cuenv function' >&2
        __cuenv_result=1
    fi
    if builtin [ "$__cuenv_result" = 0 ] && builtin [ "$__cuenv_existing" = 1 ] &&
        builtin [ "${__cuenv_owner_pid-}" = "$$" ] && builtin [ "${__cuenv_owner_level-}" = "$__cuenv_level" ]; then
        if builtin [ "$__cuenv_trace" = 1 ]; then builtin set -x; fi
        if builtin [ "$__cuenv_allexport" = 1 ]; then builtin set -a; fi
        return 0
    fi
    if builtin [ "$1" = zsh ]; then
        __cuenv_bin=$(builtin whence -p cuenv)
    else
        __cuenv_bin=$(builtin type -P cuenv)
    fi
    if builtin [ "$__cuenv_result" != 0 ]; then
        :
    elif builtin [ -z "$__cuenv_bin" ]; then
        __cuenv_error; __cuenv_result=1
    else
        case "$__cuenv_bin" in /*) ;; *) __cuenv_bin=$PWD/$__cuenv_bin ;; esac
        if __cuenv_new_session=$("$__cuenv_bin" shell session) && builtin [ -n "$__cuenv_new_session" ]; then
            __cuenv_put __cuenv_binary "$__cuenv_bin" &&
                __cuenv_put __cuenv_session "$__cuenv_new_session" &&
                __cuenv_put __cuenv_shell "$1" &&
                __cuenv_put __cuenv_owner_pid "$$" &&
                __cuenv_put __cuenv_owner_level "$__cuenv_level" &&
                __cuenv_put __cuenv_wrapper_installed 1 &&
                __cuenv_install_wrapper || __cuenv_result=1
        else
            __cuenv_error; __cuenv_result=1
        fi
    fi
    if builtin [ "$__cuenv_trace" = 1 ]; then builtin set -x; fi
    if builtin [ "$__cuenv_allexport" = 1 ]; then builtin set -a; fi
    return "$__cuenv_result"
}
"#,
    );
    match shell {
        "bash" => script.push_str(
            r#"
if __cuenv_initialize bash; then
    case ";${PROMPT_COMMAND-};" in
        *';__cuenv_hook;'*) ;;
        *) PROMPT_COMMAND="__cuenv_hook${PROMPT_COMMAND:+; $PROMPT_COMMAND}" ;;
    esac
else
    false
fi
"#,
        ),
        "zsh" => script.push_str(
            r"
if __cuenv_initialize zsh; then
    autoload -U add-zsh-hook
    add-zsh-hook -d precmd __cuenv_hook 2>/dev/null
    add-zsh-hook precmd __cuenv_hook
else
    false
fi
",
        ),
        _ => unreachable!("only Bash and Zsh use the POSIX integration"),
    }
    script
}

fn generate_fish_integration() -> String {
    let mut script = String::from("# cuenv Fish shell integration\n");
    script.push_str(super::shell_fish::RUNTIME);
    script.push_str(
        r#"
function __cuenv_initialize
    set -l __cuenv_trace_global 0
    set -l __cuenv_trace
    if set -q -g fish_trace; set __cuenv_trace_global 1; set __cuenv_trace $fish_trace; end
    set -g fish_trace
    if functions -q cuenv
        if test "$__cuenv_wrapper_installed" = 1; and test "$__cuenv_owner_pid" = "$fish_pid"
            if test $__cuenv_trace_global = 1; set -g fish_trace $__cuenv_trace; else; set -e -g fish_trace; end
            return 0
        end
        builtin printf '%s\n' 'cuenv shell integration cannot replace an existing cuenv function or alias' >&2
        if test $__cuenv_trace_global = 1; set -g fish_trace $__cuenv_trace; else; set -e -g fish_trace; end
        return 1
    end
    set -l __cuenv_bin (command --search cuenv)
    set -l __cuenv_result 0
    if test -z "$__cuenv_bin"
        __cuenv_error
        set __cuenv_result 1
    else
        if not string match -q '/*' -- "$__cuenv_bin"
            set __cuenv_bin "$PWD/$__cuenv_bin"
        end
        set -l __cuenv_new_session ("$__cuenv_bin" shell session | string collect)
        set -l __cuenv_pipeline $pipestatus
        if test "$__cuenv_pipeline[1]" = 0; and test "$__cuenv_pipeline[2]" = 0; and string match -qr '^[0-9a-f]{8}-[0-9a-f]{4}-4[0-9a-f]{3}-[89ab][0-9a-f]{3}-[0-9a-f]{12}$' -- "$__cuenv_new_session"
            set -g -u __cuenv_binary "$__cuenv_bin"
            set -g -u __cuenv_session "$__cuenv_new_session"
            set -g -u __cuenv_owner_pid "$fish_pid"
            set -g -u __cuenv_wrapper_installed 1
            __cuenv_install_wrapper
        else
            __cuenv_error
            set __cuenv_result 1
        end
    end
    if test $__cuenv_trace_global = 1; set -g fish_trace $__cuenv_trace; else; set -e -g fish_trace; end
    return $__cuenv_result
end

if __cuenv_initialize
    __cuenv_hook
else
    false
end
"#,
    );
    script
}
