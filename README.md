<div align="left">

```
██████╗  █████╗ ███████╗███████╗ ███████╗ ██████╗ ████████╗ ██████╗
██╔══██╗██╔══██╗██╔════╝██╔════╝ ██╔════╝██╔═══██╗╚══██╔══╝██╔═══██╗
██████╔╝███████║███████╗███████╗ █████╗  ██║   ██║   ██║   ██║   ██║
██╔═══╝ ██╔══██║╚════██║╚════██║ ██╔══╝  ██║   ██║   ██║   ██║   ██║
██║     ██║  ██║███████║███████║ ██║     ╚██████╔╝   ██║   ╚██████╔╝
╚═╝     ╚═╝  ╚═╝╚══════╝╚══════╝ ╚═╝      ╚═════╝    ╚═╝    ╚═════╝
```

**Ephemeral-token webcam access for agents on macOS.**

</div>

---

A small macOS CLI that gives coding agents webcam access: an HTTP API with a
per-session token for capturing still photos, so agents can check physical
experiments (hardware builds, e-ink displays, IoT rigs) without a human taking
photos for them.

## Why

Agents working on physical projects need eyes. passfoto turns the Mac's webcam
into a gated endpoint: start it, hand your agent a URL and token, and it can
look at the real world whenever the cooldown allows.

## Quickstart

Install (any one):

```sh
cargo install --git https://github.com/SeraphimSerapis/passfoto   # straight from GitHub
# or from a local clone:
git clone <repo-url> && cd passfoto && cargo install --path .
```

Then run:

```sh
passfoto start
```

1. On first run, approve the macOS camera permission prompt. passfoto waits up
   to 60 s for it before printing the banner. If you deny it, passfoto keeps
   running and snapshots return `503` until you enable it in System Settings >
   Privacy & Security > Camera.
2. Paste the two `export` lines from the banner's AGENT SETUP block into your
   agent's shell. They set `PASSFOTO_URL` and `PASSFOTO_TOKEN`.
3. Open one of the `PREVIEW` URLs in a browser to frame the shot. It works
   from any machine on your LAN and uses its own token.
4. Give your agent the skill so it knows the API. `cargo install` ships only
   the binary, so copy it into your agent's skills directory:
   `cp -r skills/passfoto ~/.pi/agent/skills/`. The skill includes
   `scripts/snap.sh`, which takes one photo and turns each failure into a
   distinct exit code.

Ctrl-C exits and revokes both tokens.

## Commands

```
passfoto [start] [OPTIONS]          serve the webcam over HTTP (default)
passfoto cameras                    list camera devices as <index><TAB><name>
passfoto help | --help | -h         show help
passfoto version | --version | -V   show version
```

## Options

Options apply to `start`. Both `--flag value` and `--flag=value` work.

| Option | Default | Meaning |
|---|---|---|
| `--port <N>` | `8741` | HTTP port |
| `--bind <ADDR>` | `0.0.0.0` | IP address to listen on (IPv4 or IPv6 literal). `127.0.0.1` keeps it on this Mac |
| `--min-interval <SECS>` | `120` | Cooldown between agent snapshots |
| `--camera <INDEX>` | `0` | Camera device index, see `passfoto cameras` |
| `--idle-timeout <SECS>` | `30` | Close the camera after this long without a capture |

Usage errors exit with `2`, runtime errors such as a port already in use exit
with `1`.

## API

passfoto prints two tokens at startup. The agent token works on `/snapshot`
and `/status`. The preview token works on `/preview` and `/preview/frame`.
Using either token on the other's routes returns `401`.

Send the token as `Authorization: Bearer <token>` or as `?token=<token>`. If an
`Authorization` header is present it must use the `Bearer` scheme, and it
takes precedence over the query string. Unknown paths return `404` before the
token is checked. Every response carries `Cache-Control: no-store`.

| Endpoint | Token | Description |
|---|---|---|
| `GET /snapshot` | agent | One JPEG from the camera. Optional `width` (32 to 8192, downscale only, keeps aspect ratio) and `quality` (1 to 100, default 85). Always subject to the cooldown |
| `GET /status` | agent | JSON with version, cooldown and camera state. Never touches the camera |
| `GET /preview` | preview | HTML viewfinder for humans. Polls `/preview/frame` every 2 s |
| `GET /preview/frame` | preview | JPEG at most 960 px wide, quality 70. No cooldown, does not affect the agent cooldown |

`/snapshot` responses:

| Status | Meaning |
|---|---|
| `200` | `image/jpeg`. `X-Captured-At` holds the capture time as RFC 3339 UTC with milliseconds, e.g. `2026-11-12T13:06:12.345Z` |
| `400` | Bad `width` or `quality`; the body says which |
| `401` | Missing or wrong token |
| `429` | Inside the cooldown. `Retry-After: N` and the body `throttled, retry in Ns` carry the same N |
| `503` + `Retry-After: 1` | Camera busy with another capture |
| `503` + `Retry-After: 2` | Transient failure, including another app holding the camera |
| `503`, no `Retry-After` | Camera unavailable (no device, permission denied). The body starts with `camera unavailable:` and needs a human to fix |

A `503` never uses up the cooldown. When several agent requests arrive at
once, only one gets through the cooldown.

`/status` example:

```json
{"version":"0.1.1","min_interval_s":120,"cooldown_remaining_s":0,"last_shot_age_s":null,"camera":"closed","camera_error":null}
```

`camera` is `closed`, `opening`, `streaming`, or `failed`. `camera_error` is
`null` unless `camera` is `failed`. `last_shot_age_s` is `null` until the first
successful agent snapshot.

## Camera and LED

The camera is not opened at startup. It opens on the first capture, discards a
few frames so exposure can settle, and closes again after `--idle-timeout`
seconds without a capture. The green camera LED is only on while passfoto is
capturing or within that idle window. An open preview tab keeps the camera
and LED on, because every preview frame is a capture and resets the idle timer.

If the camera fails to open, passfoto tries again on later captures, waiting
1, 2, 4, and so on up to 30 seconds between attempts. The terminal log shows
when the next attempt is due.

## Security notes

- passfoto binds to all interfaces (`0.0.0.0`) by default so agents on other
  LAN machines can reach it. Use `--bind 127.0.0.1` if your agent runs on the
  same Mac. On startup it prints every usable URL.
- Traffic is plain HTTP. Anyone who can sniff your network can read the
  token, so don't run passfoto with the default bind on untrusted networks.
- The agent token is the only gate on the camera. Treat it like a password and
  give it only to your agent. Prefer the `Authorization` header over
  `?token=`, which ends up in shell history and logs.
- The preview token is separate so the agent token has no way around the
  cooldown. Earlier builds let an agent add `align=1` to `/snapshot` to skip
  it; that parameter is now ignored. The preview token can still fetch
  960 px frames with no cooldown, so keep it as private as the agent token.
- Both tokens are 32 hex chars from `/dev/urandom`, live only in process
  memory, and expire when the process exits.
- Every request is logged live to the terminal as `GRANT`, `DENY`,
  `THROTTLED`, `BAD REQ`, `ERROR`, `STATUS`, or `PREVIEW`. Successful
  `/preview/frame` requests are not logged.

## Development

```sh
cargo test
cargo clippy --all-targets -- -D warnings
```

CI runs build, tests, and clippy on macOS for every push to `main` and every
pull request.

## License

MIT, see [LICENSE](LICENSE).
