---
bead: vu-nqj
polecat: dementus
date: 2026-05-17
files:
  - src/web.rs
  - src/config.rs
severity: medium
category: architecture
---

# Any process on the host can read the focused email via the web pane

`WebServer::start` listens on `<bind>:<port>` with no authentication
of any kind (`src/web.rs:122-143`). The bind default is `127.0.0.1`
(`src/config.rs:46-47`) — that protects against off-box network
attackers, but on a shared host every local user and every local
process can hit `http://127.0.0.1:<port>/api/current-email` and read
whatever the user is looking at in real time. There is no token, no
SO_PEERCRED check, no abstract-socket / unix-socket fallback, no
`Origin` check on the SSE endpoint.

This is bigger than the obvious "shared workstation" case:

- A browser tab the user opens in *any* origin can poll
  `127.0.0.1:8080` from JS — same-origin policy lets the request go
  out as a CORS preflight, and the server happily replies with full
  email contents to the no-CORS opaque read on `/` (HTML response)
  and to the JSON response on `/api/current-email` (the absence of
  CORS headers blocks JS *reads* but not the request itself, and
  attacker JS in `<img src=…>`-style probes can still detect message
  state changes via timing / `onerror`).
- Any sandboxed user-on-the-box process (a browser extension's
  native messaging helper, a flatpak, an LSP server running as the
  same uid) can dump the user's mail by polling the same endpoint.
- A user who flips `[web].bind = "0.0.0.0"` (the validator allows it
  — `src/config.rs:297-301`) immediately exposes the read interface
  to the entire LAN, including the SSE event stream that confirms a
  human is actively reading mail (a useful tell for an attacker).

The viewer is "render-only" per VISION.md, but "render-only" is not
the same as "publicly readable". The threat model in the bead
description names this exact case ("can another local process MITM
it?"); the answer today is "yes, trivially".

## Suggested next step

- File a P2 bead: "Require a per-launch loopback token for the web
  pane". Sketch: at start, generate a 128-bit random `token`, embed it
  in the URL the TUI prints, and reject any request without that
  cookie / `?t=` parameter.
- Companion bead: log a startup warning when `[web].bind` resolves to
  anything other than a loopback IP (`is_loopback()`) so a typo or
  copy-paste config doesn't silently expose mail to the LAN.
- Smaller, additive: add a default `Cache-Control: no-store` and
  `Vary: Origin` to the JSON endpoint to discourage CDN / browser
  caching of message contents if the viewer is ever proxied.
