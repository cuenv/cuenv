# Related Decisions
# - RFC-0001 CLI Command Surface and Subcommand Hierarchy
# - RFC-0004 Task Execution UX and Dependency Strategy
# - ADR-0003 Task Graph Execution Strategy

Feature: Task execution behaviors
  As a developer using cuenv
  I want tasks to execute in the correct order based on dependencies
  So that my build pipeline runs correctly

  Background:
    Given cuenv is installed and available

  Scenario: Task with dependencies executes in correct order
    Given a project with tasks:
      | name     | command | depends_on |
      | compile  | echo    | []         |
      | build    | echo    | [compile]  |
    When I run "cuenv task build"
    Then the task "compile" should complete before "build"
    And the exit code should be 0

  Scenario: Task failure prevents dependent tasks from running
    Given a project with tasks:
      | name     | command                    | depends_on  |
      | failing  | sh -c 'exit 1'             | []          |
      | depends  | echo "should not run"      | [failing]   |
    When I run "cuenv task depends"
    Then the task "failing" should fail
    And the task "depends" should not execute
    And the exit code should not be 0

  Scenario: Independent tasks can run in parallel
    Given a project with parallel tasks "lint" and "test"
    When I run "cuenv task check"
    Then both "lint" and "test" should execute
    And the exit code should be 0

  Scenario: Task group executes all children
    Given a project with a parallel group "verify" containing "typecheck" and "format"
    When I run "cuenv task verify"
    Then the task "typecheck" should execute
    And the task "format" should execute
    And the exit code should be 0

  Scenario: Task with no dependencies runs immediately
    Given a project with tasks:
      | name  | command        | depends_on |
      | hello | echo "hello"   | []         |
    When I run "cuenv task hello"
    Then the output should contain "hello"
    And the exit code should be 0

  Scenario: List tasks shows available tasks
    Given a project with tasks:
      | name  | command | depends_on |
      | build | echo    | []         |
      | test  | echo    | [build]    |
    When I run "cuenv task --list"
    Then the output should contain "build"
    And the output should contain "test"
