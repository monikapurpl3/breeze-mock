# breeze-mock: where it stands

Paused on 2026-09-28. This file is the hand-over: what exists, what was
decided, and what is left, in the order it should be done.

## What it is

One binary that stands in for both halves of a Breeze Core installation,
described by one XML blueprint:

- **emulated Midea air conditioners** on the real LAN protocol (V3 token
  handshake and AES session, or V2), answering state queries, controls,
  capability queries and discovery hellos, so a real Breeze Core can pair and
  drive them without hardware;
- **mock Breeze Core servers** (not built yet) that pretend to be a given
  release, for testing the app, the web panel and the other clients.

Linux only, packaged as RPMs for x86_64 and aarch64. Separate from Breeze
Core; it depends on Breeze Core's own crates (breeze-proto, breeze-auth,
breeze-store, and breeze-device for tests), pinned to the `v4.2.0` tag.

## Decisions (the maintainer's)

- Both layers in one binary.
- "Server building" is both the build identity (version, commit, features)
  and a physical building (floors, rooms) for the simulation.
- "Epithets" are both behaviour traits and names: `<epithets name nickname
  labels>` holds the names, its children are the traits.
- "Domains" are all three: hostnames the server answers (with TLS), network
  segments, and several servers in one run.
- **The XML is very, very strict** -- see below. Strictness is the priority.
- RPM-based Linux, x86_64 and ARM.

## Done

Commits 49241e6 and 12352a5; 27 tests pass (`cargo +stable test`).

- **Blueprint format** (`src/blueprint/`): namespace
  `urn:breeze-mock:blueprint:1`, required declaration, no DTD or processing
  instructions, fixed child order and counts, every attribute read or
  refused, exact value syntax, errors with file:line:col at the element they
  concern, typo suggestions. `<include>` of `<library>` files of `<model>`s.
  "auto" ids, serials, tokens, keys and API keys derived from the seed.
- **Schema** (`schema/blueprint.xsd`), identical rules for xmllint and
  editors. Shapes XSD 1.0 cannot express were changed so it can (`<v3>`/`<v2>`,
  one element per trait, `<constant>`/`<daily>`/`<hourly>`, `<self-signed/>`/
  `<files>`).
- **45 broken fixtures** (`tests/fixtures/bad/`), each stating the message the
  tool must give and whether the XSD rejects it; 13 are marked as rules XSD
  cannot express. `tests/blueprints.rs` checks the tool; `tests/schema-check.sh`
  checks the XSD (needs xmllint; ran clean in an alpine container).
- **Build profiles**: 4.2.0, 4.1.1, 4.1.0, 4.0.2, 3.2.0, with each release's
  real features and commit (checked against the tags).
- **Codec** (`src/lan/codec.rs`): the device side of breeze-proto -- state
  reports, control commands, capability pages from the XML (refusing sets the
  protocol cannot express, with the options). All tested through
  breeze-proto's own client decoders.
- **Simulation** (`src/sim/`): rooms with heat capacity and loss, weather,
  sun and gains; units that move heat like inverters; firmware rules (modes
  and axes a unit lacks are kept, the heating warm-up flap lock, eco only in
  cool, no flaps while off).
- **LAN emulator** (`src/lan/device.rs`) and **discovery**
  (`src/lan/discovery.rs`), with every trait: delay and jitter, dropped
  requests, post-handshake deafness, idle hang-up, forgotten sessions,
  refused handshakes, offline windows, noise, lag. `tests/lan.rs` drives
  emulated units with breeze-device, Breeze Core's own client.

Uncommitted-then-committed with this file: the `ctrlc` dependency, for
removing network interfaces on Ctrl-C and SIGTERM.

## Next, in order

1. **`breeze-mock run <blueprint>`**: set up segments (`src/net.rs`: dummy
   interfaces via `ip link add ... type dummy`, address aliases for attach,
   nothing for loopback/existing; remove on exit via ctrlc, and remove stale
   ones at start), start the ticker, every unit's listener, discovery.
2. **`breeze-mock export breeze-core-config <blueprint>`**: a config.json for
   a real Breeze Core pointing at the emulated units (derived tokens and keys
   stay stable across runs).
3. **Record the real API**: run a real `breeze-core` (debug build of the
   native repo) against the emulated units and capture every endpoint's
   response. That is the reference for the mock server, and for a
   differential test (adapt `packaging/test/differential.py`).
4. **The mock server** (`src/api/`): own small HTTP/1.1 server (tiny_http
   cannot pick a certificate per hostname), rustls with an SNI resolver,
   rcgen for a run CA and per-domain certificates, Host-header virtual hosts
   (421 for an unknown host when a port is shared). Routes: health, version,
   units, batch and single state, control (with `not_applied` from
   `control_feedback`), capabilities, history, stream (SSE), timers (with
   `kind` and `?kind=` from `timer_at`), programs, pairing (breeze-auth's
   EnrollmentService, Verifier, NonceCache, v1 and v2), devices, whoami,
   upgrade, config, system (from `<host>`), scan (from the segments), add,
   rename and delete units, metrics. Features not in the build profile answer
   404. Faults, latency and clock offset per server.
5. **Console** (`<console>`): inspect the world (rooms, units, stats), set
   the outdoor temperature, force a unit offline, approve pairing codes.
6. **Packaging**: `cargo zigbuild` static musl for x86_64 and aarch64, nfpm
   RPMs, a systemd unit (needs CAP_NET_ADMIN for dummy and attach segments),
   `/etc/breeze-mock/blueprint.xml`, the schema under `/usr/share`.
7. **Docs**: README (mirroring the other repos' style) and a blueprint
   reference generated from, or checked against, the XSD.
8. **Publishing**: ask first -- a GitHub repo and where the RPMs go are the
   maintainer's call.

## Traps met so far

- Git Bash rewrites an argument starting with `//` as a path; run scripts
  that take `// ---` markers with `MSYS_NO_PATHCONV=1`.
- Shell heredocs in this environment mangle `\\`; write files with the editor
  instead when escapes matter.
- Examples use placeholder names (Bedroom, Living room, Kitchen) and derived
  ids, never the maintainer's real units, hosts or tokens.
