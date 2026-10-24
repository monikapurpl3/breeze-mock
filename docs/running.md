# Running breeze-mock

How to install it, the commands, and recipes for the usual jobs. The format
of the blueprint itself is in [blueprint.md](blueprint.md).

## Install

**Windows** (x86_64): unpack `breeze-mock-VERSION-windows-x86_64.zip`
anywhere. It holds `breeze-mock.exe`, the schema, the examples and these
docs. Nothing else is needed: no installer, no runtime.

**RPM-based Linux** (Fedora, RHEL, AlmaLinux, Rocky, openSUSE), x86_64 or
aarch64:

```sh
sudo dnf install ./breeze-mock-VERSION-1.x86_64.rpm
```

It installs one static executable, `/usr/bin/breeze-mock`; a service,
`breeze-mock.service`, which is not enabled; a default blueprint,
`/etc/breeze-mock/blueprint.xml`, kept across upgrades once edited; the
schema and examples under `/usr/share/breeze-mock/`; and these docs under
`/usr/share/doc/breeze-mock/`.

**From source**, anywhere Rust builds: `cargo build --release`.

## Commands

```text
breeze-mock check <blueprint.xml>
breeze-mock run <blueprint.xml> [-v | -q]
breeze-mock export breeze-core-config <blueprint.xml> [--server NAME] [-o FILE]
breeze-mock export ca <blueprint.xml> [-o FILE]
breeze-mock version
```

- **`check`** validates a blueprint (or a library of models) and summarises
  it. Nothing is started.
- **`run`** starts everything the blueprint describes and prints where each
  part answers, then runs until Ctrl-C (or SIGTERM), when it removes any
  interfaces and addresses it added. `-v` logs every request and reply; `-q`
  only errors.
- **`export breeze-core-config`** writes a `config.json` for a real Breeze
  Core: every unit with its address, port, token and key, and an API key.
  With `--server NAME`, only that server's units and its API key. The tokens
  and keys come from the blueprint's seed, so the file keeps working from one
  run to the next. Without `-o`, it is printed.
- **`export ca`** writes the certificate authority behind the blueprint's
  `<self-signed/>` domains, for clients to trust.

## Where things are kept

- **Each blueprint's certificate authority**, created the first time it is
  needed and kept so clients trust it once: in breeze-mock's data directory,
  under the blueprint's name. The data directory is `BREEZE_MOCK_DATA` if
  set; else `%LOCALAPPDATA%\breeze-mock` on Windows; else
  `$XDG_DATA_HOME/breeze-mock`, or `~/.local/share/breeze-mock`. The service
  uses `/var/lib/breeze-mock`.
- **Each mock server's Breeze Core store** (its units, API key, paired
  devices, programs, timers): in a directory under the system's temporary
  directory, removed when the run ends, unless the server has a
  `state-dir=`. Units and the API key are written from the blueprint at every
  start either way.

breeze-mock writes nowhere else. In particular it never touches a real Breeze
Core's configuration: a real Breeze Core uses an exported file only when
pointed at it.

## A real Breeze Core and emulated units

To try a Breeze Core build, a configuration or a client against air
conditioners that are not your real ones, and without disturbing the Breeze
Core you already run.

**On one machine.** Run the units on loopback (the default blueprint,
[`minimal.xml`](../examples/minimal.xml)), export a config into a directory
of its own, and start a second Breeze Core on that directory and another
port:

```sh
breeze-mock run examples/minimal.xml
mkdir -p ~/bc-test
breeze-mock export breeze-core-config examples/minimal.xml -o ~/bc-test/config.json
AC_CONFIG_DIR=~/bc-test BREEZE_PORT=18430 breeze-core serve
```

On Windows (PowerShell):

```powershell
.\breeze-mock.exe export breeze-core-config examples\minimal.xml -o $HOME\bc-test\config.json
$env:AC_CONFIG_DIR = "$HOME\bc-test"; $env:BREEZE_PORT = "18430"; breeze-core serve
```

That Breeze Core is a real one, with its own API key (`export` prints it):
pair the app or `breezectl` with it at port 18430, and every control lands
on an emulated unit. The console shows the unit change, and the room
respond. Your usual Breeze Core, on its own directory and port 8420, never
sees any of it.

**Across the LAN.** To test Breeze Core on another device (a router, a
single-board computer, a NAS) against emulated units, the units must be on
an address that device can reach, and then its own scan finds them:

- on Windows, use an `existing` segment with this PC's LAN address and a
  port per unit, as in [`lan.xml`](../examples/lan.xml);
- on Linux, either the same, or an `attach` segment that gives each unit an
  address of its own on the LAN interface.

On the other device, use a Breeze Core with a configuration directory of its
own, then scan and add the units as you would real ones. The units' tokens
and keys are needed for V3 units; `breeze-mock export breeze-core-config`
gives them, or copy the exported `config.json` to that device and point its
`AC_CONFIG_DIR` at it.

