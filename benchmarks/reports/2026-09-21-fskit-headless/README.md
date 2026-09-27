# Headless FSKit activation and upgrade options

Research on 2026-09-20/21 using merged revision `523f38d` on `unfree-macos`,
macOS 26.6.2 (25G83), arm64, ordinary UID 501 over SSH. No root, graphical
session, SIP changes, private signing entitlements, agent termination, or
unmounting of existing volumes was used in this investigation.

## Conclusion

Rootless, headless activation is possible with our existing development setup:
register the bundle, update the user's enabled-module file, and restart that
user's FSKit agent while mounts are idle. Earlier retained native validation
demonstrated this. The current obstacle is doing activation or upgrades while
another workload has a live FSKit mount under the same user.

There is no verified, supported, non-disruptive enablement command available to
our ordinary command-line process on this host. The public documentation uses
System Settings. Private enable APIs are entitlement-gated. External plist
edits do not refresh the running agent, and it can overwrite those edits.

That does not prove that every upgrade needs a restart, or that restarting an
agent necessarily destroys every mount. Neither claim was tested. Keep the
guard around shared-agent restart until a disposable coexistence test establishes
its effects. The current implementation also checks mounts globally, although
the agent it restarts belongs to one UID; this is a conservative scope.

## Options under the no-root, no-GUI constraint

| Option | Status and limits | Appropriate use |
| --- | --- | --- |
| Reuse an unchanged, already enabled bundle | Demonstrated by earlier mounted tests, including simultaneous repositories. Requires no registration changes or restart. Does not test newly compiled extension code. | Normal operation and tests against an installed release. |
| Register, edit the user's plist, restart the user's agent while idle | Demonstrated by retained SSH validation as UID 501. Uses an undocumented settings file. Access can be denied by macOS privacy policy even when filesystem ownership permits it. | Current development/CI bootstrap, serialized with other mounts. |
| Let a dedicated worker reach idle, then activate and run tests | Uses the demonstrated procedure above. No root or GUI at test time; the worker/account must already be available. | Preferred near-term CI arrangement. |
| Higher bundle version, retain old registrations | Adding a signed version 2 selected its path; the plist entry survived addition. End-to-end upgrade from an enabled baseline remains unverified. Keeping duplicate registrations can still cause discovery ambiguity or stale identities. | Candidate for reducing restart frequency, not a proven replacement yet. |
| Separate stable development and release bundle IDs | Apple DTS recommends this to prevent registration collisions. Both identities still need initial enablement. For explicit CLI selection, distinct FSShortName values avoid ambiguity too. | Reduce development churn affecting the release installation. |
| A fresh ID for each generation, preseeded in the plist | Tested with a new ID and short name. Registration succeeded, but mounting reported the new module disabled. Does not bypass the running agent's cached enablement. | Not a working recovery path on this host. |
| Keep the extension stable and move frequently changing logic outside it | Architectural option, not implemented here. A stable extension can communicate with a per-mount backend process; initial activation remains necessary. Adds IPC, sandbox, lifecycle, authentication, and compatibility work. | If unattended upgrades without extension churn are a hard requirement. |
| Use an existing enabled FSKit bridge | Can avoid registering Casita's own extension, but requires a different transport/backend. Does not validate the newly merged native extension. Fresh machines still need the bridge enabled. | An alternative architecture, not a fix for this integration test. |
| Separate pre-provisioned macOS user or disposable VM/runner | Separate users have separate extension state and user agents. Creating an account/image may require provisioning outside these constraints. A VM also isolates system-wide fskitd and mount failures. | Stronger CI isolation; a new SSH shell under the same UID is insufficient. |
| Materialize a repository into an ordinary directory | Requires neither FSKit activation nor a GUI. Loses on-demand filesystem behavior and cannot validate native mount callbacks. | Workflow fallback when mounting is optional. |

Our native adapter already avoids extension upgrades for ordinary root
publication: publish more roots through the existing mount instead of rebuilding
or registering a bundle for each repository operation.

## Mechanisms examined that do not currently solve enablement

