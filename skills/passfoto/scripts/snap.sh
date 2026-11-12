#!/bin/sh
# snap.sh: take one photo from a running passfoto instance.
#
# Usage:
#   snap.sh OUTPUT.jpg
#
# Environment (both required, the human gets them from the passfoto banner):
#   PASSFOTO_URL    base URL, e.g. http://192.168.1.20:8741
#   PASSFOTO_TOKEN  session token
#
# Exit codes:
#   0  photo written to OUTPUT.jpg
#   1  unexpected HTTP status or curl error
#   2  usage error (missing argument or environment variable)
#   3  401: token is wrong or expired, ask the human for the current one
#   4  429: cooldown active. stderr ends with "retry_after=N" (seconds).
#      The script does not wait for you; sleep N and run it again if the
#      photo is still worth taking.
#   5  503: camera failed for this request, retry in a few seconds
#   6  could not reach passfoto (refused, timeout, DNS), ask the human to
#      run `passfoto start`
#
# OUTPUT.jpg is only replaced on success, so a failed call never leaves an
# error message where a JPEG used to be.

set -u

if [ "$#" -ne 1 ]; then
    echo "usage: snap.sh OUTPUT.jpg" >&2
    exit 2
fi
out=$1

if [ -z "${PASSFOTO_URL:-}" ] || [ -z "${PASSFOTO_TOKEN:-}" ]; then
    echo "snap.sh: set PASSFOTO_URL and PASSFOTO_TOKEN" >&2
    exit 2
fi
base=${PASSFOTO_URL%/}

body=$(mktemp "${out}.XXXXXX") || exit 1
headers=$(mktemp "${TMPDIR:-/tmp}/passfoto-headers.XXXXXX") || exit 1
trap 'rm -f "$body" "$headers"' EXIT

# The token goes in a header, not the URL, so it stays out of shell history
# and process listings. Older passfoto builds only accept ?token=<token>.
status=$(curl --silent --show-error \
    --connect-timeout 5 --max-time 30 \
    --header "Authorization: Bearer ${PASSFOTO_TOKEN}" \
    --dump-header "$headers" \
    --output "$body" \
    --write-out '%{http_code}' \
    "${base}/snapshot")
curl_exit=$?

if [ "$curl_exit" -ne 0 ]; then
    case $curl_exit in
        6 | 7 | 28)
            echo "snap.sh: cannot reach passfoto at ${base} (curl exit ${curl_exit})" >&2
            exit 6
            ;;
        *)
            echo "snap.sh: curl failed with exit ${curl_exit}" >&2
            exit 1
            ;;
    esac
fi

case $status in
    200)
        mv "$body" "$out"
        echo "$out"
        exit 0
        ;;
    401)
        echo "snap.sh: 401, token is wrong or expired" >&2
        exit 3
        ;;
    429)
        retry_after=$(grep -i '^retry-after:' "$headers" | tr -d '\r' | awk '{print $2}')
        echo "snap.sh: 429, cooldown active" >&2
        echo "retry_after=${retry_after:-unknown}" >&2
        exit 4
        ;;
    503)
        echo "snap.sh: 503, camera capture failed, retry in a few seconds" >&2
        exit 5
        ;;
    *)
        echo "snap.sh: unexpected HTTP status ${status}" >&2
        exit 1
        ;;
esac
