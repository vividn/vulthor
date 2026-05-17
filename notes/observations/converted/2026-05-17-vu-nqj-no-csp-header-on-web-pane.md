---
bead: vu-nqj
polecat: dementus
date: 2026-05-17
files:
  - src/web.rs
severity: high
category: architecture
---

# Web pane sends no Content-Security-Policy and no sandboxed iframe

`WebServer::start` registers seven routes (`src/web.rs:123-134`) and
none of the response helpers add a `Content-Security-Policy`,
`X-Frame-Options`, `Referrer-Policy`, or `X-Content-Type-Options`
header. The two HTML shells in `generate_email_html`
(`src/web.rs:438-446`) and `generate_welcome_html` (`src/web.rs:617-625`)
only set `<meta charset>`, `<meta viewport>`, and `<meta theme-color>`
— there is no `<meta http-equiv="Content-Security-Policy">` either.

Combined with [[2026-05-17-vu-nqj-html-body-rendered-unsanitized]] this
means a hostile HTML email can:

- Execute arbitrary inline JS (`<script>`, `onload=`, etc.).
- Hit any URL on the public internet (no `connect-src` restriction).
- Load remote images / fonts / iframes for tracking and beaconing
  (no `img-src`, no `font-src`, no `frame-src`).
- `fetch('/api/current-email')` to read the user's other messages on
  every selection change (same origin, no CORS check needed).

The rendered HTML is also not wrapped in a sandboxed `<iframe>` — the
email body is interpolated directly into the application shell
(`src/web.rs:594-596`), so it shares the origin with the service
worker, the manifest fetch, and any future JSON APIs the viewer
grows.

A reasonable target for a "render-only" viewer:

```
Content-Security-Policy:
  default-src 'none';
  style-src 'self';
  img-src 'self' data:;
  font-src 'self';
  script-src 'self';            # or 'none' if the SW/loader gets moved to an external file
  connect-src 'self';
  frame-ancestors 'none';
  base-uri 'none';
  form-action 'none';
```

…delivered as an HTTP header from every handler in `WebServer::start`,
plus an `<iframe sandbox="allow-same-origin">` (or stricter) around the
body content so even a sanitizer bypass cannot script the outer shell.

Note that the current inline `<script>` in both shells
(`src/web.rs:447-571`, `src/web.rs:626-797`) makes a strict
`script-src 'self'` impossible without first extracting the loader to
a real `.js` route. That extraction is the prerequisite refactor.

## Suggested next step

- File a P1 bead under the same security-hardening epic as
  [[2026-05-17-vu-nqj-html-body-rendered-unsanitized]]: "Send CSP +
  iframe-sandbox the rendered email body".
- Separate prerequisite bead: "Move inline `<script>` in
  `generate_*_html` to a real `/app.js` route" so CSP can drop
  `'unsafe-inline'`.
- Test: a handler-level smoke test asserting `CSP`, `XFO`, and
  `X-Content-Type-Options` headers are present on `/` and
  `/api/current-email`.
