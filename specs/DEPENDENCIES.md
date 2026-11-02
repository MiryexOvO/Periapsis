# Dependency and standards baseline

Reference snapshot date: 2026-11-02. `package-lock.json` is the reproducible dependency
record (integrities included). `pnpm-lock.yaml` is also provided for pnpm 11.18.0.
Use `npm ci --ignore-scripts` or `pnpm install --frozen-lockfile --ignore-scripts`.

| Component | Pin | Evidence and caveat |
|---|---|---|
| MOQT target | draft-ietf-moq-transport-22 | https://datatracker.ietf.org/doc/draft-ietf-moq-transport/22/ |
| JS MOQT implementation | @moq/net 0.4.7 | https://www.npmjs.com/package/@moq/net/v/0.4.7 |
| QMux implementation | @moq/qmux 0.3.4 | https://www.npmjs.com/package/@moq/qmux/v/0.3.4 |
| WebSocket server | ws 8.22.1 | Exact version; initial older pin replaced after audit |
| Rust candidate | moq-dev/moq | Inspected f8c608a24bbad5c51b1ffa2ad7a4996e9405ac0c; not vendored or built |
| LOC / MSF | Unselected | No media interoperability claim |

Runtime source for `@moq/net` includes draft-22 support, but also a stale comment
calling it unpublished/wire-identical to draft-21. Therefore source/API availability
and successful same-library round trips are insufficient proof of draft conformance.
An independent peer and draft-22 LOCATION_FILTER compatibility remain required.

The fetched upstream Git main is not a reproducible dependency of this project.
Its examples differ from the installed npm API. Implementation follows the pinned
npm declarations and source, not current main examples.

The user subsequently installed Rust 1.99.1 (`x86_64-pc-windows-msvc`). Rust core crates
`nsp-types`, `nsp-session`, `nsp-scheduler` pass workspace check and strict Clippy.
MSVC `link.exe` and SDK were not detected; executable tests cannot yet link. The
native transport backend remains unimplemented. The TypeScript networking reference
remains runnable. A partial official binary download was not used as a backend.

Primary browser constraints:

* https://www.w3.org/TR/webtransport/ — secure context and browser-server API.
* https://www.w3.org/TR/webcodecs/ — secure-context decoding interfaces.
* https://w3c.github.io/webappsec-secure-contexts/ — localhost exceptions do not
  generally extend to ordinary remote/LAN HTTP origins.
