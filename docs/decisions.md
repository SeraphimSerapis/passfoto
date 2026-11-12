# Decisions

## Separate preview token

Date: 2026-11-12

The human's browser preview and the agent use different tokens. The agent
token works on `/snapshot` and `/status`; the preview token works on
`/preview` and `/preview/frame`.

Why: with one shared token, the preview's cooldown-free frames were reachable
by the agent. Before 0.2.0, `/snapshot?align=1` skipped the cooldown so the
preview could refresh every 2 s, and any agent holding the token could use it
the same way. Splitting the tokens makes the cooldown a property of the agent
token, so no request parameter can bypass it. `align` is now ignored.

Rejected: keeping one token and throttling `align=1` separately. Any rule
keyed on a request parameter is one the agent can learn and use.

Trade-off: the human handles two tokens, and a leaked preview token still
yields 960 px frames with no cooldown.

Revisit if: passfoto gains real per-client identity (for example mTLS or
per-agent tokens), which would make the role split redundant.

## Bind to 0.0.0.0 by default

Date: 2026-11-12

`passfoto start` listens on all interfaces unless `--bind` says otherwise.

Why: the common setup is an agent on another machine on the same LAN (a dev
box, a VM, a container host) photographing a rig next to the Mac. Defaulting
to `127.0.0.1` would break that first run for most users, and the banner
already prints every reachable URL.

Rejected: defaulting to `127.0.0.1` and requiring `--bind 0.0.0.0` for LAN use.
Safer, but it moves the failure to "connection refused" on the agent side,
where it is hardest to diagnose.

Trade-off: anyone on the LAN can reach the port. The 128-bit agent token is
the only gate, the camera stays closed until a valid request arrives, and every
request is logged. Traffic is plain HTTP, so the token is visible to anyone
who can sniff the network.

Revisit if: passfoto is used on untrusted networks, or adds TLS.