| Mechanism | Evidence |
| --- | --- |
| `pluginkit -a` and `lsregister -f -R -trusted` | Register a bundle, but registration is distinct from FSKit enablement. |
| `pluginkit -e use` | Tested; did not enable the disabled FSKit module. The installed manual says election applies to all copies of an identifier, not one selected path. |
| Add replacement before removing predecessor | Tested: a higher version became selected, but the subsequent `pluginkit -r` / `lsregister -u` cleanup still removed the enabled entry. The experiment did not isolate which removal command caused the write. |
| Edit `enabledModules.plist`, atomically replace it, wait | Tested. The file contained the ID while mounting explicitly reported it disabled. Registration activity later overwrote an externally restored entry, consistent with saving a cached list. |
| Seed enablement before first registration | Tested for `org.casita.research.headless.extension`; still disabled. |
| `FSClient.setEnabledStateForIdentifier:newState:replyHandler:` | Undocumented in the installed public headers. Ordinary helper returned `NSPOSIXErrorDomain` code 1 (`EPERM`). |
| Direct agent XPC `setEnabledState:newState:replyHandler:` | Ordinary helper's connection was rejected with `NSCocoaErrorDomain` code 4097. No agent restart or private entitlement was attempted. |
| `launchctl kickstart -p user/501/com.apple.fskit.fskit_agent` | Succeeded and returned the existing PID 80319. Without `-k`, it does not restart or reload the running process. |
| `launchctl kickstart -k`, bootout/bootstrap, or signalling the agent | Process-lifecycle operations, not module-local enablement. Restart variants were not exercised with the existing live mount. Earlier retained testing found SIGTERM insufficient and used SIGKILL. They do not remove the coexistence question. |
| SIGHUP, `notifyutil`, defaults/CFPreferences, or restarting cfprefsd | No documented FSKit reload mechanism found. Do not invent a notification or assume an arbitrary signal is harmless. Not tested against the live agent. |
| Killing only an extension or extensionkitservice | May address a launch failure, but does not establish that the agent reloads enablement. Do not kill processes serving active mounts. Not tested as an activation workaround. |
| Fresh mount/resource URL | Useful for distinguishing a poisoned resource from activation failure. Does not grant enablement. Current failures included explicit disabled-module errors. |
| Re-register an unchanged bundle or rebuild at the same path | Can provoke discovery; not a reliable enablement refresh. Replacing a bundle used by a live extension is not an acceptable upgrade strategy. |
| Another SSH session, changed HOME, user LaunchAgent | Same UID still uses the same FSKit agent. The installed launchd service supports Background and Aqua sessions, so a GUI session itself is not required for the agent to run. |
| Developer ID signing/notarization or the public FSKit entitlement | Important for distribution; not evidence of authority to change enablement. The tested private APIs have separate gates. A properly provisioned distribution build was not tested in this investigation. |
| `systemextensionsctl`, System Extension MDM payloads, SMAppService | Different extension/service mechanisms. FSKit is delivered as an ExtensionKit app extension. No documented FSKit approval payload or ordinary-user CLI enablement path was found. Existing MDM/privileged-helper provisioning would also be an external dependency. |
| UI scripting, opening System Settings, Accessibility automation | Requires a graphical session/consent and fails the stated constraint. |
| Restart system fskitd, reboot, privileged helper, SIP changes | Outside the ordinary-user constraint or not an acceptable substitute for module activation. |

### Why the private API fails

Read-only inspection of the installed arm64e binaries connects the errors to
specific gates, rather than attributing them to an absent GUI:

- `fskitdXPCServer`'s `setEnabledStateForIdentifier:newState:replyHandler:` checks
  `clientHasEntitlement` and immediately returns POSIX error 1 when it is false.
  The connection listener derives that flag from
  `com.apple.private.LiveFS.connection`.
- `fskit_agent`'s listener checks
  `com.apple.private.LiveFS.setmachport` before accepting a connection.
- These are distinct from the public
  `com.apple.developer.fskit.fsmodule` entitlement, which identifies a filesystem
  extension. Adding private entitlements to a development binary is not a
  supported distribution strategy and was not attempted.
- The public SDK exposes extension discovery through `FSClient` and a read-only
  `FSModuleIdentity.enabled` property. The ordinary ad-hoc discovery helper listed
  only built-in modules; that output cannot prove third-party modules are absent.

These observations apply to this installed build, not a compatibility promise
for future macOS releases.

## Recommended path

