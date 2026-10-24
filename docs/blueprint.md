# Blueprint reference

A blueprint is one XML file that describes a whole mock world: the weather,
the rooms, the network, the air conditioners in it, and the Breeze Core
servers that manage them. `breeze-mock check` validates one;
`breeze-mock run` brings it to life.

The format is strict on purpose. Every element has a fixed place and a fixed
number of occurrences, every attribute is either understood or refused, and
values have one spelling. A mistake is reported with its file, line and
column, and a misspelt name gets a suggestion, so a blueprint that loads does
what it says. The same rules are in [`schema/blueprint.xsd`](../schema/blueprint.xsd)
for editors and `xmllint`; breeze-mock also checks what XSD 1.0 cannot
express (listed at the [end](#checked-beyond-the-schema)).

This page is checked against the schema by `tests/docs.rs`: every element and
every attribute in the schema is listed here, and nothing else is.

```xml
<?xml version="1.0" encoding="UTF-8"?>
<blueprint xmlns="urn:breeze-mock:blueprint:1" version="1" name="minimal">
  <network>
    <segment name="lo" cidr="127.66.0.0/24" mode="loopback"/>
  </network>
  <unit key="bedroom" segment="lo" ip="127.66.0.2">
    <epithets name="Bedroom"/>
    <v3 token="auto" key="auto"/>
  </unit>
  <server name="home">
    <build profile="4.3.2"/>
    <listen address="127.0.0.1" port="18420"/>
    <manage unit="bedroom"/>
  </server>
</blueprint>
```

More in [`examples/`](../examples): `minimal.xml`, `flat.xml` (a whole flat
with a building, weather, several servers, TLS and faults), `bench.xml`
(units that misbehave like real ones), `models.xml` (a library).

## Rules for every file

- It starts with `<?xml version="1.0" encoding="UTF-8"?>`. No DTD, no
  processing instructions, no text outside elements.
- Every element is in the namespace `urn:breeze-mock:blueprint:1`, declared
  on the root.
- Children come in the order this page lists them. An element listed as
  "once" may appear at most once; "required" ones must appear.
- An attribute that is not listed for an element is an error, not ignored.
- Values have exactly one spelling: `true`/`false`, not `yes` or `1`; `30s`,
  not `30 s`; lowercase hex.

### Values

| Kind | Spelling | Examples |
|---|---|---|
| name | lowercase letters, digits and inner hyphens, starting with a letter, at most 32 | `living-room`, `bm1` |
| text | anything printable, no leading or trailing space | `Living room` |
| duration | a number and `ms`, `s`, `m`, `h` or `d` | `720ms`, `1.5s`, `3m` |
| rate | a fraction, a decimal or a percentage, at most 1 | `1/30`, `0.05`, `5%` |
| watts | a number and `W` or `kW` | `350W`, `3.5kW` |
| range | low-high, in °C | `16-30` |
| clock | `HH:MM`, 24-hour; `24:00` is midnight at the end of the day | `07:30` |
| hex | lowercase, an even number of digits | `0a1b` |

## `<blueprint>`

The root. Its children, in order: `<include>` (any number), `<environment>`
(once), `<building>` (once), `<network>` (once), `<model>` (any number),
`<unit>` (any number), `<server>` (any number), `<console>` (once).

| Attribute | Value | Default | |
|---|---|---|---|
| `version=` | `1` | required | The format version. |
| `name=` | name | required | Names the run, its data directory and its certificate authority. |
| `seed=` | whole number | `0` | Everything `auto` is derived from the seed and the item's name: device ids, serials, V3 tokens and keys, API keys. The same seed gives the same values every run; change it for a different set. Also seeds the simulation's randomness (dropped requests, noise, faults). |

## `<include>`

Loads the `<model>`s of a [`<library>`](#library) file, so several blueprints
can share them.

| Attribute | Value | Default | |
|---|---|---|---|
| `href=` | text | required | A path, relative to the blueprint. A file may be included once. |

## `<library>`

The root of an included file: one or more `<model>`s, nothing else.
`breeze-mock check` validates a library on its own too.

| Attribute | Value | Default | |
|---|---|---|---|
| `version=` | `1` | required | The format version. |

## `<environment>`

The weather, and how fast simulated time runs. Exactly one of `<constant>`,
`<daily>` or `<hourly>`. Without `<environment>`, it is 20 °C outdoors and
time runs at real speed.

| Attribute | Value | Default | |
|---|---|---|---|
| `time-scale=` | positive number, at most 3600 | `1` | Simulated seconds per real second. `60` makes an hour pass in a minute: rooms warm and cool, and `<sleepy>` windows come round, that much faster. The console can change it while running, including to 0 (paused). |
| `tick=` | duration, 50ms to 60s | `1s` | How often the simulation steps. |

### `<constant>`

| Attribute | Value | Default | |
|---|---|---|---|
| `outdoor=` | decimal, °C | required | The outdoor temperature, always. |

### `<daily>`

A sine-shaped day.

| Attribute | Value | Default | |
|---|---|---|---|
| `mean=` | decimal, °C | required | The day's average. |
| `swing=` | non-negative decimal, °C | required | How far above and below the mean it goes. |
| `warmest=` | clock | required | When it is warmest; the coldest is twelve hours later. |

### `<hourly>`

Two or more `<at>` points in increasing time order, joined by straight lines
and wrapping round midnight.

### `<at>`

| Attribute | Value | Default | |
|---|---|---|---|
| `time=` | clock | required | |
| `outdoor=` | decimal, °C | required | |

## `<building>`

Rooms for the units to heat and cool. One or more `<floor>`s, each with one
or more `<room>`s. A unit placed in no room gets a room of its own, 20 m² at
21 °C.

| Attribute | Value | Default | |
|---|---|---|---|
| `name=` | text | required | |

### `<floor>`

| Attribute | Value | Default | |
|---|---|---|---|
| `name=` | text | required | |
| `level=` | integer | its position, from 0 | |

### `<room>`

Each room is a heat balance: the units' output, people and appliances, the
sun, and loss to outdoors through the walls.

| Attribute | Value | Default | |
|---|---|---|---|
| `id=` | name | required | What a unit's `room=` refers to; unique in the blueprint. |
| `name=` | text | required | |
| `area=` | positive number, m² | required | |
| `height=` | positive number, m | `2.5` | |
| `insulation=` | `poor`, `average`, `good`, `passive`, or a figure like `85W/K` | `average` | Heat loss per kelvin of difference to outdoors. The words are 6, 3.5, 2 and 0.8 W/K per m² of floor. |
| `start=` | decimal, °C | `21` | The temperature when the run starts. |
| `gains=` | watts | `0W` | Constant gains: people, appliances. |
| `sun=` | watts | `0W` | Gain from the sun at noon; it rises from 06:00 and sets at 18:00 (simulated time). |

## `<network>`

Where the emulated units live. One or more `<segment>`s; segments may not
overlap.

### `<segment>`

| Attribute | Value | Default | |
|---|---|---|---|
| `name=` | name | required | What a unit's `segment=` refers to. |
| `cidr=` | IPv4 network, /8 to /30 | required | Every unit on the segment has an address inside it. |
| `mode=` | `loopback`, `dummy`, `attach`, `existing` | required | How the addresses come to exist; see below. |
| `interface=` | name, at most 15 characters | `bm-` and the segment's name, for `dummy` | For `dummy`: the interface to create. For `attach`: the existing interface to add addresses to (required). Not allowed for `loopback` and `existing`. |
| `discovery=` | `true`, `false` | `true` | Whether the units answer discovery (UDP 6445 and 20086), so Breeze Core's scan finds them. |

The modes:

- **`loopback`**: inside 127.0.0.0/8. Linux and Windows answer on the whole
  range without any setup, so nothing is created. Reachable only from this
  machine, and Breeze Core will not scan loopback, so a real Breeze Core
  reaches these units through an [exported config](running.md#a-real-breeze-core-and-emulated-units).
- **`dummy`** (Linux): a dummy interface is created with every unit's
  address, and removed when the run ends. Private ranges only (10/8,
  172.16/12, 192.168/16), which is also all Breeze Core will scan. Needs
  `CAP_NET_ADMIN`.
- **`attach`** (Linux): the units' addresses are added to an interface that
  already exists, for example the one on your LAN, so other machines can
  reach the units. Removed when the run ends. Private ranges only. Needs
  `CAP_NET_ADMIN`.
- **`existing`**: addresses this machine already has; nothing is created or
  removed. This is how units reach the LAN on Windows: give each unit the
  PC's own LAN address and a port of its own. A unit's address that the
  machine lacks is reported before anything starts.

## `<model>`

A kind of unit, for `<unit model="...">` to start from: the same elements a
unit has, all optional, in the same order (`<identity>`, `<hardware>`,
`<capabilities>`, `<state>`, `<epithets>`, `<behaviour>`). A unit's own
element replaces the model's element of the same name; `<epithets>` add up
instead (traits appended, labels joined). A model's `<epithets>` carry no
`name=` or `nickname=`, and its `<identity>` no `serial=` or `ssid=`.

| Attribute | Value | Default | |
|---|---|---|---|
| `name=` | name | required | Unique among the blueprint's models and its libraries'. |

## `<unit>`

One emulated air conditioner, answering the real LAN protocol at its address.
Its children, in order: `<epithets>` (required), `<identity>` (once),
`<v3>` or `<v2>` (one of them, required), `<hardware>`, `<capabilities>`,
`<state>`, `<behaviour>` (each once).

| Attribute | Value | Default | |
|---|---|---|---|
| `key=` | name | required | The unit's handle in the blueprint, the console and the logs. |
| `id=` | `auto` or 1 to 15 digits | `auto` | The device id Breeze Core sees. |
| `model=` | name | none | A `<model>` to start from. |
| `segment=` | name | required | The `<segment>` it is on. |
| `ip=` | IPv4 | required | Inside the segment. Units may share an address when their ports differ. |
| `room=` | name | none | The `<room>` it heats and cools. |

### `<epithets>`

A unit's names, and its traits: how it behaves beyond the textbook. The
traits are child elements, each at most once and in the order of the
[traits table](#traits), except `<sleepy>`, which may repeat.

| Attribute | Value | Default | |
|---|---|---|---|
| `name=` | text | required on a unit | What Breeze Core and the apps call it. Not allowed on a model. |
| `nickname=` | name | none | A second handle for the console; unique in the blueprint. Not allowed on a model. |
| `labels=` | names, space-separated | none | Free words for grouping; the console lists them. |

#### Traits

Each trait is an element inside `<epithets>`. A trait sets [`<behaviour>`](#behaviour)
settings; `<behaviour>` itself is applied after the traits, so it can adjust
one.

| Trait | Attributes | Effect |
|---|---|---|
| `<realistic>` | | Everything measured on real units at once: replies after 720 ms, 1 request in 30 dropped, idle connections closed after 30 s, flaps held for 3 minutes of heating warm-up, eco only in cool, flaps ignored while off, the first request after a handshake ignored for 1 s. |
| `<flaky>` | `rate=` (`1/30`) | Drops this share of requests without an answer; the connection stays usable. |
| `<slow>` | `delay=` (`720ms`), `jitter=` (`0ms`) | Waits before every reply, plus a random part up to `jitter=`. |
| `<hangs-up>` | `after=` (`30s`) | Closes a connection idle this long. |
| `<warmup-lock>` | `for=` (`3m`) | After switching on or into heat, holds the flaps (and refuses flap changes) this long. |
| `<eco-cool-only>` | | Refuses eco outside cool. |
| `<no-flaps-when-off>` | | Ignores flap changes while switched off. |
| `<no-turbo-in>` | `modes=` (required) | Refuses turbo in these modes. |
| `<sleepy>` | `from=` (required), `to=` (required) | Does not answer at all between these times of simulated day; may wrap midnight. Several windows are allowed. |
| `<forgetful>` | `every=` (required) | Forgets its session this often, forcing a new handshake. |
| `<estranged>` | | Refuses every handshake, as a unit paired to another account does. |
| `<deaf-after-handshake>` | `for=` (`1s`) | Ignores the first request after each handshake for this long. |
| `<noisy-sensor>` | `amplitude=` (`0.3`) | Adds random noise of up to this many °C to its temperature readings. |
| `<laggy>` | `lag=` (required) | Reports its state this late: a change shows up in readings only after the lag. |

### `<identity>`

How the unit introduces itself in discovery and in its replies. All optional.

| Attribute | Value | Default | |
|---|---|---|---|
| `model=` | text | the model's name in capitals, else `MOCK-AC` | The model number. |
| `serial=` | 32 of A-Z and 0-9 | derived | Units only. |
| `ssid=` | `net_` + type + `_` + up to 8 letters or digits | `net_ac_` and the serial's 25th to 28th characters | Units only. Discovery reads the appliance type from it. |
| `firmware=` | text | `breeze-mock` | |
| `type=` | `0x` and two hex digits | `0xac` | The appliance type. Breeze Core manages `0xac`. |

### `<v3>` and `<v2>`

The LAN protocol, and with it the credentials: a `<v2>` unit has none to give.

#### `<v3>`

The protocol of current units: a token handshake, then an encrypted session.

| Attribute | Value | Default | |
|---|---|---|---|
| `port=` | port | `6444` | |
| `token=` | `auto` or 128 hex digits | required | The device token Breeze Core needs. `auto` derives one from the seed and the unit's key. |
| `key=` | `auto` or 64 hex digits | required | The device key, likewise. |

#### `<v2>`

Older units: no handshake, no credentials.

| Attribute | Value | Default | |
|---|---|---|---|
| `port=` | port | `6444` | |

### `<hardware>`

The unit's capacity and sensors, for the simulation.

| Attribute | Value | Default | |
|---|---|---|---|
| `cooling=` | watts | required | Full cooling output. Without `<hardware>`: 3.5 kW. |
| `heating=` | watts | required | Full heating output. Without `<hardware>`: 3.8 kW. |
| `indoor-sensor=` | `true`, `false` | `true` | Whether it reports an indoor temperature. |
| `outdoor-sensor=` | `true`, `false` | `true` | Whether it reports an outdoor temperature. |
| `indoor-offset=` | decimal, °C | `0` | Added to the room's true temperature, as a badly placed sensor reads. |
| `outdoor-offset=` | decimal, °C | `0` | Likewise for outdoors. |

### `<capabilities>`

What the unit says it can do when Breeze Core asks, and what its firmware
then accepts. Modes, axes and fan steps it lacks are kept as they were when
asked for, as real firmware does. Without `<capabilities>`, a unit has every
mode, both flaps, low/medium/high/auto fan, eco, turbo in cool and heat,
display and buzzer control, Fahrenheit, half degrees and 17-30 °C.

| Attribute | Value | Default | |
|---|---|---|---|
| `modes=` | some of `auto cool dry heat fan-only` | required | |
| `swing=` | `none`, or `vertical` and/or `horizontal` | required | The flaps it has. |
| `fan=` | some of `silent low medium high auto` | required | Its fan steps. |
| `turbo=` | `none`, or `cool` and/or `heat` | `none` | The modes it has turbo in. |
| `fan-custom=` | `true`, `false` | `false` | A fan speed in percent rather than steps. |
| `eco=` | `true`, `false` | `false` | |
| `freeze-protection=` | `true`, `false` | `false` | |
| `display=` | `true`, `false` | `false` | Whether its display can be switched off. |
| `humidity=` | `true`, `false` | `false` | Whether it reports humidity. |
| `buzzer=` | `true`, `false` | `false` | Whether its beep can be silenced. |
| `fahrenheit=` | `true`, `false` | `true` | |
| `cool=` | range | `17-30` | Setpoints in cool. |
| `auto=` | range | `17-30` | Setpoints in auto. |
| `heat=` | range | `17-30` | Setpoints in heat. |
| `half-degrees=` | `true`, `false` | `true` | |
| `pages=` | `1`, `2` | `1` | How many pages its capability answer spans, as units differ. |

Capability records the attributes do not cover can be added raw, any number:

#### `<record>`

| Attribute | Value | Default | |
|---|---|---|---|
| `id=` | `0x` and four hex digits | required | The record id. |
| `value=` | hex, 1 to 32 bytes | required | Its bytes. |

### `<state>`

How the unit is set when the run starts. Without it: off, cool, 24 °C, fan
auto.

| Attribute | Value | Default | |
|---|---|---|---|
| `power=` | `on`, `off` | required | |
| `mode=` | `auto`, `cool`, `dry`, `heat`, `fan-only` | required | One its capabilities list. |
| `target=` | 16 to 30, whole or half | required | °C. |
| `fan=` | `silent`, `low`, `medium`, `high`, `full`, `auto` | `auto` | |
| `swing=` | `off`, `vertical`, `horizontal`, `both` | `off` | |
| `eco=` | `true`, `false` | `false` | |
| `turbo=` | `true`, `false` | `false` | |
| `sleep=` | `true`, `false` | `false` | |
| `fahrenheit=` | `true`, `false` | `false` | Whether its own display shows °F. |
| `display=` | `true`, `false` | `true` | |
| `humidity=` | 0 to 100 | `40` | The humidity it reports, if it reports one. |
| `error=` | 0 to 255 | `0` | An error code to show from the start; the console can set one later. |

### `<behaviour>`

The settings the [traits](#traits) are made of, given directly. Applied after
the traits, so `<behaviour>` wins. Without either, a unit answers at once,
never drops a request and never hangs up.

| Attribute | Value | Default | |
|---|---|---|---|
| `reply-delay=` | duration | `0ms` | As `<slow delay>`. |
| `jitter=` | duration | `0ms` | As `<slow jitter>`. |
| `drop-rate=` | rate | `0` | As `<flaky rate>`. |
| `idle-hangup=` | duration or `never` | `never` | As `<hangs-up after>`. |
| `heat-warmup=` | duration or `never` | `never` | As `<warmup-lock for>`. |
| `eco-cool-only=` | `true`, `false` | `false` | As `<eco-cool-only>`. |
| `no-flaps-when-off=` | `true`, `false` | `false` | As `<no-flaps-when-off>`. |
| `reject-handshake=` | `true`, `false` | `false` | As `<estranged>`. |
| `post-handshake-deafness=` | duration | `0ms` | As `<deaf-after-handshake for>`. |
| `session-lifetime=` | duration or `never` | `never` | As `<forgetful every>`. |
| `sensor-noise=` | `0` or a positive number, °C | `0` | As `<noisy-sensor amplitude>`. |
| `report-lag=` | duration | `0ms` | As `<laggy lag>`. |

## `<server>`

A mock Breeze Core: Breeze Core's own HTTP layer (from the release named in
the profile below) with its own store, behind a front that makes it the
release the blueprint asks for, adds faults and latency, serves TLS and picks
the server by hostname. Its children, in order: `<build>` (required),
`<host>` (once), `<listen>` (one or more), `<domain>` (any number),
`<pairing>` (once), `<manage>` (any number), `<fault>` (any number).

| Attribute | Value | Default | |
|---|---|---|---|
| `name=` | name | required | Unique in the blueprint. |
| `api-key=` | `auto` or 16 to 128 of A-Z a-z 0-9 - _ | `auto` | The server's API key. `auto` derives one from the seed and the name; `breeze-mock export breeze-core-config` writes it out. |
| `latency=` | duration | `0ms` | Added before every answer. |
| `state-dir=` | text | none | A directory, relative to the blueprint, that keeps the server's pairings, programs and timers between runs. Without it they last one run. Units and the API key come from the blueprint every run either way. |

### `<build>`

Which release of Breeze Core the server is. Routes the release did not have
answer 404, and fields it did not send are left out; see
[how close a profile is](running.md#how-close-a-profile-is).

| Attribute | Value | Default | |
|---|---|---|---|
| `profile=` | `4.3.2`, `4.3.1`, `4.3.0`, `4.2.0`, `4.1.1`, `4.1.0`, `4.0.2`, `3.2.0` | required | The release. Its version, commit and features are the real ones. |
| `version=` | three whole numbers | the profile's | The version reported, to test a client against a release that does not exist yet. |
| `commit=` | 7 to 40 hex digits | the profile's | |
| `add=` | features, space-separated | none | Features to add to the profile's. |
| `remove=` | features, space-separated | none | Features to take away; their routes then answer 404. |
| `auth-versions=` | `1`, `2`, `1 2` or `2 1` | the profile's | The pairing versions offered: 1 is a device token, 2 an Ed25519 key. Version 2 goes with the `ed25519_auth` feature. |
| `min-auth-version=` | `1`, `2` | `1` | The oldest pairing version accepted. |

The features: `batch_state`, `beep_control`, `compression`, `config_api`,
`control_feedback`, `delete_unit`, `device_pairing`, `ed25519_auth`,
`live_stream`, `metrics`, `programs`, `sleep_timer`, `system_info`,
`timer_at`, `unit_capabilities`, `unit_history`, `unit_scan`, `whoami`.

### `<host>`

The machine `/api/system` describes, in place of the one breeze-mock runs on.
Without `<host>`, the hostname is the server's name and `.mock`, and
everything else takes the defaults below.

| Attribute | Value | Default | |
|---|---|---|---|
| `hostname=` | hostname | required | |
| `os=` | text | `AlmaLinux 9.6 (Sage Margay)` | As os-release's `PRETTY_NAME`; the distribution id and version are read from it. |
| `kernel=` | text | `5.14.0-570.el9.x86_64` | |
| `arch=` | `x86_64`, `aarch64`, `armv7l`, `riscv64`, `ppc64le`, `s390x` | `x86_64` | |
| `init=` | `systemd`, `openrc`, `runit`, `s6`, `procd`, `rc.d`, `launchd`, `windows-sc`, `termux-services`, `none` | `systemd` | The names Breeze Core itself reports. |
| `cpu=` | text | `Mock CPU @ 2.4GHz` | |
| `cpus=` | whole number | `4` | |
| `timezone=` | text | `UTC` | |
| `utc-offset=` | `+` or `-`, a whole number and `s`, `m`, `h` or `d` | `+0s` | |

### `<listen>`

Where the server answers. Several servers may share an address and port; they
are then told apart by the hostname the client asks for (the Host header, or
TLS's server name), and a name none of them has gets 421.

| Attribute | Value | Default | |
|---|---|---|---|
| `address=` | IPv4 or IPv6 | required | `127.0.0.1` for this machine only; a LAN address for phones and other machines. |
| `port=` | port | required | |
| `tls=` | `true`, `false` | `false` | HTTPS, with the certificates of the server's `<domain>`s. Needs at least one domain with a certificate. |

### `<domain>`

A hostname the server answers to. With no child it is plain HTTP; with one,
the name gets a certificate.

| Attribute | Value | Default | |
|---|---|---|---|
| `name=` | hostname | required | Unique in the blueprint. Make it resolve to the listen address yourself (the hosts file, or the client's DNS). |

#### `<self-signed>`

A certificate from the blueprint's own certificate authority, which
`breeze-mock export ca` writes out for clients to trust. The authority is
created on first use and kept in the data directory, so a client trusts it
once.

#### `<files>`

A certificate of your own.

| Attribute | Value | Default | |
|---|---|---|---|
| `cert=` | text | required | A PEM certificate chain, relative to the blueprint. |
| `key=` | text | required | Its PEM private key. |

### `<pairing>`

How the server pairs clients, Breeze Core's own way: an API key, then a code
approved by an admin, then a device token (or key) of the client's own.

| Attribute | Value | Default | |
|---|---|---|---|
| `approve=` | `manual`, `auto`, `never` | `manual` | `manual`: approve from the console, or with an approval call from the LAN as on a real server. `auto`: every code is approved half a second after it is shown. `never`: approval is refused, so the client waits until the code expires. |
| `code-ttl=` | duration, at least 5s | `60s` | How long a pairing code lives. |
| `token-ttl=` | duration | `3650d` | How long a device token lives, in whole days. |
| `lan-only=` | `true`, `false` | `true` | Breeze Core lets only clients on a private network approve or pair. `false` treats every client as local. |
| `skew=` | duration | `300s` | How far a signed request's clock may be off. |

#### `<device>`

A client already paired when the run starts, so tests need no pairing step.

| Attribute | Value | Default | |
|---|---|---|---|
| `label=` | text | required | What the server lists it as. |
| `token=` | 32 to 128 of A-Z a-z 0-9 - _ | required | Its device token. Use a made-up one; it is written to the server's store. |

### `<manage>`

| Attribute | Value | Default | |
|---|---|---|---|
| `unit=` | name | required | A unit this server manages. A unit may be managed by several servers. |

### `<fault>`

A request that goes wrong on purpose, checked before anything else. The first
fault that matches and fires decides the answer.

| Attribute | Value | Default | |
|---|---|---|---|
| `method=` | `GET`, `POST`, `PUT`, `PATCH`, `DELETE` | any | |
| `path=` | text starting with `/`, may end in `*` | required | The exact path, or with `*`, every path starting with it. The query is not part of it. |
| `rate=` | rate | `1` | How often it fires. |
| `status=` | 100 to 599 | `503` | |
| `body=` | text | `{"detail": "<reason>"}` | The answer's body, sent as JSON. |
| `delay=` | duration | `0ms` | Waits this long first. |
| `hang-up=` | `true`, `false` | `false` | Closes the connection without a word, after the delay. Takes no `body=`. |

## `<console>`

A page and a small JSON API for watching and changing the running world; see
[running.md](running.md#the-console). It has no login: keep it on loopback.

| Attribute | Value | Default | |
|---|---|---|---|
| `address=` | IPv4 or IPv6 | required | |
| `port=` | port | required | |

## Checked beyond the schema

breeze-mock refuses these too, with a message saying why; the schema cannot
express them:

- numbers out of range inside a pattern: a rate above 1, a range whose low
  end is above its high end, a time scale above 3600, a tick outside 50 ms to
  60 s, a code TTL under 5 s;
- a unit's address outside its segment, or two units on the same address and
  port;
- overlapping segments, and dummy or attach segments outside the private
  ranges, loopback ones outside 127/8;
- a unit's starting mode that its capabilities lack, and capability sets the
  protocol cannot express (the message lists what it can);
- the same word twice in a list, `<at>` points out of time order, `<sleepy>`
  windows that start and end at the same time;
- models defined twice across the blueprint and its libraries, and libraries
  included twice;
- a feature added that the profile already has, or removed that it lacks;
  auth version 2 without `ed25519_auth`, or the other way round;
  `min-auth-version=` not among `auth-versions=`;
- listeners that collide with each other or with the console, `tls="true"`
  without a certificate, a domain with both `<self-signed>` and `<files>`;
- a fault with both `hang-up="true"` and `body=`;
- `clock-offset=` on a server, removed in favour of setting the client's
  clock off (a mock server runs Breeze Core's own code, which keeps the
  system clock).
