Feature: Advanced Hook Execution Scenarios
  As a developer
  I want hooks to handle edge cases gracefully
  So that my development environment remains stable under unusual conditions

  Background:
    Given cuenv is installed and available
    And the shell integration is configured

  # ============================================================================
  # Timeout Handling
  # ============================================================================

  Scenario: Hook Timeout is Enforced
    Given I am in a directory with a slow hook configured
    And the hook timeout is set to 2 seconds
    And cuenv is allowed in this directory
    When the hook starts executing
    And 3 seconds have passed
    Then the hook should be terminated
    And the hook status should show "timed out"
    And subsequent hooks should not execute if fail_fast is enabled

  Scenario: Timeout Does Not Affect Fast Hooks
    Given I am in a directory with a fast hook configured
    And the hook timeout is set to 30 seconds
    And cuenv is allowed in this directory
    When the hook executes
    Then the hook should complete successfully before timeout
    And the duration should be less than the timeout

  # ============================================================================
  # Concurrent Execution Safety
  # ============================================================================

  Scenario: Multiple Directory Entries Do Not Cause Race Conditions
    Given I am in a directory with hooks configured
    And cuenv is allowed in this directory
    When I trigger hook execution from multiple terminals simultaneously
    Then only one supervisor process should run
    And all terminals should see consistent hook status
    And environment variables should be loaded correctly in all shells

  Scenario: Rapid Directory Changes Do Not Corrupt State
    Given I am in a directory with hooks configured
    And cuenv is allowed in this directory
    When I rapidly change between directories 10 times
    Then the hook state should remain consistent
    And no orphaned supervisor processes should exist
    And environment variables should reflect the final directory state

  # ============================================================================
  # Environment Variable Edge Cases
  # ============================================================================

  Scenario: Multiline Environment Variables Are Preserved
    Given I am in a directory with a source hook that exports multiline values
    And cuenv is allowed in this directory
    When the hook completes successfully
    Then multiline environment variable values should be preserved
    And newlines within values should not corrupt other variables

  Scenario: Unicode Environment Variables Are Preserved
    Given I am in a directory with a source hook that exports unicode values
    And cuenv is allowed in this directory
    When the hook completes successfully
    Then unicode characters in environment variables should be preserved
    And emoji characters should be correctly handled

  Scenario: Environment Variables With Special Characters
    Given I am in a directory with a source hook that exports values with special characters
    And cuenv is allowed in this directory
    When the hook completes successfully
    Then values with equals signs should be preserved
    And values with quotes should be preserved
    And values with backslashes should be preserved

  Scenario: Empty Environment Variables Are Handled
    Given I am in a directory with a source hook that exports empty values
    And cuenv is allowed in this directory
    When the hook completes successfully
    Then empty environment variables should be set correctly
    And they should be distinguishable from unset variables

  # ============================================================================
  # Hook Ordering and Dependencies
  # ============================================================================

  Scenario: Hooks Execute In Order By Priority
    Given I am in a directory with multiple hooks with different orders
    And hook A has order 50
    And hook B has order 100
    And hook C has order 150
    And cuenv is allowed in this directory
    When hooks execute
    Then hook A should execute before hook B
    And hook B should execute before hook C

  Scenario: Hooks With Same Order Execute In Stable Order
    Given I am in a directory with multiple hooks with the same order
    And cuenv is allowed in this directory
    When hooks execute multiple times
    Then the execution order should be consistent across runs

  # ============================================================================
  # Error Recovery
  # ============================================================================

  Scenario: Failed Hook Does Not Prevent Status Queries
    Given I am in a directory with a hook that will fail
    And cuenv is allowed in this directory
    When the hook fails
    Then I should be able to query the hook status
    And the status should show the failure reason
    And I should be able to re-trigger hooks after fixing the issue

  Scenario: Corrupted State File Is Recovered
    Given I am in a directory with previously completed hooks
    And the state file has been corrupted
    When I enter the directory again
    Then cuenv should detect the corruption
    And cuenv should clean up the corrupted state
    And hooks should re-execute normally

  # ============================================================================
  # Cancellation
  # ============================================================================

  Scenario: Hook Cancellation Terminates Running Processes
    Given I am in a directory with a long-running hook
    And cuenv is allowed in this directory
    And the hook is currently executing
    When I cancel the hook execution
    Then the supervisor process should be terminated
    And any child processes should be terminated
    And the state should show "Cancelled"

  Scenario: Cancelled Hooks Can Be Re-Triggered
    Given I am in a directory with hooks that were cancelled
    When I trigger hook execution again
    Then hooks should start fresh
    And the previous cancelled state should be cleared

  # ============================================================================
  # Large Output Handling
  # ============================================================================

  Scenario: Hooks With Large Output Do Not Cause Memory Issues
    Given I am in a directory with a hook that produces 10MB of output
    And cuenv is allowed in this directory
    When the hook executes
    Then the hook should complete successfully
    And the output should be captured without truncation
    And memory usage should remain reasonable

  Scenario: Hooks With Binary Output Are Handled Gracefully
    Given I am in a directory with a hook that produces binary output
    And cuenv is allowed in this directory
    When the hook executes
    Then the hook should complete without crashing
    And binary data should be handled safely
