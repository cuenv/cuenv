//! Fish shell-local environment restoration helpers.

pub(super) const RUNTIME: &str = r####"
function __cuenv_error
    builtin printf '%s\n' 'cuenv cannot restore this shell environment; start a fresh shell or explicitly clear the inherited environment' >&2
    return 1
end

function __cuenv_valid_name -a __cuenv_name
    string match -qr '^[A-Za-z_][A-Za-z0-9_]*$' -- "$__cuenv_name"; or return 1
    set -l __cuenv_upper (string upper -- "$__cuenv_name")
    switch $__cuenv_upper
        case '__CUENV_*' 'BASH_*' 'ZSH_*' 'FISH_*' CUENV_HOOK_SESSION CUENV_MANAGED_VARS CUENV_SHELL_INTEGRATION CUENV_LOADED_DIR CUENV_PENDING_APPROVAL_DIR PS1 PS2 PS3 PS4 ENV SHELLOPTS BASHOPTS BASHPID ZDOTDIR PWD OLDPWD LINENO SECONDS RANDOM SRANDOM EPOCHSECONDS EPOCHREALTIME SHLVL PPID UID EUID GROUPS FUNCNAME ARGV STATUS PIPESTATUS HISTORY VERSION PROMPT_COMMAND OPTIND OPTARG IFS _
            return 1
    end
    return 0
end

function __cuenv_equal -a __cuenv_left_name __cuenv_right_name
    set -l __cuenv_left $$__cuenv_left_name
    set -l __cuenv_right $$__cuenv_right_name
    test (builtin count $__cuenv_left) -eq (builtin count $__cuenv_right); or return 1
    set -l __cuenv_index 1
    while test $__cuenv_index -le (builtin count $__cuenv_left)
        test "$__cuenv_left[$__cuenv_index]" = "$__cuenv_right[$__cuenv_index]"; or return 1
        set __cuenv_index (builtin math $__cuenv_index + 1)
    end
    return 0
end

function __cuenv_can_set -a __cuenv_name
    true
    set -l $__cuenv_name __cuenv_probe 2>/dev/null; or return 1
    return 0
end

function __cuenv_validate -a __cuenv_expected
    test "$CUENV_MANAGED_VARS" = "$__cuenv_expected"; or return 1
    if test -n "$__cuenv_expected"
        set -q -g __cuenv_private_token; or return 1
        test "$__cuenv_private_token" = "$__cuenv_expected"; or return 1
        set -q -x __cuenv_private_token; and return 1
        test (string length -- "$__cuenv_expected") -le 24576; or return 1
        set -l __cuenv_parts (string split ':' -- "$__cuenv_expected")
        test (builtin count $__cuenv_parts) -eq 4; and test "$__cuenv_parts[1]" = v1; or return 1
        string match -qr '^[0-9a-f]{8}-[0-9a-f]{4}-4[0-9a-f]{3}-[89ab][0-9a-f]{3}-[0-9a-f]{12}$' -- "$__cuenv_parts[2]"; or return 1
        string match -qr '^[0-9a-f]{64}$' -- "$__cuenv_parts[3]"; or return 1
        set -l __cuenv_nonce (string replace -a -- '-' '' "$__cuenv_parts[2]")
        set -l __cuenv_seen
        for __cuenv_name in (string split ',' -- "$__cuenv_parts[4]")
            __cuenv_valid_name "$__cuenv_name"; or return 1
            builtin contains -- "$__cuenv_name" $__cuenv_seen; and return 1
            set -a __cuenv_seen "$__cuenv_name"
            for __cuenv_field in p x o v a q s
                set -l __cuenv_key "__cuenv_$__cuenv_nonce"_"$__cuenv_field"_"$__cuenv_name"
                set -q -g $__cuenv_key; or return 1
                set -q -x $__cuenv_key; and return 1
            end
        end
        test (builtin count $__cuenv_seen) -le 1024; or return 1
    else
        set -q -g __cuenv_private_token; and return 1
    end
    return 0
end

function __cuenv_token -a __cuenv_uuid __cuenv_project
    set -e argv[1..2]
    if test (builtin count $argv) -eq 0
        if set -q -g CUENV_MANAGED_VARS; set -e -g CUENV_MANAGED_VARS; end
        if set -q -g __cuenv_private_token; set -e -g __cuenv_private_token; end
    else
        set -l __cuenv_next "v1:$__cuenv_uuid:$__cuenv_project:"(string join ',' -- $argv)
        true
        set -g -u __cuenv_private_token "$__cuenv_next"; or return 1
        set -gx CUENV_MANAGED_VARS "$__cuenv_next"; or return 1
    end
    return 0
