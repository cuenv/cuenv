#!/usr/bin/env bash
set -euo pipefail

# ============================================================================
# claude-loop.sh - Run Claude Code headlessly until task completion
# ============================================================================
# Usage: claude-loop.sh <prompt> [max_iterations]
#
# Environment Variables:
#   MAX_ITERATIONS    - Maximum loop iterations (default: 10)
#   MAX_TURNS_PER_RUN - Claude turns per iteration (default: 10)
#   TIMEOUT           - Timeout per iteration in seconds (default: none)
#   VERBOSE           - Set to 1 for debug output
# ============================================================================

# Colors for output
RED='\033[0;31m'
GREEN='\033[0;32m'
YELLOW='\033[1;33m'
BLUE='\033[0;34m'
CYAN='\033[0;36m'
NC='\033[0m' # No Color

# Configuration
MAX_ITERATIONS="${MAX_ITERATIONS:-10}"
MAX_TURNS_PER_RUN="${MAX_TURNS_PER_RUN:-10}"
TIMEOUT="${TIMEOUT:-}"
VERBOSE="${VERBOSE:-0}"

# Temp file for stderr (cleaned up on exit)
stderr_file=$(mktemp)
trap 'rm -f "$stderr_file"' EXIT

# Completion patterns (case-insensitive matching)
COMPLETION_PATTERNS=(
    "task is complete"
    "task complete"
    "completed successfully"
    "successfully completed"
    "nothing left to do"
    "no more changes"
    "all done"
    "finished"
    "no further action"
    "work is complete"
)

# ============================================================================
# Helper Functions
# ============================================================================

log_info() {
    echo -e "${BLUE}[INFO]${NC} $*"
}

log_success() {
    echo -e "${GREEN}[SUCCESS]${NC} $*"
}

log_warn() {
    echo -e "${YELLOW}[WARN]${NC} $*"
}

log_error() {
    echo -e "${RED}[ERROR]${NC} $*" >&2
}

log_debug() {
    [[ "$VERBOSE" == "1" ]] && echo -e "${CYAN}[DEBUG]${NC} $*"
}

# Check if text contains completion indicators
check_completion_text() {
    local text="$1"
    local lower_text
    lower_text=$(echo "$text" | tr '[:upper:]' '[:lower:]')

    for pattern in "${COMPLETION_PATTERNS[@]}"; do
        if [[ "$lower_text" == *"$pattern"* ]]; then
            return 0
        fi
    done
    return 1
}

# ============================================================================
# Main Execution
# ============================================================================

# Parse arguments
PROMPT="${1:-}"
if [[ -z "$PROMPT" ]]; then
    echo "Usage: $0 <prompt> [max_iterations]"
    echo ""
    echo "Runs Claude Code in headless mode repeatedly until task completion."
    echo ""
    echo "Arguments:"
    echo "  prompt          The task prompt for Claude"
    echo "  max_iterations  Optional: Override MAX_ITERATIONS (default: 10)"
    echo ""
    echo "Environment Variables:"
    echo "  MAX_ITERATIONS    - Maximum loop iterations (default: 10)"
    echo "  MAX_TURNS_PER_RUN - Claude turns per iteration (default: 10)"
    echo "  TIMEOUT           - Timeout per iteration in seconds (default: none)"
    echo "  VERBOSE           - Set to 1 for debug output"
    echo ""
    echo "Exit Codes:"
    echo "  0 - Task completed successfully"
    echo "  1 - Error occurred during execution"
    echo "  2 - Maximum iterations reached (task may be incomplete)"
    echo ""
    echo "Examples:"
    echo "  $0 \"Fix all TypeScript errors in the project\""
    echo "  $0 \"Refactor the auth module\" 20"
    echo "  MAX_TURNS_PER_RUN=15 $0 \"Write tests for api.ts\""
    echo "  VERBOSE=1 $0 \"Add documentation\""
    exit 1
fi

[[ -n "${2:-}" ]] && MAX_ITERATIONS="$2"

# Validate dependencies
if ! command -v jq &> /dev/null; then
    log_error "jq is required but not installed. Install with: brew install jq"
    exit 1
fi

if ! command -v claude &> /dev/null; then
    log_error "claude CLI is required but not installed."
    exit 1
fi

# Initialize
log_info "Starting headless Claude Code loop"
log_info "Task: $PROMPT"
log_info "Max iterations: $MAX_ITERATIONS, Max turns per run: $MAX_TURNS_PER_RUN"
echo ""

