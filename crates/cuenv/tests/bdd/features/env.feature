# Related Decisions
# - RFC-0005 Environment Loading UX
# - ADR-0004 Secret Resolution Architecture

Feature: Environment command behaviors
  As a developer using cuenv
  I want environment variables to be loaded correctly
  So that my development environment works as expected

  Background:
    Given cuenv is installed and available

  Scenario: Environment print shows variables
    Given a project with environment variables:
      | name        | value              |
      | API_URL     | https://api.test   |
      | DEBUG       | true               |
    When I run "cuenv env print"
    Then the output should contain "API_URL"
    And the output should contain "https://api.test"
    And the exit code should be 0

  Scenario: Environment print with JSON format
    Given a project with environment variables:
      | name    | value |
      | MY_VAR  | hello |
    When I run "cuenv env print --output-format json"
    Then the output should contain "MY_VAR"
    And the output should be valid JSON
    And the exit code should be 0

  Scenario: Environment variables are inherited from base
    Given a project with base environment "MY_BASE=base_value"
    And a derived environment "dev" with "DEV_VAR=dev_value"
    When I run "cuenv env print --environment dev"
    Then the output should contain "MY_BASE"
    And the output should contain "DEV_VAR"
    And the exit code should be 0

  Scenario: Environment with missing required variable fails gracefully
    Given a project with no environment variables
    When I run "cuenv env print"
    Then the exit code should be 0