end

function __cuenv_restore_one -a __cuenv_nonce __cuenv_name
    set -l __cuenv_prefix "__cuenv_$__cuenv_nonce"_
    set -l __cuenv_p "$__cuenv_prefix"p_"$__cuenv_name"
    set -l __cuenv_x "$__cuenv_prefix"x_"$__cuenv_name"
    set -l __cuenv_o "$__cuenv_prefix"o_"$__cuenv_name"
    set -l __cuenv_v "$__cuenv_prefix"v_"$__cuenv_name"
    set -l __cuenv_a "$__cuenv_prefix"a_"$__cuenv_name"
    set -l __cuenv_q "$__cuenv_prefix"q_"$__cuenv_name"
    set -l __cuenv_s "$__cuenv_prefix"s_"$__cuenv_name"
    if set -q -x $__cuenv_name; and begin; __cuenv_equal "$__cuenv_name" "$__cuenv_a"; or begin; test "$$__cuenv_s" = 1; and __cuenv_equal "$__cuenv_name" "$__cuenv_q"; end; end
        if test "$$__cuenv_p" = 1; and test "$$__cuenv_o" = global
            if test "$$__cuenv_x" = 1
                set -gx $__cuenv_name $$__cuenv_v; or return 1
            else
                true
                set -g -u $__cuenv_name $$__cuenv_v; or return 1
            end
        else
            if set -q -g $__cuenv_name
                set -e -g $__cuenv_name; or return 1
            end
        end
    end
    return 0
end

function __cuenv_cleanup_one -a __cuenv_nonce __cuenv_name
    for __cuenv_field in p x o v a q s
        set -e -g "__cuenv_$__cuenv_nonce"_"$__cuenv_field"_"$__cuenv_name"
    end
    return 0
end

function __cuenv_unload_impl -a __cuenv_expected __cuenv_retry_requested
    __cuenv_validate "$__cuenv_expected"; or begin; __cuenv_error; return 1; end
    test -n "$__cuenv_expected"; or return 0
    set -l __cuenv_parts (string split ':' -- "$__cuenv_expected")
    set -l __cuenv_uuid "$__cuenv_parts[2]"
    set -l __cuenv_project "$__cuenv_parts[3]"
    set -l __cuenv_nonce (string replace -a -- '-' '' "$__cuenv_uuid")
    set -l __cuenv_names (string split ',' -- "$__cuenv_parts[4]")
    for __cuenv_name in CUENV_MANAGED_VARS CUENV_LOADED_DIR CUENV_PENDING_APPROVAL_DIR __cuenv_private_token $__cuenv_names
        set -q -l $__cuenv_name; and begin; __cuenv_error; return 1; end
        __cuenv_can_set "$__cuenv_name"; or begin; __cuenv_error; return 1; end
    end
    for __cuenv_name in $__cuenv_names
        __cuenv_restore_one "$__cuenv_nonce" "$__cuenv_name"; or begin; __cuenv_error; return 1; end
        set -l __cuenv_index (builtin contains -i -- "$__cuenv_name" $__cuenv_names)
        set -e __cuenv_names[$__cuenv_index]
        __cuenv_token "$__cuenv_uuid" "$__cuenv_project" $__cuenv_names; or return 1
        __cuenv_cleanup_one "$__cuenv_nonce" "$__cuenv_name"
    end
    for __cuenv_name in CUENV_LOADED_DIR CUENV_PENDING_APPROVAL_DIR
        if set -q -g $__cuenv_name; set -e -g $__cuenv_name; end
    end
    set -g -u __cuenv_retry "$__cuenv_retry_requested"
end

function __cuenv_unload
    set -l __cuenv_trace_global 0
    set -l __cuenv_trace
    if set -q -g fish_trace
        set __cuenv_trace_global 1
        set __cuenv_trace $fish_trace
    end
    set -g fish_trace
    __cuenv_unload_impl $argv
    set -l __cuenv_result $status
    if test $__cuenv_trace_global = 1; set -g fish_trace $__cuenv_trace; else; set -e -g fish_trace; end
    return $__cuenv_result
end

