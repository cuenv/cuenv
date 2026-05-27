use crate::cli::ShellType;

pub(super) fn generate_integration(shell: ShellType) -> String {
    match shell {
        ShellType::Fish => generate_fish_integration(),
        ShellType::Bash => generate_bash_integration(),
        ShellType::Zsh => generate_zsh_integration(),
    }
}

fn generate_fish_integration() -> String {
    r"# cuenv Fish shell integration
# Add this to your ~/.config/fish/config.fish

# Mark that shell integration is active
set -x CUENV_SHELL_INTEGRATION 1

# Hook function that loads environment on each prompt
function __cuenv_hook --on-variable PWD
    # The export command handles everything:
    # - Checks if env.cue exists
    # - Loads cached state if available (fast path)
    # - Evaluates CUE only when needed
    # - Starts hooks in background if needed
    # - Returns safe no-op if nothing to do
    source (cuenv export --shell fish 2>/dev/null | psub)
end

# Also run on shell startup
source (cuenv export --shell fish 2>/dev/null | psub)"
        .to_string()
}

fn generate_bash_integration() -> String {
    r#"# cuenv Bash shell integration
# Add this to your ~/.bashrc

# Mark that shell integration is active
export CUENV_SHELL_INTEGRATION=1

# Hook function that loads environment on each prompt
__cuenv_hook() {
    # The export command handles everything:
    # - Checks if env.cue exists
    # - Loads cached state if available (fast path)
    # - Evaluates CUE only when needed
    # - Starts hooks in background if needed
    # - Returns safe no-op if nothing to do
    eval "$(cuenv export --shell bash 2>/dev/null)"
}

# Set up the hook via PROMPT_COMMAND
if [[ -n "$PROMPT_COMMAND" ]]; then
    PROMPT_COMMAND="__cuenv_hook; $PROMPT_COMMAND"
else
    PROMPT_COMMAND="__cuenv_hook"
fi

# Also run on shell startup
__cuenv_hook"#
        .to_string()
}

fn generate_zsh_integration() -> String {
    r#"# cuenv Zsh shell integration
# Add this to your ~/.zshrc

# Mark that shell integration is active
export CUENV_SHELL_INTEGRATION=1

# Hook function that loads environment on each prompt
__cuenv_hook() {
    # The export command handles everything:
    # - Checks if env.cue exists
    # - Loads cached state if available (fast path)
    # - Evaluates CUE only when needed
    # - Starts hooks in background if needed
    # - Returns safe no-op if nothing to do
    eval "$(cuenv export --shell zsh 2>/dev/null)"
}

# Set up the hook via precmd
autoload -U add-zsh-hook
add-zsh-hook precmd __cuenv_hook

# Also run on shell startup
__cuenv_hook"#
        .to_string()
}
