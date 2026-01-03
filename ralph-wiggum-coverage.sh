#!/usr/bin/env bash
set -euo pipefail

MAX_ITERATIONS=50
CHECKLIST_FILE="ARCHITECTURE_REFACTOR.md"

get_progress() {
    if [[ ! -f "$CHECKLIST_FILE" ]]; then
        echo "0 0 0"
        return
    fi

    # Count completed checkboxes (- [x])
    local completed
    completed=$(grep -c '\- \[x\]' "$CHECKLIST_FILE" 2>/dev/null || echo "0")

    # Count incomplete checkboxes (- [ ])
    local incomplete
    incomplete=$(grep -c '\- \[ \]' "$CHECKLIST_FILE" 2>/dev/null || echo "0")

    local total=$((completed + incomplete))

    if [[ $total -eq 0 ]]; then
        echo "0 0 0"
    else
        local percentage=$((completed * 100 / total))
        echo "$percentage $completed $total"
    fi
}

get_current_phase() {
    if [[ ! -f "$CHECKLIST_FILE" ]]; then
        echo "Unknown"
        return
    fi

    # Find the first phase header that has incomplete checkboxes after it
    local current_phase=""
    local in_phase=""

    while IFS= read -r line; do
        if [[ $line =~ ^##[[:space:]]Phase[[:space:]]([0-9]+): ]]; then
            in_phase="${BASH_REMATCH[1]}"
        elif [[ -n "$in_phase" && $line =~ ^-[[:space:]]\[[[:space:]]\] ]]; then
            current_phase="$in_phase"
            break
        fi
    done < "$CHECKLIST_FILE"

    if [[ -n "$current_phase" ]]; then
        echo "Phase $current_phase"
    else
        echo "All Complete"
    fi
}

iteration=0

while true; do
    iteration=$((iteration + 1))
    echo "=== Ralph Wiggum Iteration $iteration ==="

    if [[ $iteration -gt $MAX_ITERATIONS ]]; then
        echo "Max iterations reached. Giving up."
        exit 1
    fi

    read -r progress completed total <<< "$(get_progress)"
    current_phase=$(get_current_phase)

    echo "Current progress: ${progress}% ($completed/$total tasks complete)"
    echo "Current phase: $current_phase"

    if [[ $progress -eq 100 ]]; then
        echo "All refactor tasks complete!"
        exit 0
    fi

    echo "Invoking Claude Code to continue refactoring..."

    claude --dangerously-skip-permissions --output-format stream-json --verbose -p "
You are continuing the cuenv-core architecture refactor. Your progress tracker is '${CHECKLIST_FILE}'.

## Current Status
- Progress: ${progress}% ($completed/$total tasks complete)
- Current Phase: $current_phase

## Your Task

1. **Read ${CHECKLIST_FILE}** to understand what's been done and what's next
2. **Work through tasks in order** - complete the current phase before moving to the next
3. **Focus on ONE task at a time** - complete it fully before moving on
4. **Mark tasks complete** by changing \`- [ ]\` to \`- [x]\` in ${CHECKLIST_FILE}

## Workflow for Each Task

1. Read the relevant source files to understand current state
2. Make the required changes (move files, update imports, etc.)
3. Run validation commands for the current phase
4. If validation passes, mark the task complete in ${CHECKLIST_FILE}
5. Commit your changes with a descriptive message

## Validation Commands

After making changes, run these to verify:
\`\`\`bash
cuenv exec -- cargo check --workspace
cuenv exec -- cargo test --workspace
cuenv task check
\`\`\`

## Commit Strategy

- One commit per logical unit of work (e.g., completing a subsection)
- Commit message format: \`refactor(crate-name): description of change\`
- Push and update the PR description with progress

## Important Notes

- **No backwards compatibility** - clean breaks are fine, no re-exports or deprecation shims
- If you encounter issues, document them in the checklist with a note
- If a task is blocked by another, skip it and note the dependency
- Keep the code compiling at each step

## When Done

After completing tasks this session, ensure:
1. All your changes compile: \`cuenv exec -- cargo check --workspace\`
2. Tests pass: \`cuenv exec -- cargo test --workspace\`
3. Checklist is updated with your progress
4. Changes are committed and pushed
" | jq --unbuffered -r 'select(.type == "assistant") | .message.content[]? | select(.type == "text") | "[\(now | strftime("%H:%M:%S"))] \(.text)"'

    echo "Claude iteration complete. Checking progress again..."
done