session_id=""
iteration=0
final_result=""
continuation_prompt="Continue working on the task. When you have completed all work, explicitly state 'task is complete'."

# Main loop
while [[ $iteration -lt $MAX_ITERATIONS ]]; do
    ((iteration++))
    echo -e "${YELLOW}━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━${NC}"
    log_info "Iteration $iteration/$MAX_ITERATIONS"
    echo -e "${YELLOW}━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━${NC}"

    # Build command
    cmd=(claude --dangerously-skip-permissions -p --output-format stream-json --max-turns "$MAX_TURNS_PER_RUN")

    if [[ -n "$session_id" ]]; then
        cmd+=(--resume "$session_id")
        current_prompt="$continuation_prompt"
    else
        current_prompt="$PROMPT"
    fi

    log_debug "Running: ${cmd[*]} \"$current_prompt\""

    # Build timeout wrapper if configured
    if [[ -n "$TIMEOUT" ]]; then
        timeout_cmd=(timeout "$TIMEOUT")
    else
        timeout_cmd=()
    fi

    # Run Claude with streaming and process output
    num_turns=0
    is_error="false"
    result_text=""
    received_result="false"

    # Clear stderr file for this iteration
    : > "$stderr_file"

    # Process streaming JSONL output (stderr goes to temp file, not mixed with JSON)
    while IFS= read -r line; do
        [[ -z "$line" ]] && continue

        # Parse JSON line
        msg_type=$(echo "$line" | jq -r '.type // empty' 2>/dev/null) || continue

        case "$msg_type" in
            "assistant")
                # Stream assistant content in real-time
                content=$(echo "$line" | jq -r '.message.content[]? | select(.type=="text") | .text // empty' 2>/dev/null)
                if [[ -n "$content" ]]; then
                    echo -e "${GREEN}Claude:${NC} $content"
                fi
                ;;
            "result")
                # Final result message - extract metadata
                received_result="true"
                session_id=$(echo "$line" | jq -r '.session_id // empty')
                num_turns=$(echo "$line" | jq -r '.num_turns // 0')
                is_error=$(echo "$line" | jq -r '.is_error // false')
                result_text=$(echo "$line" | jq -r '.result // empty')
                final_result="$result_text"

                log_debug "Session ID: $session_id"
                log_debug "Turns used: $num_turns"
                log_debug "Is error: $is_error"
                ;;
            "system")
                # System messages (init, etc.)
                subtype=$(echo "$line" | jq -r '.subtype // empty')
                log_debug "System message: $subtype"
                ;;
        esac
    done < <("${timeout_cmd[@]}" "${cmd[@]}" "$current_prompt" 2>"$stderr_file")

    # Show stderr in verbose mode
    if [[ -s "$stderr_file" ]] && [[ "$VERBOSE" == "1" ]]; then
        log_debug "stderr output:"
        cat "$stderr_file" >&2
    fi

    # Check if we received a result message (known Claude Code bug: sometimes missing)
    if [[ "$received_result" == "false" ]]; then
        log_warn "No result message received (possible timeout or Claude Code bug)"
        if [[ -s "$stderr_file" ]]; then
            log_error "stderr output:"
            cat "$stderr_file" >&2
        fi
    fi

    echo ""
    log_info "Turns used this iteration: $num_turns"

    # Check for errors
    if [[ "$is_error" == "true" ]]; then
        log_error "Claude encountered an error"
        echo "$result_text"
        exit 1
    fi

    # Check completion: zero turns means nothing to do
    if [[ "$num_turns" -eq 0 ]]; then
        log_success "No turns taken - task appears complete"
        echo ""
        echo -e "${GREEN}Final Result:${NC}"
        echo "$final_result"
        exit 0
    fi

    # Check completion: text pattern matching
    if check_completion_text "$result_text"; then
        log_success "Completion phrase detected in response"
        echo ""
        echo -e "${GREEN}Final Result:${NC}"
        echo "$final_result"
        exit 0
    fi

    log_info "Task not yet complete, continuing..."
    echo ""
done

# Max iterations reached
log_warn "Maximum iterations reached ($MAX_ITERATIONS)"
echo ""
echo -e "${YELLOW}Final Result (may be incomplete):${NC}"
echo "$final_result"
exit 2