function __cuenv_matches -a __cuenv_nonce __cuenv_name
    set -l __cuenv_a "__cuenv_$__cuenv_nonce"_a_"$__cuenv_name"
    set -l __cuenv_q "__cuenv_$__cuenv_nonce"_q_"$__cuenv_name"
    set -l __cuenv_s "__cuenv_$__cuenv_nonce"_s_"$__cuenv_name"
    set -q -x $__cuenv_name; or return 1
    __cuenv_equal "$__cuenv_name" "$__cuenv_a"; and return 0
    test "$$__cuenv_s" = 1; and __cuenv_equal "$__cuenv_name" "$__cuenv_q"
end

function __cuenv_pending_value -a __cuenv_key __cuenv_name __cuenv_value
    true
    set -fx $__cuenv_name "$__cuenv_value" 2>/dev/null; or return 1
    set -g -u $__cuenv_key $$__cuenv_name
end

function __cuenv_apply_impl -a __cuenv_expected __cuenv_project __cuenv_uuid
    set -e argv[1..3]
    set -l __cuenv_args $argv
    test (builtin math (builtin count $__cuenv_args) % 2) = 0; or begin; __cuenv_error; return 1; end
    __cuenv_validate "$__cuenv_expected"; or begin; __cuenv_error; return 1; end
    set -l __cuenv_nonce (string replace -a -- '-' '' "$__cuenv_uuid")
    set -l __cuenv_names
    if test -n "$__cuenv_expected"
        set -l __cuenv_parts (string split ':' -- "$__cuenv_expected")
        test "$__cuenv_parts[2]" = "$__cuenv_uuid"; and test "$__cuenv_parts[3]" = "$__cuenv_project"; or begin; __cuenv_error; return 1; end
        set __cuenv_names (string split ',' -- "$__cuenv_parts[4]")
    end
    set -l __cuenv_old_names $__cuenv_names
    set -l __cuenv_union $__cuenv_names
    set -l __cuenv_desired
    for __cuenv_name in CUENV_MANAGED_VARS CUENV_LOADED_DIR CUENV_PENDING_APPROVAL_DIR __cuenv_private_token $__cuenv_names
        set -q -l $__cuenv_name; and begin; __cuenv_error; return 1; end
        __cuenv_can_set "$__cuenv_name"; or begin; __cuenv_error; return 1; end
    end
    set -l __cuenv_index 1
    while test $__cuenv_index -le (builtin count $__cuenv_args)
        set -l __cuenv_name "$__cuenv_args[$__cuenv_index]"
        __cuenv_valid_name "$__cuenv_name"; or begin; __cuenv_error; return 1; end
        set -q -l $__cuenv_name; and begin; __cuenv_error; return 1; end
        __cuenv_can_set "$__cuenv_name"; or begin; __cuenv_error; return 1; end
        builtin contains -- "$__cuenv_name" $__cuenv_desired; and begin; __cuenv_error; return 1; end
        set -a __cuenv_desired "$__cuenv_name"
        if not builtin contains -- "$__cuenv_name" $__cuenv_union
            for __cuenv_field in p x o v a q s
                set -l __cuenv_key "__cuenv_$__cuenv_nonce"_"$__cuenv_field"_"$__cuenv_name"
                set -q $__cuenv_key; and begin; __cuenv_error; return 1; end
            end
            set -a __cuenv_union "$__cuenv_name"
        end
        set __cuenv_index (builtin math $__cuenv_index + 2)
    end
    set -l __cuenv_union_token (string join ',' -- $__cuenv_union | string collect -N)
    test (builtin count $__cuenv_union) -le 1024; and test (string length -- "$__cuenv_union_token") -le 24400; or begin; __cuenv_error; return 1; end
    for __cuenv_name in $__cuenv_old_names
        if not builtin contains -- "$__cuenv_name" $__cuenv_desired
            __cuenv_restore_one "$__cuenv_nonce" "$__cuenv_name"; or begin; __cuenv_error; return 1; end
            set -l __cuenv_index (builtin contains -i -- "$__cuenv_name" $__cuenv_names)
            set -e __cuenv_names[$__cuenv_index]
            __cuenv_token "$__cuenv_uuid" "$__cuenv_project" $__cuenv_names; or return 1
            __cuenv_cleanup_one "$__cuenv_nonce" "$__cuenv_name"
        end
    end
    set __cuenv_index 1
    while test $__cuenv_index -le (builtin count $__cuenv_args)
        set -l __cuenv_name "$__cuenv_args[$__cuenv_index]"
        set -l __cuenv_next_index (builtin math $__cuenv_index + 1)
        set -l __cuenv_value "$__cuenv_args[$__cuenv_next_index]"
        set -l __cuenv_prefix "__cuenv_$__cuenv_nonce"_
        set -l __cuenv_p "$__cuenv_prefix"p_"$__cuenv_name"
        set -l __cuenv_x "$__cuenv_prefix"x_"$__cuenv_name"
        set -l __cuenv_o "$__cuenv_prefix"o_"$__cuenv_name"
        set -l __cuenv_v "$__cuenv_prefix"v_"$__cuenv_name"
        set -l __cuenv_a "$__cuenv_prefix"a_"$__cuenv_name"
        set -l __cuenv_q "$__cuenv_prefix"q_"$__cuenv_name"
        set -l __cuenv_s "$__cuenv_prefix"s_"$__cuenv_name"
        set -l __cuenv_present 0; set -q $__cuenv_name; and set __cuenv_present 1
        set -l __cuenv_exported 0; set -q -x $__cuenv_name; and set __cuenv_exported 1
        set -l __cuenv_scope absent
        if set -q -g $__cuenv_name; set __cuenv_scope global; else if set -q -U $__cuenv_name; set __cuenv_scope universal; end
        set -l __cuenv_current $$__cuenv_name
        set -l __cuenv_changed 1
        if builtin contains -- "$__cuenv_name" $__cuenv_old_names; and __cuenv_matches "$__cuenv_nonce" "$__cuenv_name"
            set __cuenv_changed 0
        end
        if test $__cuenv_changed = 1
            set -g -u $__cuenv_p "$__cuenv_present"
            set -g -u $__cuenv_x "$__cuenv_exported"
            set -g -u $__cuenv_o "$__cuenv_scope"
            set -g -u $__cuenv_v $__cuenv_current
        end
        if not builtin contains -- "$__cuenv_name" $__cuenv_old_names
            set -g -u $__cuenv_a $__cuenv_current
            __cuenv_pending_value "$__cuenv_q" "$__cuenv_name" "$__cuenv_value"; or return 1
            set -g -u $__cuenv_s 1
            set -a __cuenv_names "$__cuenv_name"
            __cuenv_token "$__cuenv_uuid" "$__cuenv_project" $__cuenv_names; or return 1
        end
        __cuenv_pending_value "$__cuenv_q" "$__cuenv_name" "$__cuenv_value"; or return 1
        set -g -u $__cuenv_s 1
        set -gx $__cuenv_name "$__cuenv_value"; or return 1
        set -g -u $__cuenv_a $$__cuenv_name
        set -g -u $__cuenv_s 0
        set __cuenv_index (builtin math $__cuenv_index + 2)
    end
    return 0
