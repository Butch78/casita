# Each output retains references to its two predecessors, forming overlapping
# transitive closures. Request every node so verification checks every byte.
{ count ? 64, nonce ? "manual", system ? builtins.currentSystem }:
assert builtins.isInt count && count > 0;
let
  graph = builtins.genList (index:
    let
      dependencies = builtins.genList
        (offset: builtins.elemAt graph (index - offset - 1))
        (if index < 2 then index else 2);
    in derivation {
      name = "shared-${nonce}-${toString index}";
      inherit system;
      builder = "/bin/sh";
      previous = dependencies;
      value = "value-${toString index}";
      args = [ "-c" ''
        set -eu
        printf '%s\n' "$value" > "$out"
        for dependency in $previous; do
          test -s "$dependency"
          printf '%s\n' "$dependency" >> "$out"
        done
      '' ];
      preferLocalBuild = true;
      allowSubstitutes = false;
    }) count;
in graph
