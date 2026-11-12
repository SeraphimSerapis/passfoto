---
name: passfoto
description: Take a webcam photo on the user's Mac through an authenticated HTTP API, to check the physical result of something you just did (a wired circuit, an e-ink display, an LED, a 3D print). Use it to verify a change after acting, not to watch or poll.
---

# passfoto

passfoto runs on the user's Mac and serves the webcam as a small HTTP API on
the local network. The human starts it with `passfoto start`. Its banner has an
AGENT SETUP block with the base URL and your agent token. Every request needs
that token, and it dies when the process exits.

## When to take a photo

Take a photo when you need to see the physical world to finish a task:

- After you flash firmware, push an image to a display, or ask the human to
  rewire something, to confirm the change actually shows up.
- When the human asks you to look at something.
- When a test passes in software but you have no other way to confirm the
  hardware did what it should.

Do not use it to poll or watch. There is a cooldown between shots (120 s by
default), every request shows up live in the human's terminal, and a photo
costs you vision tokens. Decide what you expect to see before you take the
shot, then check for exactly that.

## Setup

Ask the human to paste the AGENT SETUP lines from the banner into your shell:

```sh
export PASSFOTO_URL=http://192.168.1.20:8741
export PASSFOTO_TOKEN=<agent token>
```

Never guess or brute-force the token. A wrong token gets `401`, and the human
sees every attempt. The banner also shows a preview token for the human's
browser. It does not work on `/snapshot` or `/status`, so don't use it.

To check that passfoto is up and how long the cooldown has left, without
touching the camera or the cooldown:

```sh
curl -s -H "Authorization: Bearer $PASSFOTO_TOKEN" "$PASSFOTO_URL/status"
```

```json
{"version":"0.1.1","min_interval_s":120,"cooldown_remaining_s":0,"last_shot_age_s":null,"camera":"closed","camera_error":null}
```

`camera` is one of `closed`, `opening`, `streaming`, or `failed`. `closed` is
normal: the camera opens on the next snapshot. When it is `failed`,
`camera_error` says why.

## Taking a photo

Use `scripts/snap.sh` from this skill's directory. It writes the JPEG only on
success and maps every failure to its own exit code:

```sh
sh <skill-dir>/scripts/snap.sh shot.jpg 1024
```

The second argument is an optional width in pixels (32 to 8192). The server
downscales to that width and keeps the aspect ratio. It never upscales.

| Exit | Meaning | What to do |
|---|---|---|
| `0` | `shot.jpg` written | Look at it. |
| `2` | Bad arguments, missing `PASSFOTO_URL`/`PASSFOTO_TOKEN`, or the server rejected the width (`400`) | Fix the call or ask the human for the values. |
| `3` | `401`, token wrong or expired | Ask the human for the current token. Do not retry with the same one. |
| `4` | `429`, cooldown active | stderr ends with `retry_after=N`. Sleep `N` seconds and run it again only if you still need the photo. The script never waits on its own. |
| `5` | `503` with `Retry-After`: camera busy, another app holds it, or one capture failed | stderr ends with `retry_after=N`. Sleep `N` seconds and retry. The cooldown was not used up. |
| `7` | `503` without `Retry-After`: camera unavailable (no device, permission denied) | Tell the human what stderr says. Retry only after they fix it. |
| `6` | passfoto not reachable | Ask the human to run `passfoto start`, or check the host and port. |
| `1` | Anything else | Read stderr and report it to the human. |

The first snapshot after a quiet period takes a little longer. The camera
closes when idle, so it has to open again and discard a few frames while the
exposure settles.

## Saving vision tokens

Without a width you get the camera's highest resolution, often 1920x1080. Most
checks (is the LED on, did the display update, is the wire in the right hole)
work at 1024 px wide or less, so pass a width. Keep full resolution for small
printed text or part numbers. Each shot uses up the cooldown, so pick the
width before you shoot instead of taking a small one and then a big one.

## Direct HTTP

If you can't run the script, call the API yourself. Send the token as
`Authorization: Bearer <token>`; `?token=<token>` in the URL also works, but
it ends up in logs and shell history.

```sh
curl -sS -H "Authorization: Bearer $PASSFOTO_TOKEN" -o shot.jpg -D headers.txt \
  "$PASSFOTO_URL/snapshot?width=1024"
```

`GET /snapshot` takes `width` (32 to 8192) and `quality` (JPEG quality 1 to
100, default 85).

- `200`: body is the JPEG. `X-Captured-At` holds the capture time in UTC, for
  example `2026-11-12T13:06:12.345Z`.
- `400`: bad `width` or `quality`. The body says which.
- `401`: wrong or expired token. The token changes every time passfoto
  restarts.
- `404`: wrong path. The agent paths are `/snapshot` and `/status`.
- `429`: cooldown. Wait the number of seconds in `Retry-After`.
- `503` with `Retry-After`: busy or transient failure, retry after that many
  seconds.
- `503` without `Retry-After`: camera unavailable. The body says why; the
  human has to fix it.
- Connection refused: passfoto is not running.

## Notes

- `/preview` and `/preview/frame` are the human's viewfinder and need the
  preview token. They don't affect your cooldown.
- `align=1` on `/snapshot` is ignored. Agent snapshots are always rate
  limited.
- Every request is logged in the human's terminal as `GRANT`, `DENY`,
  `THROTTLED`, `BAD REQ`, `ERROR`, `STATUS`, or `PREVIEW`.
