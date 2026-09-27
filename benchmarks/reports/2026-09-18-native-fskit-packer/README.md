# Native FSKit packer stack profile, 2026-09-18

## Finding

The native extension's stack sample places much of the packer's sampled work in
Objective-C object handling: `FSFileName.data`, retain/autorelease operations,
message dispatch and access to `NSData` storage. The profile supports the
[phase timers' finding](../2026-09-18-native-fskit-phases/README.md) that packing
is expensive, and identifies work inside that call. It does not demonstrate a
new launch-speed improvement.

In one sampled call subtree, the Rust packer binding has 303 inclusive samples.
Its largest FSKit call-site branch has 123 samples, descending into
`FSFileName.data`, `objc_retain` and `objc_autoreleaseReturnValue`. Other visible
branches descend into `objc_retainAutorelease`, `objc_msgSend` and
`object_getIndexedIvars`. These are nested, inclusive counts. They must not be
added together as if independent, converted directly to total launch time, or
divided by samples of idle threads to estimate a CPU percentage.

Abbreviated path from [the full stack file](packer-profile.stacks.txt.gz):

```text
Volume::enumerateDirectory...
  FSDirectoryEntryPacker::packEntryWithName...
    -[FSDirectoryEntryPacker packEntryWithName:...]
      -[FSFileName data]
        objc_retain
      objc_autoreleaseReturnValue
        AutoreleasePoolPage::add
      objc_retainAutorelease
      object_getIndexedIvars
```

The same profile also captures our filename-construction path allocating an
`NSData` with `dataWithBytes:length:` before creating the `FSFileName`. That is
a concrete candidate for a smaller experiment: use the SDK's
`initWithBytes:length:` filename initializer, which copies bytes directly, and
compare it with the current `nameWithData:` construction in the same binary.
The checked `objc2-fs-kit 0.3.2` bindings expose this initializer. Both byte/data
initializers stop at a NUL byte; filesystem names cannot contain NUL. Any change
must preserve arbitrary non-UTF-8 filename bytes and retain correctness gates.

Avoid assuming that changing construction eliminates the packer's own data
access and ownership operations. That requires measurement. No constructor
change, filename cache, private-framework bypass or system-policy change was
made in this investigation.

## Run and validation

- [The benchmark report](packer-profile.json.gz) completed all ten rounds at 512
  metadata siblings, with 14 cases and native/fuser/host controls: 4,200 timed
  operations. Correctness, publication, sandbox/mutation denial, independent
  mounts and teardown gates passed.
- The 20-second sample ran as the ordinary user against PID 96913, matching
  `native_process` and `extension_sample` in the report. It identifies our
  `org.casita.native-fskit.extension.repository` extension, not the driver or
  fuser process. Sampling returned zero and produced a nonempty stack file.
- This is a shared Apple M1 Mac running macOS 26.6.2. Another test job was active.
  The profiler and shared-host activity perturb timings, so this report is for
  attribution rather than new latency comparisons.
- 41 Python tests pass, including the new entry in `benchmark all`; whitespace
  checks pass. No Rust code changed in this turn. The native release build and
  mounted workload completed successfully.

`packer-preparation.stacks.txt` is a separate three-second sample of our driver
while waiting for pre-measurement gates. It showed progress in file open/read
operations. It is retained for the preparation diagnosis and is not used for
the packer attribution.

## Permanent reproduction

`native-fskit-launch-profile` is registered in `benchmarks/manifest.json` and
`benchmark all`. It samples only the native PID validated by this run, after
the lifecycle gates and warm-up. The sampler finishes before teardown; failure
or interruption still runs cleanup and does not count as a successful profile.

From the repository root on the Mac, with the previously documented toolchain:

```sh
python3 -m benchmarks.cli run native-fskit-launch-profile \
  --profile standard --output results/packer-profile.json
```

This retains the JSON, `.stacks.txt` and `.sample.log` sidecars. The entry defaults
to 512 metadata siblings, ten rounds and a 20-second sample. Existing density
benchmarks cover both sides of the pagination steps; profiling introduces no
new directory-size threshold. `--sample-extension-seconds 0` disables the
sampler for an unprofiled control.

Run `python3 benchmarks/reports/2026-09-18-native-fskit-packer/summarize.py`
to validate result/sample identity and print a bounded packer subtree.

## Next

Prefer a same-binary direct-byte versus temporary-NSData construction experiment,
using the existing phase and directory-density cases. It avoids the retained
memory and lifetime complexity of another cache. A filename cache remains an
alternative if constructor changes do not help enough. Keep fuser in production
until unprofiled comparisons establish sufficient native performance.

## Compressed raw reports

Raw JSON and logs are stored as `.gz`. Before running the reproduction or
summary scripts, run `gzip -dk ./*.gz` in this report directory. This restores
the original filenames and exact report bytes; uncompressed copies are ignored.
