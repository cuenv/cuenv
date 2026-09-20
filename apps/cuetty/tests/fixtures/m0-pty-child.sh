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
    shell-level)
        printf 'M0-SHLVL:%s\r\n' "${SHLVL:-missing}"
        ;;
    raw|bracketed|kitty|modify-other-keys)
        count=${2:?expected input byte count}
        case "$count" in
            ''|*[!0-9]*) exit 64 ;;
        esac
        stty raw -echo
        if test "$1" = bracketed; then
            printf '\033[?2004h'
        elif test "$1" = kitty; then
            printf '\033[>1u'
        elif test "$1" = modify-other-keys; then
            printf '\033[>4;2m'
        fi
        printf 'M0-READY\r\n'
        bytes=$(dd bs=1 count="$count" 2>/dev/null | od -An -tx1 | tr -d ' \n')
        printf '\033[?2004l\033[<u\033[>4;0mM0-BYTES:%s\r\n' "$bytes"
        ;;
    hold)
        stty -echo
        printf 'M0-READY\r\n'
        IFS= read -r command
        printf 'M0-UNEXPECTED-INPUT:%s\r\n' "$command"
        ;;
    history)
        stty -echo
        line=1
        while test "$line" -le 40; do
            printf 'M0-HISTORY-%02d\r\n' "$line"
            line=$((line + 1))
        done
        printf 'M0-READY\r\n'
        IFS= read -r command
        printf 'M0-UNEXPECTED-INPUT:%s\r\n' "$command"
        ;;
    *)
        printf 'usage: m0-pty-child.sh exit|resize|shell-level|raw COUNT|bracketed COUNT|kitty COUNT|modify-other-keys COUNT|hold|history\n' >&2
        exit 64
        ;;
esac
