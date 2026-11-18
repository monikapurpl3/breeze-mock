# packaging/macos/ — cross-building for macOS

`packaging/build.sh` builds both Mac binaries on the workstation with
`cargo zigbuild`, like the Linux ones. No Mac and no Apple SDK are involved.
They are plain programs, not packages: the μCore deployer downloads them to
run simulated units next to an emulated board.

## The one thing zig does not have: CoreFoundation

zig carries macOS's C library (`libSystem`, `libiconv`) but no frameworks.
One dependency needs one: `chrono`'s local clock goes through
`iana-time-zone`, which asks CoreFoundation for the system time zone. That is
seven functions, so linking stops at `unable to find framework 'CoreFoundation'`
and nowhere else.

`CoreFoundation.framework/CoreFoundation.tbd` is a **link stub written by
hand** (the same one Breeze Core, breeze-token and the μCore deployer use), not
a copy of anything from Apple's SDK. It only says that the framework at its
real install name,
`/System/Library/Frameworks/CoreFoundation.framework/Versions/A/CoreFoundation`,
exports those seven symbols, so the linker can record the dependency. At run
time macOS loads its own CoreFoundation, which is on every Mac.

If `iana-time-zone` ever uses another function, the link fails, naming the
missing symbol: add it to the list. The symbols it uses today:

```sh
grep -ohE '\bCF[A-Za-z]+\b' ~/.cargo/registry/src/*/iana-time-zone-*/src/tz_darwin.rs | sort -u
```

`ctrlc` is held at 3.4 in `Cargo.toml` for the same reason: 3.5 links
Objective-C and Foundation on macOS, which zig does not have either.

## What comes out

- **`macos-arm64`** (Apple silicon) and **`macos-x86_64`** (Intel), needing
  **macOS 13 or newer**, which is zig's floor.
- zig's linker gives the arm64 one an *ad-hoc* signature, which is enough to
  run it. It is not a Developer ID: a copy downloaded with a browser is
  quarantined, and Gatekeeper refuses it until that is cleared
  (`xattr -d com.apple.quarantine breeze-mock`). A copy fetched by a program,
  as the deployer fetches it, is not quarantined.
- They load only what every Mac has: `libSystem`, `libiconv`, `libcharset` and
  CoreFoundation. Check with
  `llvm-objdump --macho --dylibs-used packaging/out/breeze-mock-VERSION-macos-arm64`.
- **Not yet run on a Mac.** Built and inspected only.
