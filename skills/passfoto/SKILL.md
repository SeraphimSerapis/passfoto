---
name: passfoto
description: Lets you capture webcam photos on a macOS host via authenticated HTTP, so you can visually inspect physical-world progress (hardware builds, eink displays, IoT rigs) without a human snapping photos for you.
---

# passfoto

passfoto runs on the user's Mac and exposes the webcam as a tiny HTTP API,
reachable from localhost and other machines on the local network. The human
starts it and receives a session token plus ready-to-use URLs; every request
must carry that token.

## Discovering a running instance

The human gives you the host (an IP like `192.168.x.x` or `localhost`) and the
token. To check an instance:

```sh
curl -s -o /dev/null -w "%{http_code}" "http://<host>:8741/snapshot?token=TEST"
```

- `401` means passfoto is running but your token is wrong/expired.
- connection refused means it is not running — ask the human to start it
  (`passfoto start`).

## Getting the token

The human provides the token. It is printed in the terminal where passfoto
runs. Never guess or brute-force it; there are no retries to win.

## Taking a photo

```sh
curl -s -o shot.jpg "http://<host>:8741/snapshot?token=<token>"
```

Returns `200 image/jpeg` (typically around 1920x1080), suitable for visual
inspection of physical setups, screens, LEDs, wiring, and printed text.

## Status codes

- `200` — success, body is the JPEG.
- `401` — invalid or expired token. The token dies when the human's passfoto
  process exits. Ask the human to restart it or re-share the current token.
- `429` — you hit the cooldown (default 120 s between shots). Read the
  `Retry-After` header and wait that long. Do not hammer the endpoint; each
  request is logged and visible to the human.

## Notes

- `/preview` exists for the human's browser only; don't use it.
- Every request is logged live in the human's terminal as GRANT, DENY,
  THROTTLED, ALIGN, or PREVIEW. Behave accordingly.
