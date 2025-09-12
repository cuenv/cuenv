//! Shell integration command handlers
//!
//! This module provides commands for generating shell integration scripts
//! that enable automatic hook execution on directory changes.

use crate::cli::ShellType;
use cuenv_core::Result;

/// Execute shell init command to output integration script
pub async fn execute_shell_init(shell: ShellType) -> Result<String> {
    let script = match shell {
        ShellType::Fish => generate_fish_script(),
        ShellType::Bash => generate_bash_script(),
        ShellType::Zsh => generate_zsh_script(),
    };
    
    Ok(script)
}

/// Generate Fish shell integration script
fn generate_fish_script() -> String {
    r#"# cuenv Fish shell integration
# Add this to your ~/.config/fish/config.fish

function __cuenv_auto_load --on-variable PWD
    if test -f "$PWD/env.cue"
        cuenv env load --path "$PWD" 2>/dev/null
    end
end

# Initialize for current directory
__cuenv_auto_load
"#.to_string()
}

/// Generate Bash shell integration script
fn generate_bash_script() -> String {
    r#"# cuenv Bash shell integration
# Add this to your ~/.bashrc

__cuenv_auto_load() {
    if [[ -f "$PWD/env.cue" ]]; then
        cuenv env load --path "$PWD" 2>/dev/null || true
    fi
}

# Set up directory change hook
if [[ "${BASH_VERSION:-}" ]]; then
    __cuenv_old_pwd="$PWD"
    __cuenv_check_pwd() {
        if [[ "$PWD" != "$__cuenv_old_pwd" ]]; then
            __cuenv_old_pwd="$PWD"
            __cuenv_auto_load
        fi
    }
    
    # Hook into PROMPT_COMMAND
    if [[ -z "${PROMPT_COMMAND:-}" ]]; then
        PROMPT_COMMAND="__cuenv_check_pwd"
    else
        PROMPT_COMMAND="__cuenv_check_pwd; $PROMPT_COMMAND"
    fi
fi

# Initialize for current directory
__cuenv_auto_load
"#.to_string()
}

/// Generate Zsh shell integration script
fn generate_zsh_script() -> String {
    r#"# cuenv Zsh shell integration  
# Add this to your ~/.zshrc

__cuenv_auto_load() {
    if [[ -f "$PWD/env.cue" ]]; then
        cuenv env load --path "$PWD" 2>/dev/null || true
    fi
}

# Set up directory change hook
autoload -U add-zsh-hook
add-zsh-hook chpwd __cuenv_auto_load

# Initialize for current directory
__cuenv_auto_load
"#.to_string()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[tokio::test]
    async fn test_execute_shell_init_fish() {
        let result = execute_shell_init(ShellType::Fish).await.unwrap();
        assert!(result.contains("function __cuenv_auto_load"));
        assert!(result.contains("--on-variable PWD"));
        assert!(result.contains("cuenv env load"));
    }

    #[tokio::test]
    async fn test_execute_shell_init_bash() {
        let result = execute_shell_init(ShellType::Bash).await.unwrap();
        assert!(result.contains("__cuenv_auto_load()"));
        assert!(result.contains("PROMPT_COMMAND"));
        assert!(result.contains("cuenv env load"));
    }

    #[tokio::test]
    async fn test_execute_shell_init_zsh() {
        let result = execute_shell_init(ShellType::Zsh).await.unwrap();
        assert!(result.contains("__cuenv_auto_load()"));
        assert!(result.contains("add-zsh-hook chpwd"));
        assert!(result.contains("cuenv env load"));
    }

    #[test]
    fn test_generate_fish_script() {
        let script = generate_fish_script();
        assert!(script.contains("Fish shell integration"));
        assert!(script.contains("function __cuenv_auto_load"));
        assert!(script.contains("test -f \"$PWD/env.cue\""));
    }

    #[test]
    fn test_generate_bash_script() {
        let script = generate_bash_script();
        assert!(script.contains("Bash shell integration"));
        assert!(script.contains("__cuenv_auto_load()"));
        assert!(script.contains("[[ -f \"$PWD/env.cue\" ]]"));
    }

    #[test]
    fn test_generate_zsh_script() {
        let script = generate_zsh_script();
        assert!(script.contains("Zsh shell integration"));
        assert!(script.contains("__cuenv_auto_load()"));
        assert!(script.contains("add-zsh-hook chpwd"));
    }
}