1. Use a dedicated idle worker for the real native lifecycle test, using the
   rootless headless setup we have already demonstrated. Serialize registration
   changes and mount lifetimes, not just the setup command itself.
2. Separate release and development identifiers and use immutable, versioned
   bundle paths. Keep the exact serving-executable assertion in lifecycle tests.
3. Test retaining old registrations from a known enabled baseline. Check fresh
   mounts, remounts, an unrelated live filesystem, selected executable path, and
   rollback. The disabled baseline in the current session cannot establish this.
4. Separately measure the correctness effects of restarting the user agent with
   disposable active mounts. Until then, describe the all-mounts guard as a
   conservative protection in our setup, not an Apple one-mount limitation.
5. If the product must upgrade continuously while arbitrary same-user mounts
   remain live, evaluate a stable extension with a separately versioned backend.
   That is a larger architecture decision, not a reason to remove the guard now.

For this investigation, "without root and GUI" does not imply "without downtime."
The idle activation option satisfies the former and still needs the latter.

## Evidence and reproducibility

Earlier repository evidence:

- [Rootless SSH bootstrap, concurrent mounts, and activation](../2026-09-10-fskit-bootstrap/README.md).
- [Native extension activation and restart receipts](../2026-09-15-native-fskit/README.md).
- [Native repository lifecycle and simultaneous mounts](../2026-09-15-native-fskit-repository/README.md).

Current investigation artifacts are at
`/tmp/casita-macos-validation-523f38d/` locally and
`/Users/hetzner/casita-validation-523f38d/` on the Mac. They include the original
activation investigation, ordinary helper sources/results, registration and
enabled-list snapshots, mount errors, and before/after agent and mount tables.
They are diagnostic artifacts, not performance benchmarks.

The merged native library tests passed all 13 cases and both production bundles
passed signature verification before these experiments. At the end of this
investigation, the real lifecycle test had not passed for the merged build on
this occupied host.

State at the end of the investigation: original production registration and enabled list restored; the
temporary research registration removed. Agent PID 80319 and the full mount table
matched their prior values. Casita still needs activation recovery in the running
agent, recorded by its existing `activation-pending` marker. Obrador remained
mounted throughout.

Subsequent multi-mount validation completed after the user approved unmounting
Obrador. Ordinary unmount failed with `Connection reset by peer`; an explicitly
approved force-unmount succeeded. Setup then activated `CasitaFSKitConcurrent.app`
and cleared the pending marker. Both the lifecycle and independent-process mount
tests passed. All test volumes were unmounted afterward; the new bundle remains
registered. See [validation results](../../../casita-fs/native-fskit/README.md#build-and-test).

Read-only commands for another investigation:

```sh
id
sw_vers
mount
pgrep -u "$(id -u)" -x fskit_agent
pluginkit -m -A -D -vv -i org.casita.fskit.extension
plutil -convert json -o - \
  "$HOME/Library/Group Containers/group.com.apple.fskit.settings/enabledModules.plist"
launchctl print "user/$(id -u)/com.apple.fskit.fskit_agent"
```

## Primary sources

- [Apple's passthrough sample](https://developer.apple.com/documentation/fskit/building-a-passthrough-file-system): enables through System Settings, then mounts from Terminal.
- [FSClient](https://developer.apple.com/documentation/fskit/fsclient): public discovery interface; also checked against SDK 26.4 headers on the host.
- [FSKit module entitlement](https://developer.apple.com/documentation/bundleresources/entitlements/com.apple.developer.fskit.fsmodule): identifies a filesystem extension.
- [Apple DTS on stale identities and development/release IDs](https://developer.apple.com/forums/thread/804432): explains registration churn and recommends separate IDs.
- [Apple engineer on FSKit lifecycle](https://developer.apple.com/forums/thread/831396): confirms per-user approval and notes synchronization improvements in macOS 26.5. This host runs 26.6.2, so those improvements do not establish that our failure is fixed.
- [FSKit CI automation question](https://developer.apple.com/forums/thread/838060): unattended approval question remained unanswered when checked; it is evidence of an open question, not Apple guidance.
- Installed macOS `pluginkit(8)` and `launchctl(1)` manuals, the user-agent launchd plist, installed SDK headers, and the bounded runtime probes described above.