**Discovery on one Linux machine.** A `dummy` segment (on a private range)
gives the units an interface of their own, which Breeze Core's scan covers
like any other: see the [service](#the-service) or run as root, since it
needs `CAP_NET_ADMIN`.

## Real clients and a mock server

To test the Android app, the web panel, `breezectl` or a script against Breeze Core releases you do not run, or against a server that
misbehaves on cue.

- A `<server>` with `<build profile="...">` is that release: its version,
  commit, features and routes. Several can run at once, on several ports or
  on one port by hostname.
- `<listen address="127.0.0.1">` for clients on this machine; this machine's
  LAN address for phones.
- Pairing works as on a real server. `approve="auto"` approves every code;
  `manual` waits for the console (or an approval from the LAN, as usual);
  `never` lets a client sit on a code until it expires. A `<device>` under
  `<pairing>` is a client already paired, for scripts and tests.
- `<fault>` makes chosen requests fail with a status, hang, or hang up;
  `latency=` slows every answer.
- `tls="true"` with a `<domain>` serves HTTPS. With `<self-signed/>`, trust
  the blueprint's authority on the client (`breeze-mock export ca`), and make
  the domain resolve to the listen address (the hosts file, or the client's
  DNS).

## Both, wired together

That is what a blueprint is: a mock server's `<manage>`d units are the
emulated ones, so a client's control reaches a unit over the real protocol,
and the unit's state comes back the way it would from hardware, misbehaviour
included. A real Breeze Core on an exported config can drive the same units
at the same time.

## The console

`<console address="127.0.0.1" port="18499"/>` serves a page that shows the
rooms, the units and the servers, and changes the world while it runs: the
outdoor temperature, the speed of time, a room's temperature, a unit offline
or showing an error code, and pending pairing codes to approve.

The page is a front for a JSON API, for scripts and tests:

| Request | Body | |
|---|---|---|
| `GET /api/world` | | Rooms, units, servers, the weather, statistics. |
| `POST /api/outdoor` | `{"celsius": 31.5}` or `{"celsius": null}` | Hold the outdoor temperature, or follow the weather again. |
| `POST /api/time-scale` | `{"scale": 60}` | Simulated seconds per second; 0 pauses. |
| `POST /api/rooms/{id}/temperature` | `{"celsius": 26}` | |
| `POST /api/units/{key}/offline` | `{"offline": true}` | The unit stops answering, as if unplugged. |
| `POST /api/units/{key}/error` | `{"code": 5}` | The error code it reports; 0 clears it. |
| `POST /api/servers/{name}/approve` | `{"code": "ABCD-EFGH"}` | Approve a pairing code, as an admin. |

A unit can be named by its key or its nickname.

```sh
curl -s localhost:18499/api/units/bedroom/offline -d '{"offline": true}'
```

The console has no login, so keep it on loopback: anyone who can reach it can
change the weather, take units offline and approve pairings on the mock
servers. `run` points it out when a blueprint does otherwise.

## The service

On Linux, `breeze-mock.service` runs `/etc/breeze-mock/blueprint.xml`. It is
installed disabled; check the blueprint, then:

```sh
sudo breeze-mock check /etc/breeze-mock/blueprint.xml
sudo systemctl enable --now breeze-mock
journalctl -u breeze-mock -f
```

It runs as a dynamic user with only `CAP_NET_ADMIN` (for `dummy` and
`attach` segments) and `CAP_NET_BIND_SERVICE` (for ports below 1024), and can
write only `/var/lib/breeze-mock`: a server's `state-dir=` must be under it.
Stopping it removes the interfaces and addresses it added; a run that was
killed outright has its leftovers removed at the next start.

## How close a profile is

A mock server is not a re-creation of Breeze Core: it *is* Breeze Core's own
HTTP layer, from release 4.3.2, with Breeze Core's own pairing, store,
scheduler, timers, programs and unit polling, running against the emulated
units. A comparison test runs a real `breeze-core` 4.3.2 beside a mock server
on the same units and requires the same status, headers and body for each
request it compares, and the same shape where readings move.

Older profiles are 4.3.2 with what the release lacked taken away: its routes
answer 404, fields it did not send (`not_applied`, a timer's `kind`) are
removed, options it did not have (a timer `at` a time, beep control,
compression) are not honoured, and `/api/version` and `/api/system` give its
version, commit and features. What is not taken away is the wording of
errors and the order of fields, which are 4.3.2's. A client that depends on
an older release's exact error text needs that release itself.

`/api/system` describes the blueprint's `<host>`, never the machine
breeze-mock runs on: its paths are Breeze Core's packaged ones, its libc is
static musl, and the connection it reports is a direct one (or, on a TLS
listener, one through a TLS-terminating proxy, which is how a real Breeze
Core serves HTTPS).

## Limits

- **Windows** has no `dummy` or `attach` segments: units are on loopback, or
  share this PC's own addresses (`existing`). The first run on a LAN address
  asks for a firewall exception.
- **Breeze Core does not scan loopback**, so units on a `loopback` segment are
  reached through an exported config, not a scan.
- **The clock** is the real one, as Breeze Core's is: a skewed server clock
  is tested by setting the client's clock off.
- **V1 units** (XML over TCP) are not emulated; neither is Midea's cloud.