end

function __cuenv_apply
    set -l __cuenv_trace_global 0
    set -l __cuenv_trace
    if set -q -g fish_trace; set __cuenv_trace_global 1; set __cuenv_trace $fish_trace; end
    set -g fish_trace
    __cuenv_apply_impl $argv
    set -l __cuenv_result $status
    if test $__cuenv_trace_global = 1; set -g fish_trace $__cuenv_trace; else; set -e -g fish_trace; end
    return $__cuenv_result
end

function __cuenv_run_impl
    set -l __cuenv_expected "$CUENV_MANAGED_VARS"
    __cuenv_validate "$__cuenv_expected"; or begin; __cuenv_error; return 1; end
    if test -n "$__cuenv_expected"
        set -l __cuenv_parts (string split ':' -- "$__cuenv_expected")
        set -l __cuenv_nonce (string replace -a -- '-' '' "$__cuenv_parts[2]")
        set -l __cuenv_names (string split ',' -- "$__cuenv_parts[4]")
        for __cuenv_name in $__cuenv_names
            set -q -l $__cuenv_name; and begin; __cuenv_error; return 1; end
            __cuenv_can_set "$__cuenv_name"; or begin; __cuenv_error; return 1; end
        end
        for __cuenv_name in $__cuenv_names
            if __cuenv_matches "$__cuenv_nonce" "$__cuenv_name"
                set -l __cuenv_p "__cuenv_$__cuenv_nonce"_p_"$__cuenv_name"
                set -l __cuenv_x "__cuenv_$__cuenv_nonce"_x_"$__cuenv_name"
                set -l __cuenv_v "__cuenv_$__cuenv_nonce"_v_"$__cuenv_name"
                if test "$$__cuenv_p" = 1; and test "$$__cuenv_x" = 1
                    set -fx $__cuenv_name $$__cuenv_v
                else if test "$$__cuenv_p" = 1
                    set -f -u $__cuenv_name $$__cuenv_v
                else
                    set -f -u $__cuenv_name
                end
            end
        end
    end
    set -fx CUENV_SHELL_INTEGRATION 1
    set -fx CUENV_HOOK_SESSION "$__cuenv_session"
    "$__cuenv_binary" $argv
