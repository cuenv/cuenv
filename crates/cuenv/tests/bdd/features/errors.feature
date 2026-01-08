# Related Decisions
# - RFC-0001 CLI Command Surface and Subcommand Hierarchy
# - ADR-0002 Error Handling Strategy

Feature: Error handling behaviors
  As a developer using cuenv
  I want clear and actionable error messages
  So that I can quickly identify and fix configuration problems

  Background:
    Given cuenv is installed and available

  Scenario: Invalid CUE syntax produces helpful error
    Given a project with invalid CUE syntax
    When I run "cuenv env print"
    Then the exit code should not be 0
    And the output should contain "error"

  Scenario: Missing task dependency produces clear error
    Given a project with tasks:
      | name     | command | depends_on   |
      | deploy   | echo    | [missing]    |
    When I run "cuenv task deploy"
    Then the exit code should not be 0
    And the output should contain "missing"

  Scenario: Unknown task name produces helpful error
    Given a project with tasks:
      | name  | command | depends_on |
      | build | echo    | []         |
    When I run "cuenv task nonexistent"
    Then the exit code should not be 0
    And the output should contain "not found"

  Scenario: Empty project is handled gracefully
    Given a project with no tasks or environment
    When I run "cuenv task"
    Then the exit code should be 0

