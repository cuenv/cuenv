#!/bin/sh
# Deterministic child for Cuetty's real-PTY tests. Invoke with /bin/sh.
# This script is only intended to run inside a disposable test PTY.
set -eu
LC_ALL=C
export LC_ALL

case "${1:-}" in
    exit)
        printf 'M0-READY\r\nM0-FINAL-OUTPUT\r\n'
        exit 23
        ;;
    resize)
        stty -echo
        printf 'M0-READY\r\n'
        IFS= read -r command
        test "$command" = size
        dimensions=$(stty size)
        printf 'M0-SIZE:%s\r\n' "$dimensions"
        ;;
    raw|bracketed)
        count=${2:?expected input byte count}
        case "$count" in
            ''|*[!0-9]*) exit 64 ;;
        esac
        stty raw -echo
        if test "$1" = bracketed; then
            printf '\033[?2004h'
        fi
        printf 'M0-READY\r\n'
        bytes=$(dd bs=1 count="$count" 2>/dev/null | od -An -tx1 | tr -d ' \n')
        printf '\033[?2004lM0-BYTES:%s\r\n' "$bytes"
        ;;
    hold)
        stty -echo
        printf 'M0-READY\r\n'
        IFS= read -r command
        printf 'M0-UNEXPECTED-INPUT:%s\r\n' "$command"
        ;;
    *)
        printf 'usage: m0-pty-child.sh exit|resize|raw COUNT|bracketed COUNT|hold\n' >&2
        exit 64
        ;;
esac
