//! Shell integration commands for cuenv

use cuenv_core::Result;

/// Execute shell init command
pub fn execute_shell_init(shell: &str) -> Result<String> {
    let script = match shell {
        "fish" => generate_fish_script(),
        "bash" => generate_bash_script(),
        "zsh" => generate_zsh_script(),
        _ => {
            return Err(cuenv_core::Error::configuration(format!(
                "Unsupported shell: {shell}. Supported shells are: fish, bash, zsh"
            )));
        }
    };

    Ok(script)
}

/// Generate Fish shell integration script
fn generate_fish_script() -> String {
    r#"# cuenv Fish shell integration
# Add this to your ~/.config/fish/config.fish

function __cuenv_on_pwd_change --on-variable PWD
    if test -f "$PWD/env.cue"
        cuenv env load 2>/dev/null &
        disown
    end
end

# Initialize for current directory
if test -f "$PWD/env.cue"
    cuenv env load 2>/dev/null &
    disown
end
"#
    .to_string()
}

/// Generate Bash shell integration script
fn generate_bash_script() -> String {
    r#"# cuenv Bash shell integration
# Add this to your ~/.bashrc or ~/.bash_profile

__cuenv_on_pwd_change() {
    if [[ -f "$PWD/env.cue" ]]; then
        cuenv env load 2>/dev/null &
        disown
    fi
}

# Set up the prompt command
if [[ -z "$PROMPT_COMMAND" ]]; then
    PROMPT_COMMAND="__cuenv_on_pwd_change"
else
    PROMPT_COMMAND="$PROMPT_COMMAND; __cuenv_on_pwd_change"
fi

# Initialize for current directory
if [[ -f "$PWD/env.cue" ]]; then
    cuenv env load 2>/dev/null &
    disown
fi
"#
    .to_string()
}

/// Generate Zsh shell integration script
fn generate_zsh_script() -> String {
    r#"# cuenv Zsh shell integration
# Add this to your ~/.zshrc

__cuenv_on_pwd_change() {
    if [[ -f "$PWD/env.cue" ]]; then
        cuenv env load 2>/dev/null &
        disown
    fi
}

# Hook into directory change
autoload -U add-zsh-hook
add-zsh-hook chpwd __cuenv_on_pwd_change

# Initialize for current directory
if [[ -f "$PWD/env.cue" ]]; then
    cuenv env load 2>/dev/null &
    disown
fi
"#
    .to_string()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_shell_init_fish() {
        let result = execute_shell_init("fish").unwrap();
        assert!(result.contains("cuenv Fish shell integration"));
        assert!(result.contains("__cuenv_on_pwd_change"));
        assert!(result.contains("env.cue"));
    }

    #[test]
    fn test_shell_init_bash() {
        let result = execute_shell_init("bash").unwrap();
        assert!(result.contains("cuenv Bash shell integration"));
        assert!(result.contains("PROMPT_COMMAND"));
        assert!(result.contains("env.cue"));
    }

    #[test]
    fn test_shell_init_zsh() {
        let result = execute_shell_init("zsh").unwrap();
        assert!(result.contains("cuenv Zsh shell integration"));
        assert!(result.contains("add-zsh-hook"));
        assert!(result.contains("chpwd"));
    }

    #[test]
    fn test_shell_init_unsupported() {
        let result = execute_shell_init("unsupported");
        assert!(result.is_err());
        if let Err(e) = result {
            let error_str = e.to_string();
            assert!(error_str.contains("Unsupported shell"));
        }
    }
}
