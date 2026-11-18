# breeze-mock -- notes for working on it

Emulated Midea air conditioners and mock Breeze Core servers, described by
one XML blueprint. Users' documentation is `README.md` and `docs/`; this is
for changing the code.

## Layout

| | |
|---|---|
| `src/blueprint/` | the XML: `xml.rs` (strict reading), `parse.rs` (every element), `model.rs` (what it becomes), `profiles.rs` (Breeze Core releases) |
| `schema/blueprint.xsd` | the same rules for editors and xmllint |
| `src/sim/` | weather, rooms, units' physics and firmware rules |
| `src/lan/` | the emulated units on the V3/V2 protocol (`device.rs`, `codec.rs`) and discovery |
| `src/api/` | mock servers: `backend.rs` (Breeze Core's breeze-http, embedded), `front.rs` (the proxy in front of it), `profile.rs` (a release's routes and fields), `tls.rs`, `http.rs` |
| `src/net.rs` | segments: dummy interfaces and attached addresses on Linux, bind checks |
| `src/console.rs`, `src/export.rs`, `src/run.rs` | the console, `export`, `run` |
| `tests/` | `blueprints.rs` (examples + 45 bad fixtures, tool and XSD), `lan.rs` (units driven by Breeze Core's own client), `api.rs` (the running mock from outside), `differential.rs` (against a real breeze-core), `docs.rs` (docs/blueprint.md against the XSD) |
| `packaging/` | `build.sh` (RPMs, the Windows zip and the bare programs), nfpm recipe, systemd unit, the packaged default blueprint, `macos/` (the CoreFoundation link stub) |

## Rules that are not obvious from the code

- **Strictness is the point.** Every element has a fixed place and count,
  every attribute is read or refused (`El::done()`), values have one
  spelling, and every error names file:line:col. A new attribute needs: the
  parser, the XSD, docs/blueprint.md, and a bad fixture if it adds a rule.
  `tests/docs.rs` and `tests/blueprints.rs` fail until all agree. Rules XSD
  1.0 cannot express are marked `xsd: accepts` in their fixture.
- **No attribute that does nothing.** `<host memory-mb>` and `container=`
  went because Breeze Core reports neither; `clock-offset=` went because a
  mock server keeps the system clock. A removed attribute gets an error that
  says why, not just "unknown".
- **A mock server is Breeze Core, not an imitation.** `backend.rs` starts
  breeze-http (tag `v4.3.2`, `default-features = false`) with the same four
  background threads `breeze-core serve` starts, on a free loopback port, with
  `behind_proxy` on. The front adds what a blueprint asks for and nothing
  else. Before changing what the front does to a request or a response, run
  `tests/differential.rs` against a real binary:
  `BREEZE_CORE_BIN=...\breeze-core.exe cargo test --release --test differential`.
- **Older profiles subtract.** A route outside the profile's features is a
  404 before authentication; fields and options the release lacked are
  stripped (`profile.rs`). Error wording stays 4.3.2's; the docs say so.
- **`/api/system` describes the blueprint's `<host>`**, never this machine:
  no Windows paths, no temp directories, no "behind a proxy" on a plain
  listener. `tests/api.rs` greps the answer for leaks.
- **Bumping Breeze Core**: change the tag in Cargo.toml, add the release to
  `profiles.rs` (its real commit and features, from the tag) and to the XSD's
  `profile` enumeration, then run the differential test against that
  release's binary.
- **Run directories** live in the temp directory and hold a `.lock` for the
  run's life; a start removes those whose lock nobody holds (runs that were
  killed). Windows will not delete an open file: unlock before removing.
- **Tests parse blueprints from text** (`blueprint::parse`), so they leave no
  files; `tests/api.rs` keeps one data directory per process and removes
  older ones.

## Platforms

- **Windows**: loopback and `existing` segments only. The whole 127.0.0.0/8
  is bindable without setup. Units on the LAN share the PC's address, one
  port each. The first bind on a LAN address raises a Windows Firewall
  prompt: never trigger it without telling the maintainer first, and never
  change firewall settings.
- **Linux**: `dummy` and `attach` need `CAP_NET_ADMIN` (`ip` is run for
  them, and inherits it). Stale interfaces and `<if>:bm` addresses from a
  killed run are removed at the next start.

## Building and testing

```sh
cargo test --release                          # everything but the differential
cargo clippy --release --all-targets          # keep it at zero warnings
BM_ZIG=/c/Users/Admin/zig/zig-x86_64-windows-0.16.0/zig.exe bash packaging/build.sh
```

`build.sh` needs cargo-zigbuild, zig 0.16.0 and Docker (nfpm runs in a
container; the tree goes in and the packages come out over tar, so the drive
need not be shared with Docker).

## Traps met so far

- Shell heredocs in this environment halve backslashes: write edit scripts
  with the Write tool.
- XML comments may not contain `--`: write `breeze-mock run`, not
  `xmllint --noout`, inside one.
- Examples and docs use placeholder names (Bedroom, Living room, Kitchen,
  192.168.1.50) and made-up keys, never the maintainer's units, hosts or
  tokens.
- `breeze-core diag` and `control` create `~/.config/breeze-core/cli.json`
  unless `BREEZE_CLI_PROFILE` points somewhere else; point it at a scratch
  file when testing.

## Publishing

The GitHub repository (monikapurpl3/breeze-mock) is public. The bare
programs `build.sh` makes (Linux amd64/arm64, macOS arm64/x86_64, Windows)
are published unlisted on aspic under `/breeze-mock/`, with SHA256SUMS: the
μCore deployer downloads its pinned one to run simulated units beside an
emulated board, so **a new release means a new pin there**. Each published
build is tagged here (`v1.0.1` on), which is the source its AGPL-3.0 points
to. The RPMs and the zip are not published; where, if anywhere, is the
maintainer's decision.

## macOS

Cross-built with zig and the CoreFoundation link stub in
`packaging/macos/` (its README says why); `ctrlc` is held at 3.4 because 3.5
links Objective-C and Foundation, which zig lacks. Built and inspected, not yet
run on a Mac.
