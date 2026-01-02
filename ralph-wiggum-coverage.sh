#!/usr/bin/env bash
set -euo pipefail

TARGET_COVERAGE=80
MAX_ITERATIONS=50
CHECKLIST_FILE="test-coverage-review.md"

get_coverage() {
    if [[ ! -f lcov.info ]]; then
        echo "0"
        return
    fi

    local lines_found=0
    local lines_hit=0

    while IFS= read -r line; do
        if [[ $line == LF:* ]]; then
            lines_found=$((lines_found + ${line#LF:}))
        elif [[ $line == LH:* ]]; then
            lines_hit=$((lines_hit + ${line#LH:}))
        fi
    done < lcov.info

    if [[ $lines_found -eq 0 ]]; then
        echo "0"
    else
        echo "$((lines_hit * 100 / lines_found))"
    fi
}

# Initialize checklist if it doesn't exist
init_checklist() {
    if [[ -f "$CHECKLIST_FILE" ]]; then
        echo "Checklist already exists: $CHECKLIST_FILE"
        return
    fi

    echo "Creating test coverage review checklist..."

    cat > "$CHECKLIST_FILE" << 'EOF'
# Test Coverage Review Checklist

This checklist tracks systematic review of each crate's test coverage.
Updated automatically by the Ralph Wiggum coverage script.

## Review Criteria

For each crate, verify:
1. **Existing tests are meaningful** - not just smoke tests
2. **Critical paths are covered** - error handling, edge cases
3. **Tests match the code's intent** - testing behavior, not implementation
4. **No missing test scenarios** - happy path, error path, boundary conditions

## Crate Review Status

| Crate | Coverage | Reviewed | Tests Added | Notes |
|-------|----------|----------|-------------|-------|
EOF

    # Add each crate to the checklist
    for crate_dir in crates/*/; do
        crate_name=$(basename "$crate_dir")
        echo "| $crate_name | ⏳ | [ ] | [ ] | Pending review |" >> "$CHECKLIST_FILE"
    done

    cat >> "$CHECKLIST_FILE" << 'EOF'

## Session Log

<!-- Claude will append notes here as it reviews each crate -->
EOF

    echo "Checklist created: $CHECKLIST_FILE"
}

iteration=0

# Initialize the checklist on first run
init_checklist

while true; do
    iteration=$((iteration + 1))
    echo "=== Ralph Wiggum Iteration $iteration ==="

    if [[ $iteration -gt $MAX_ITERATIONS ]]; then
        echo "Max iterations reached. Giving up."
        exit 1
    fi

    echo "Running coverage..."
    # Use cargo llvm-cov directly (not nextest) to avoid macOS double-spawn issues
    cuenv exec -- cargo llvm-cov --workspace --all-features --lcov --output-path lcov.info 2>&1 || true

    coverage=$(get_coverage)
    echo "Current coverage: ${coverage}%"

    if [[ $coverage -ge $TARGET_COVERAGE ]]; then
        echo "Target coverage of ${TARGET_COVERAGE}% reached!"
        exit 0
    fi

    echo "Coverage below target. Invoking Claude Code..."

    claude --dangerously-skip-permissions -p "
Our test coverage is currently at ${coverage}%, and we need to reach ${TARGET_COVERAGE}%.

## Your Tracking File

There is a checklist at '${CHECKLIST_FILE}' that tracks your progress reviewing each crate.
- Read this file at the START of each session to see what's been done
- Update the checklist as you complete reviews
- Mark crates as reviewed with [x] when done
- Add notes about what tests you added or issues you found

## Priority Order

### 1. FIRST - Fix Failing Tests
Run 'cuenv exec -- cargo test --workspace' to check for failing tests.
If ANY tests are failing, fix them FIRST before adding new tests.
Commit each fix separately: 'fix(crate-name): resolve failing test for X'

### 2. SECOND - Fix Slow Tests
Review test output for slow tests (taking more than a few seconds).
Optimize or refactor slow tests to run faster.
Commit each optimization: 'perf(crate-name): speed up test for X'

### 3. THIRD - Systematic Crate Review
Work through crates ONE AT A TIME, in order of lowest coverage first.

For each crate:
a) Check the lcov.info to find its current coverage
b) Read the existing tests - are they testing the RIGHT things?
c) Read the source code - what critical paths are untested?
d) Add meaningful tests for:
   - Error handling paths
   - Edge cases and boundary conditions
   - Core business logic
   - Integration points
e) Update the checklist with your findings
f) Commit your changes for this crate before moving to the next

### 4. Test Quality Standards
- Tests must exercise real code paths, not just exist for coverage
- Test behavior and outcomes, not implementation details
- Include both happy path AND error path tests
- Use descriptive test names that explain what's being tested
- Do NOT add trivial tests just to inflate coverage numbers

## Commit Strategy
- One commit per crate reviewed (or per logical unit within large crates)
- Commit message format: 'test(crate-name): add tests for X functionality'
- If you fix a bug, commit separately: 'fix(crate-name): description'
- Do NOT batch everything into one large commit

## After Each Crate
1. Run tests to verify: 'cuenv exec -- cargo test -p <crate-name>'
2. Update the checklist file with coverage % and notes
3. Commit your changes
4. Move to the next lowest-coverage crate

When done with this session, regenerate coverage:
'cuenv exec -- cargo llvm-cov --workspace --all-features --lcov --output-path lcov.info'
"

    echo "Claude iteration complete. Checking coverage again..."
done
