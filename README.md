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

A minimal macOS CLI that gives coding agents webcam access: an ephemeral-token
HTTP API for capturing still photos, so agents can watch physical experiments
(hardware builds, eink displays, IoT rigs) without a human snapping photos on
their behalf.

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

1. Approve the macOS camera permission prompt (first run only).
2. Copy the session token from the banner.
3. Open the printed `PREVIEW` URL in a browser to frame the shot — it works from
   any machine on your LAN.
4. Give your agent the token and the base URL (see `skills/passfoto/SKILL.md`).
   `cargo install` ships only the binary — copy the skill into your agent's
   skills dir so it knows the API: `cp -r skills/passfoto ~/.pi/agent/skills/`

Ctrl-C exits and permanently revokes the token.

## API

All endpoints require `?token=<token>`; anything else returns `401`.

| Endpoint | Description | Status codes |
|---|---|---|
| `GET /snapshot` | JPEG still from the default camera | `200`, `429` when inside the cooldown (`Retry-After` header set), `503` on transient camera failure (safe to retry shortly) |
| `GET /snapshot?token=<t>&align=1` | Skips the cooldown, for humans framing in `/preview`; does not touch the agent cooldown | `200` |
| `GET /preview` | Minimal HTML live view, refreshes every 2 s; for humans, not agents | `200` |

## Flags

| Flag | Default | Meaning |
|---|---|---|
| `--port <n>` | `8741` | HTTP port (always bound to `0.0.0.0`) |
| `--min-interval <secs>` | `120` | Cooldown between agent shots |

Usage: `passfoto start [--port N] [--min-interval SECS]`

## Security notes

- Binds to all interfaces (`0.0.0.0`) so agents on other LAN machines can reach
  it; on startup it prints every usable URL (localhost + LAN IPs). The session
  token is the only gate — treat it like a password and share it only with your
  agent.
- The token is 32 hex chars from `/dev/urandom`, lives only in process memory,
  and expires the moment the process exits.
- Every request is logged live to the terminal as `GRANT`, `DENY`,
  `THROTTLED`, `ALIGN`, or `PREVIEW`.

## Development

```sh
cargo test
```

## License

MIT — see [LICENSE](LICENSE).
