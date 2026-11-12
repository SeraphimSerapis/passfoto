#!/bin/sh
# snap.sh: take one photo from a running passfoto instance.
#
# Usage:
#   snap.sh OUTPUT.jpg [WIDTH]
#
#   WIDTH  optional, 32 to 8192. The server downscales to this width and keeps
#          the aspect ratio. It never upscales. Smaller images cost fewer
#          vision tokens; 1024 is enough for most checks.
#
# Environment (both required, printed in the AGENT SETUP block of the banner):
#   PASSFOTO_URL    base URL, e.g. http://192.168.1.20:8741
#   PASSFOTO_TOKEN  agent token
#
# Exit codes:
#   0  photo written to OUTPUT.jpg, path printed on stdout
#   1  unexpected HTTP status or curl error
#   2  usage error: missing argument or variable, or the server rejected
#      WIDTH (400)
#   3  401: token is wrong or expired, ask the human for the current one
#   4  429: cooldown active. stderr ends with "retry_after=N" (seconds).
#      The script does not wait for you; sleep N and run it again if the
#      photo is still worth taking.
#   5  503 with Retry-After: camera busy or a capture failed. stderr ends
#      with "retry_after=N"; retrying after N seconds is expected to work.
#   6  could not reach passfoto (refused, timeout, DNS), ask the human to
#      run `passfoto start`
#   7  503 without Retry-After: camera unavailable (no device, permission
#      denied). Retrying won't help until the human fixes it; tell them what
#      stderr says.
#
# OUTPUT.jpg is only replaced on success, so a failed call never leaves an
# error message where a JPEG used to be.

set -u

if [ "$#" -lt 1 ] || [ "$#" -gt 2 ]; then
    echo "usage: snap.sh OUTPUT.jpg [WIDTH]" >&2
    exit 2
fi
out=$1
width=${2:-}

if [ -z "${PASSFOTO_URL:-}" ] || [ -z "${PASSFOTO_TOKEN:-}" ]; then
    echo "snap.sh: set PASSFOTO_URL and PASSFOTO_TOKEN" >&2
    exit 2
fi

url="${PASSFOTO_URL%/}/snapshot"
if [ -n "$width" ]; then
    url="${url}?width=${width}"
fi

body=$(mktemp "${out}.XXXXXX") || exit 1
headers=$(mktemp "${TMPDIR:-/tmp}/passfoto-headers.XXXXXX") || exit 1
trap 'rm -f "$body" "$headers"' EXIT

# The token goes in a header, not the URL, so it stays out of shell history
# and process listings. passfoto also accepts ?token=<token> in the URL.
status=$(curl --silent --show-error \
    --connect-timeout 5 --max-time 30 \
    --header "Authorization: Bearer ${PASSFOTO_TOKEN}" \
    --dump-header "$headers" \
    --output "$body" \
    --write-out '%{http_code}' \
    "$url")
curl_exit=$?

if [ "$curl_exit" -ne 0 ]; then
    case $curl_exit in
        6 | 7 | 28)
            echo "snap.sh: cannot reach passfoto at ${PASSFOTO_URL} (curl exit ${curl_exit})" >&2
            exit 6
            ;;
        *)
            echo "snap.sh: curl failed with exit ${curl_exit}" >&2
            exit 1
            ;;
    esac
fi

retry_after=$(grep -i '^retry-after:' "$headers" | tr -d '\r' | awk '{print $2}')

case $status in
    200)
        mv "$body" "$out"
        echo "$out"
        exit 0
        ;;
    400)
        printf 'snap.sh: 400, ' >&2
        cat "$body" >&2
        exit 2
        ;;
    401)
        echo "snap.sh: 401, token is wrong or expired" >&2
        exit 3
        ;;
    429)
        echo "snap.sh: 429, cooldown active" >&2
        echo "retry_after=${retry_after}" >&2
        exit 4
        ;;
    503)
        if [ -n "$retry_after" ]; then
            printf 'snap.sh: 503, ' >&2
            cat "$body" >&2
            echo "retry_after=${retry_after}" >&2
            exit 5
        fi
        printf 'snap.sh: 503, ' >&2
        cat "$body" >&2
        exit 7
        ;;
    *)
        echo "snap.sh: unexpected HTTP status ${status}" >&2
        exit 1
        ;;
esac
