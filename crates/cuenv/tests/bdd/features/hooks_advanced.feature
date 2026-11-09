Feature: Advanced hook execution
  Hooks should handle failures, concurrent sessions, and unusual output safely.

  Background:
    Given cuenv is installed and available
    And the shell integration is configured

  Scenario: Hook timeout stops the direct child and later hooks
    Given an advanced "timeout" hook fixture
    When I run the advanced hook fixture
    Then the direct child times out and the next hook does not run

  Scenario: A fast hook completes before the executor deadline
    Given an advanced "fast" hook fixture
    When I run the advanced hook fixture
    Then the fast hook completes within its deadline

  Scenario: Separate shell sessions serialize execution without sharing results
    Given an advanced "sessions" hook fixture
    When I run the advanced hook fixture
    Then the sessions have separate results and no overlapping hooks

  Scenario: Rapid directory changes retain separate hook states
    Given an advanced "directories" hook fixture
    When I run the advanced hook fixture
    Then both directories complete and the final directory supplies its environment

  Scenario: Multiline environment variables are preserved
    Given an advanced "multiline" hook fixture
    When I run the advanced hook fixture
    Then the multiline value and its neighboring variable are intact

  Scenario: Unicode environment variables are preserved
    Given an advanced "unicode" hook fixture
    When I run the advanced hook fixture
    Then Unicode and emoji values are intact

  Scenario: Environment variables with special characters are preserved
    Given an advanced "special" hook fixture
    When I run the advanced hook fixture
    Then equals signs, quotes, and backslashes are intact

  Scenario: Empty environment variables are distinguished from absent variables
    Given an advanced "empty" hook fixture
    When I run the advanced hook fixture
    Then the empty variable exists and the absent variable does not

  Scenario: Hooks execute in order by priority
    Given an advanced "priority" hook fixture
    When I run the advanced hook fixture
    Then hooks execute in priority order

  Scenario: Hooks with the same order execute in stable name order
    Given an advanced "stable" hook fixture
    When I run the advanced hook fixture
    Then both executions have the same name order

  Scenario: A failed hook allows status queries and a corrected retry
    Given an advanced "failure" hook fixture
    When I run the advanced hook fixture
    Then status reports failure and the corrected hook succeeds

  Scenario: A corrupted state file fails closed
    Given an advanced "corrupt" hook fixture
    When I run the advanced hook fixture
    Then corruption is reported without deleting the file or rerunning hooks

  Scenario: Cancellation stops and reaps the direct child
    Given an advanced "cancel" hook fixture
    When I run the advanced hook fixture
    Then cancellation is acknowledged after the child stops

  Scenario: Cancelled hooks can be triggered again
    Given an advanced "restart" hook fixture
    When I run the advanced hook fixture
    Then a fresh run completes after cancellation

  Scenario: Large hook output is captured in memory
    Given an advanced "large" hook fixture
    When I run the advanced hook fixture
    Then the complete large output is available without being persisted

  Scenario: Binary hook output is handled safely
    Given an advanced "binary" hook fixture
    When I run the advanced hook fixture
    Then invalid UTF-8 is replaced and the hook succeeds
