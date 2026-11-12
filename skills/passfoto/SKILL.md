---
name: passfoto
description: Take a webcam photo on the user's Mac through an authenticated HTTP API, to check the physical result of something you just did (a wired circuit, an e-ink display, an LED, a 3D print). Use it to verify a change after acting, not to watch or poll.
---

# passfoto

passfoto runs on the user's Mac and serves the webcam as a small HTTP API on
the local network. The human starts it with `passfoto start`, and the banner
shows the base URL (port `8741` by default) and a session token. Every request
needs that token. The token dies when the process exits.

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

Ask the human for the base URL and token, then export them:

```sh
export PASSFOTO_URL=http://192.168.1.20:8741   # from the banner
export PASSFOTO_TOKEN=<token>                  # from the banner
```

Never guess or brute-force the token. A wrong token gets `401`, and the human
sees every attempt.

## Taking a photo

Use `scripts/snap.sh` from this skill's directory. It writes the JPEG only on
success and maps every failure to its own exit code:

```sh
sh <skill-dir>/scripts/snap.sh shot.jpg
```

| Exit | Meaning | What to do |
|---|---|---|
| `0` | `shot.jpg` written | Look at it. |
| `2` | Missing argument, `PASSFOTO_URL`, or `PASSFOTO_TOKEN` | Fix the call or ask the human for the values. |
| `3` | `401`, token wrong or expired | Ask the human for the current token. Do not retry with the same one. |
| `4` | `429`, cooldown active | stderr ends with `retry_after=N`. Sleep `N` seconds and run it again only if you still need the photo. The script never waits on its own. |
| `5` | `503`, camera failed for this request | Wait a few seconds and retry. The cooldown was not used up. |
| `6` | passfoto not reachable | Ask the human to run `passfoto start`, or check the host and port. |
| `1` | Anything else | Read stderr and report it to the human. |

The script sends the token as `Authorization: Bearer <token>`. If you are on a
passfoto build that rejects the header with `401` even though the token is
right, fall back to the query string:

```sh
curl -sS -o shot.jpg "$PASSFOTO_URL/snapshot?token=$PASSFOTO_TOKEN"
```

## Saving vision tokens

The camera returns its full resolution, usually around 1920x1080. Most checks
(is the LED on, did the display update, is the wire in the right hole) work at
1024 px wide or less. Downscale before you look:

```sh
sips -Z 1024 shot.jpg          # macOS
magick shot.jpg -resize 1024x shot.jpg   # ImageMagick, anywhere
```

Keep the full image when you need to read small printed text or part numbers.

## Status codes, for direct HTTP use

- `200`: body is the JPEG.
- `401`: wrong or expired token. Ask the human; the token changes every time
  passfoto restarts.
- `404`: wrong path. Only `/snapshot` and `/preview` exist.
- `429`: cooldown. Wait for the number of seconds in `Retry-After`.
- `503`: transient camera failure. Retry after a few seconds.
- Connection refused: passfoto is not running.

## Notes

- `/preview` is the human's live view for framing the shot. Don't call it, and
  don't pass `align=1` to `/snapshot`; that parameter is for the preview page.
- Every request is logged in the human's terminal as `GRANT`, `DENY`,
  `THROTTLED`, `ALIGN`, `PREVIEW`, or `ERROR`.
