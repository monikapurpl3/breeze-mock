<div align="center">

# breeze-mock

**Air conditioners and Breeze Core servers that aren't there.**

Emulated Midea units on the real LAN protocol, and mock
[Breeze Core](https://github.com/monikapurpl3/breeze-core-native) servers of
any release, described in one XML file, for testing Breeze Core and its
clients without touching a real unit or a real server.

[![License](https://img.shields.io/badge/license-AGPL--3.0-blue)](LICENSE)
[![Runs on](https://img.shields.io/badge/runs%20on-Windows%20·%20Linux-e69138)](docs/running.md#install)
[![Breeze Core](https://img.shields.io/badge/Breeze%20Core-3.2.0%20→%204.3.2-6aa84f)](docs/blueprint.md#build)

</div>

```console
$ breeze-mock run examples/flat.xml

ready.
  units:
    Bedroom        10.77.1.21:6444  V3  id 18402913552064
    Living room    10.77.1.22:6444  V3  id 40871283010291
    Kitchen        10.77.1.23:6444  V2  id 22195700481352
  servers:
    home (Breeze Core 4.3.2)
    https://breeze.flat.test:18443/  (resolve breeze.flat.test to 127.0.0.1)
    legacy (Breeze Core 3.2.0)
    http://127.0.0.1:18441/
  console: http://127.0.0.1:18499/
  Ctrl-C stops it.
```

## Why

Breeze Core talks to hardware you only have so much of, and its clients talk
to a server you would rather not experiment on. breeze-mock stands in for
both:

- **Units that behave like units.** They speak the real V3 and V2 protocols,
  answer discovery, refuse what their firmware refuses, hold their flaps
  while the coil warms up, ignore the first request after a handshake, drop
  one request in thirty and hang up idle connections, as measured on real
  ones, or only as much of that as you ask for.
- **A room behind every unit.** Rooms warm and cool with the weather, the
  sun and the units, so a schedule or a temperature curve has something to
  work against, at up to an hour a second.
- **Servers that are Breeze Core.** A mock server is Breeze Core's own HTTP
  layer, its pairing, store, scheduler and timers, made into whichever
  release you name, from 3.2.0 to 4.3.2: the routes, fields and features it
  had, and no others. A test holds it to a real `breeze-core`'s answers.
- **Trouble on cue.** Failing or hanging requests, latency, pairings that
  are never approved, units that go offline at night or forget their session,
  TLS with a certificate authority of the blueprint's own, several servers
  on one port told apart by hostname.
- **Strict, so it does what it says.** A blueprint is XML with a schema, and
  every mistake is reported with its line and column, before anything starts.
- **Separate from your real setup.** A real Breeze Core reaches the emulated
  units through a config breeze-mock exports, in a directory of its own, so
  the server you rely on never sees them.

## Install

**Windows:** unpack `breeze-mock-VERSION-windows-x86_64.zip` and run
`breeze-mock.exe` from it. Nothing else to install.

**Fedora, RHEL, AlmaLinux, Rocky** (x86_64, aarch64):

```sh
sudo dnf install ./breeze-mock-VERSION-1.x86_64.rpm
```

That brings a service, `breeze-mock.service`, which runs
`/etc/breeze-mock/blueprint.xml` once you enable it.

**From source:** `cargo build --release`.

Then:

```sh
breeze-mock check examples/minimal.xml
breeze-mock run examples/minimal.xml
```

## Documentation

|  |  |
|---|---|
| [Running breeze-mock](docs/running.md) | the commands, and recipes: a real Breeze Core on emulated units, real clients on a mock server, testing across the LAN, the console |
| [Blueprint reference](docs/blueprint.md) | every element and attribute, with its default |
| [`examples/`](examples) | a minimal world, a whole flat, misbehaving units, a LAN setup, a model library |
| [`schema/blueprint.xsd`](schema/blueprint.xsd) | the same rules, for editors and `xmllint` |

## Honest limits

It is a test tool, not a simulator of any one model: units behave like the
ones it was measured against, which may not be yours. Older Breeze Core
releases are reproduced by taking 4.3.2's features away, so their error
messages are 4.3.2's. On Windows, units are either on loopback or share the
PC's own LAN address; giving each one an address of its own needs Linux. V1
units and Midea's cloud are not emulated.

## Security

Everything a blueprint makes up (API keys, device tokens, V3 tokens and keys)
is derived from its seed and is no secret; never put a real unit's or a real
server's credentials in a blueprint. The console has no login and belongs on
loopback. A mock server pairs and authenticates exactly as Breeze Core does,
because it is Breeze Core's code, but it is still a mock: do not expose it to
the internet.

## Licence

[AGPL-3.0-or-later](LICENSE), as Breeze Core is, whose crates it is built on.
