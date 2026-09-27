# EROFS experiment: excluded by the deployment requirement

The user requires mounting directly on the Linux host without sudo. The host's
kernel EROFS mount was denied with EPERM inside an unprivileged user namespace.
Running the workload as guest root in a disposable, unprivileged KVM VM permits
testing, but does not satisfy that deployment requirement. Kernel EROFS is
therefore excluded from the current transport selection.

One smoke run completed before the experiment was stopped. `smoke.json` retains
that VM result and its provenance; it is not a native-host result or evidence
for adopting EROFS. No standard comparison was run. The experiment remains in
the permanent corpus as `erofs-transports`, including `benchmark all`, as
required by AGENTS.md for benchmarks created during performance investigations.

Reproduce the experimental VM smoke case in the Nix development environment:

```sh
python3 -m benchmarks.cli run erofs-transports --profile smoke --repetitions 1 \
  --output benchmarks/results/erofs-transports-smoke.json
```

The runner requires accessible KVM, a matching Linux kernel/module tree, Nix
runtime closures, QEMU, static BusyBox, erofs-utils, e2fsprogs and cpio. It performs
privileged filesystem operations only inside its disposable guest. Guest
performance and guest permissions must not be presented as host deployment
capabilities.
