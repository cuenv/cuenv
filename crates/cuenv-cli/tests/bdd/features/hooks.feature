Feature: Background Hook Execution with Environment Loading
  As a developer
  I want hooks to execute in the background when I enter a directory
  And have the environment variables loaded into my shell when hooks complete
  So that my development environment is automatically configured

  Background:
    Given cuenv is installed and available
    And the shell integration is configured

  Scenario: Background Hooks Load into Active Shell
    Given I am in the "examples" directory
    And cuenv is allowed in "examples/hook" directory
    When I change directory to "examples/hook"
    Then hooks should be spawned in the background
    When I wait for hooks to complete
    Then the environment variables should be loaded in my shell
    When I execute "echo $CUENV_TEST"
    Then I should see "loaded_successfully"

  Scenario: Hook Execution Status Check
    Given I am in the "examples" directory
    And cuenv is allowed in "examples/hook" directory
    When I change directory to "examples/hook"
    Then hooks should be spawned in the background
    When I check the hook execution status
    Then I should see hooks are running
    When I wait for hooks to complete
    And I check the hook execution status again
    Then I should see hooks have completed successfully

  Scenario: Environment Variables Persist After Hook Completion
    Given I am in the "examples" directory
    And cuenv is allowed in "examples/hook" directory
    When I change directory to "examples/hook"
    And I wait for hooks to complete
    Then the environment variable "CUENV_TEST" should equal "loaded_successfully"
    And the environment variable "API_ENDPOINT" should equal "http://localhost:8080/api"
    When I execute a command that uses these variables
    Then the command should have access to the loaded environment

  Scenario: Failed Hooks Do Not Load Environment
    Given I am in the "examples" directory
    And cuenv is allowed in "hook-failure" directory with failing hooks
    When I change directory to "hook-failure"
    And I wait for hooks to complete or fail
    Then the environment variables should not be loaded
    And I should see an error message about hook failure

  Scenario: Changing Away From Directory Preserves State
    Given I am in the "examples/hook" directory with completed hooks
    When I change directory to "../"
    Then the environment variables from hooks should still be set
    When I change back to "examples/hook"
    Then hooks should not re-execute since configuration hasn't changed