end

function __cuenv_run
    set -l __cuenv_trace_global 0
    set -l __cuenv_trace
    if set -q -g fish_trace; set __cuenv_trace_global 1; set __cuenv_trace $fish_trace; end
    set -g fish_trace
    __cuenv_run_impl $argv
    set -l __cuenv_result $status
    if test $__cuenv_trace_global = 1; set -g fish_trace $__cuenv_trace; else; set -e -g fish_trace; end
    return $__cuenv_result
end

function __cuenv_produce
    __cuenv_run export --shell fish
end

function __cuenv_needs_session
    set -l __cuenv_args $argv
    while test (builtin count $__cuenv_args) -gt 0
        switch "$__cuenv_args[1]"
            case -L --level -e --env
                test (builtin count $__cuenv_args) -ge 2; or return 1
                set -e __cuenv_args[1..2]
            case '--level=*' '--env=*' '-L?*' '-e?*' --json
                set -e __cuenv_args[1]
            case env
                set -e __cuenv_args[1]
                switch "$__cuenv_args[1]"
                    case load status inspect check
                        return 0
                    case '*'
                        return 1
                end
            case export
                return 0
            case '*'
                return 1
        end
    end
    return 1
end

function __cuenv_install_wrapper
    function cuenv
        if test (builtin count $argv) = 3; and test "$argv[1]" = shell; and test "$argv[2]" = session; and test "$argv[3]" = --current
            builtin printf '%s\n' "$__cuenv_session"
            return
        end
        if test (builtin count $argv) = 2; and test "$argv[1]" = shell; and test "$argv[2]" = refresh
            if test "$__cuenv_owner_pid" != "$fish_pid"; __cuenv_error; return 1; end
            __cuenv_hook
            return
        end
        if __cuenv_needs_session $argv
            if test "$__cuenv_owner_pid" != "$fish_pid"
                __cuenv_error
                return 1
            end
            __cuenv_run $argv
        else
            set -f -u CUENV_HOOK_SESSION
            "$__cuenv_binary" $argv
        end
    end
end

function __cuenv_hook --on-variable PWD --on-event fish_prompt
    set -l __cuenv_trace_global 0
    set -l __cuenv_trace
    if set -q -g fish_trace; set __cuenv_trace_global 1; set __cuenv_trace $fish_trace; end
    set -g fish_trace
    set -l __cuenv_prior "$CUENV_MANAGED_VARS"
    set -g -u __cuenv_retry 0
    set -l __cuenv_script (__cuenv_produce | string collect -N)
    set -l __cuenv_pipeline $pipestatus
    set -l __cuenv_result 0
    if test "$__cuenv_pipeline[1]" = 0; and test "$__cuenv_pipeline[2]" = 0
        builtin printf '%s' "$__cuenv_script" | builtin source
        set __cuenv_pipeline $pipestatus
        if test "$__cuenv_pipeline[1]" != 0; or test "$__cuenv_pipeline[2]" != 0
            set __cuenv_result 1
        else if test "$__cuenv_retry" = 1
            set -g -u __cuenv_retry 0
            if test "$CUENV_MANAGED_VARS" = "$__cuenv_prior"
                __cuenv_error
                set __cuenv_result 1
            else
                set __cuenv_script (__cuenv_produce | string collect -N)
                set __cuenv_pipeline $pipestatus
                if test "$__cuenv_pipeline[1]" = 0; and test "$__cuenv_pipeline[2]" = 0
                    builtin printf '%s' "$__cuenv_script" | builtin source
                    set __cuenv_pipeline $pipestatus
                    if test "$__cuenv_pipeline[1]" != 0; or test "$__cuenv_pipeline[2]" != 0; or test "$__cuenv_retry" != 0
                        __cuenv_error
                        set __cuenv_result 1
                    end
                else
                    set __cuenv_result 1
                end
            end
        end
    else
        set __cuenv_result 1
    end
    set -e -g __cuenv_retry
    if test $__cuenv_trace_global = 1; set -g fish_trace $__cuenv_trace; else; set -e -g fish_trace; end
    return $__cuenv_result
end
"####;
