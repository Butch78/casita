# Fresh Mac installation check

Target: `dogwood213@mini-m4p-590`, arm64, macOS 27.0 build 26A428,
UID 502. SIP and Gatekeeper assessments are enabled. No valid code-signing
identity is available in the current user's keychain.

Copied the unchanged ad-hoc-signed bundle and repository from the previous
callback experiment to `/Users/dogwood213/casita-install-test.cIgwt7`.
Strict deep signature verification passed. The extension depends only on
system frameworks/libraries. LaunchServices and PluginKit registration passed.

Mounting reports that `org.casita.native-fskit.extension.repository` is disabled.
The mount command surprisingly returns zero despite reporting failure, so
actual mount state must be checked independently.

The retained `activate.py` development probe fails when reading
`~/Library/Group Containers/group.com.apple.fskit.settings/enabledModules.plist`
with `PermissionError: [Errno 1] Operation not permitted`. No activation settings
were changed, no agents were terminated, and no filesystem remains mounted.
`activation-report.json` retains the partial result. The registered application
is retained for the next attempt after user activation in System Settings.

This is an installation experiment, not a performance benchmark. It does not
establish Developer ID distribution, notarization, successful activation, or
rootless mounting on this Mac. Next enable the extension through System Settings
and retry the mount/read/unmount checks; separately validate a release-signed
distribution artifact.

## CLI election follow-up

Tested as UID 502:

```sh
pluginkit -e use -p com.apple.fskit.fsmodule -i org.casita.native-fskit.extension.repository
pluginkit -m -A -D -v -i org.casita.native-fskit.extension.repository
mount -F -t casitarepo /Users/dogwood213/casita-install-test.cIgwt7/repository /Users/dogwood213/casita-install-test.cIgwt7/mount
```

PluginKit reports `+` (user election enabled), but mounting still reports
`Module org.casita.native-fskit.extension.repository is disabled!` and
`mount: Unable to invoke task`. Python `os.path.ismount` confirms `False`.
PluginKit election does not satisfy FSKit activation on this machine.
Apple's [FSKit sample](https://developer.apple.com/documentation/FSKit/building-a-passthrough-file-system)
documents enabling the extension in System Settings. Its
[FSClient API](https://developer.apple.com/documentation/fskit/fsclient)
exposes opening that settings panel, not a documented approval method.

## Existing production activation and privacy hypothesis

`casita-fs/src/darwin/setup.rs::enable_module` already implements programmatic
FUSE-T FSKit enablement: read this same plist with `plutil`, preserve its entries,
append the module identifier through a temporary plist, atomically persist it,
and restart the current user's `fskit_agent` after refusing any existing FSKit
mount. Native FSKit can use the same mechanism with its own identifier.

A read-only check using the production `plutil -convert json -o -` command also
fails on the new Mac. The plist is owned by dogwood213 with mode 0600; its parent
is owned by that user with mode 0755. The denial is therefore not explained by
those POSIX mode bits. Apple documents default denial for cross-team container
access in the [macOS 27 release notes](https://developer.apple.com/documentation/macos-release-notes/macos-27-release-notes).
It also documents the Remote Login option
[Allow full disk access for remote users](https://support.apple.com/guide/mac-help/allow-a-remote-computer-to-access-your-mac-mchlp1066/mac).

Privacy/container protection is a likely explanation, not a confirmed diagnosis
of this host. Testing with an explicitly authorized provisioning process that
has the necessary privacy access is still outstanding. No Full Disk Access,
SIP, Gatekeeper or TCC settings were changed. The evidence does not establish
that GUI FSKit approval is unavoidable on all server installations; it establishes
that the existing programmatic method cannot access its settings in this SSH
session. Signing the filesystem extension alone does not grant our setup process
access to Apple's group container.

## Confirmed TCC attribution

The read-only investigation captured `tcc-attribution.log` after reproducing the
`plutil` denial. At 2026-09-20 16:55:27, request `598.1637` identifies:

- accessing process: `/usr/bin/plutil`, UID 502;
- responsible process: `/opt/homebrew/Cellar/tailscale/1.102.2/bin/tailscaled`, PID 809;
- requested service: `kTCCServiceSystemPolicyAllFiles`;
- result: `authValue=0`, `authReason=5`.

Subsequent requests `598.1638` and `598.1639` target
`kTCCServiceSystemPolicyAppDataDetailed` for the FSKit group container and fail
with `access requires indirect object have an associated app bundle`, error 9.
`tailscaled` is ad-hoc/linker-signed with identifier `a.out` and no Team ID.
Apple's OpenSSH launch daemon is loaded but was not running in this snapshot.
This session uses Tailscale SSH; the responsible process is not Apple's SSH
wrapper. Apple's Remote Login Full Disk Access option therefore must not be
assumed to authorize this connection.

For comparison, the original macOS 26.6.2 host can read the same settings path
over SSH with SIP enabled. We did not determine that host's TCC grants.

The next controlled test is for the administrator to authorize Full Disk Access
for the actual `tailscaled` executable and repeat the read and activation checks,
or provide an existing Apple OpenSSH connection with the necessary privacy
authorization. A successful mount after that change is still unverified. The
investigation did not change privacy grants, restart daemons, or edit settings.

## Setup error handling follow-up

The Rust setup path now directly reads activation settings before invoking
`plutil`, preserving the OS error kind and distinguishing an absent file from
denied access. Permission failures identify the settings path and explain the
Tailscale attribution and limits of signing/manual activation. The native
evaluation's `enable.py` gives the same guidance. Both stop on read failure
before changing activation settings or restarting agents.

Validation: 17 Rust setup tests passed on both Linux and the macOS 26.6.2 host, including
new read-error regression tests and existing preservation, retry and busy-mount
gates. Command:

```sh
cargo test --release -p casita-fs --no-default-features --features darwin-fuse \
  --lib darwin::setup::tests -- --skip benchmark
```

`setup-permissions-tests.log` retains the Mac result. Nine Python harness tests
passed, including native activation denial without settings replacement or
agent restart. `ssh-recheck.json` retains the separate UID 501 native mount,
exact-byte verification of 17 fixture files, and successful ordinary unmount.
These changes improve diagnostics and do not resolve macOS 27 authorization
or complete production integration of the native backend.

## Compressed raw reports

Raw JSON and logs are stored as `.gz`. Run `gzip -dk ./*.gz` in this directory
before running the summary or reproduction scripts. Uncompressed copies are ignored.
