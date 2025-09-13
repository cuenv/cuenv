Feature: Shell Integration with Preexec Hooks
  As a developer
  I want hooks to execute in the background and load environment variables automatically
  So that my development environment is configured without manual intervention

  Background:
    Given cuenv is installed and available

  Scenario: Preexec hook loads environment on next command with shell integration
    Given shell integration is configured with preexec hooks
    And I am in a directory with env.cue containing environment variables
    When I run "cuenv env load --path ."
    And hooks complete in the background
    And I run any command to trigger preexec
    Then environment variables should be loaded into my shell
    And the preexec hook should have self-unloaded

  Scenario: Standalone env load without shell integration provides eval script
    Given shell integration is NOT configured
    And I am in a directory with env.cue containing environment variables
    When I run "cuenv env load --path ."
    Then I should see instructions to eval a preexec hook
    And the output should contain an eval-able script
    When I eval the provided preexec hook script
    And hooks complete in the background
    And I run any command
    Then environment variables should be loaded into my shell

  Scenario: Sequential hook execution order is maintained
    Given I have an env.cue with multiple hooks:
      """
      hooks: {
        onEnter: [
          {command: "echo", args: ["First hook"]},
          {command: "echo", args: ["Second hook"]},
          {command: "echo", args: ["Third hook"]}
        ]
      }
      """
    When hooks are executed
    Then they should run sequentially in order
    And "First hook" should complete before "Second hook" starts
    And "Second hook" should complete before "Third hook" starts

  Scenario: Preexec hook checks on every command until environment loads
    Given shell integration is configured with preexec hooks
    And I am in a directory with env.cue
    When I run "cuenv env load --path ."
    And hooks are still running in background
    And I run "echo 'first command'"
    Then preexec should check but not load environment yet
    When I run "echo 'second command'"
    Then preexec should check but not load environment yet
    When hooks complete in the background
    And I run "echo 'third command'"
    Then environment variables should be loaded
    And subsequent commands should not trigger preexec checks

  Scenario: Failed hooks do not load environment but still self-unload
    Given shell integration is configured with preexec hooks
    And I have an env.cue with a failing hook:
      """
      env: {
        TEST_VAR: "should_not_load"
      }
      hooks: {
        onEnter: [{command: "false"}]
      }
      """
    When I run "cuenv env load --path ."
    And hooks fail during execution
    And I run any command to trigger preexec
    Then environment variables should NOT be loaded
    And the preexec hook should have self-unloaded
    And TEST_VAR should not be set

  Scenario: Fish shell integration with fish_preexec event
    Given I am using Fish shell
    And shell integration is configured
    When I enter a directory with env.cue
    Then a fish_preexec event handler should be registered
    When hooks complete and I run a command
    Then environment is loaded and handler is unregistered
    And "functions __cuenv_check_hooks" should return an error

  Scenario: Bash shell integration with DEBUG trap
    Given I am using Bash shell
    And shell integration is configured
    When I enter a directory with env.cue
    Then a DEBUG trap should be set for preexec
    When hooks complete and I run a command
    Then environment is loaded and DEBUG trap is removed
    And "trap -p DEBUG" should show no cuenv handler

  Scenario: Zsh shell integration with preexec hook
    Given I am using Zsh shell
    And shell integration is configured
    When I enter a directory with env.cue
    Then a preexec hook should be added
    When hooks complete and I run a command
    Then environment is loaded and preexec hook is removed
    And the hook should no longer be in the preexec array

  Scenario: Environment variables persist after loading
    Given shell integration is configured
    And I load environment with variables:
      """
      env: {
        CUENV_TEST: "persistent_value"
        API_KEY: "secret123"
      }
      """
    When hooks complete and environment is loaded
    Then CUENV_TEST should equal "persistent_value"
    And API_KEY should equal "secret123"
    When I run multiple subsequent commands
    Then the variables should remain set
    And no additional hook checks should occur

  Scenario: Changing directories with pending hooks
    Given shell integration is configured
    And I start loading hooks in directory A
    When I change to directory B before hooks complete
    Then the pending hooks for directory A should be cancelled
    And directory B hooks should start if env.cue exists
    And preexec should track directory B instead