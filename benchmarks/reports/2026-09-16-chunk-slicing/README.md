# Content-defined chunk slicing against exact chunk deduplication

Casita's chunked backend deduplicates only whole FastCDC chunks. A rebuilt Nix
store path differs from its previous build in the store hashes it embeds, so
a 256 KiB chunk that contains one changed hash is stored again in full. This
investigation measures an alternative encoder that stores a rebuilt blob as
slices of blobs the store already holds: sampled small discovery chunks find
a candidate, a byte comparison confirms it, and the match extends in both
directions until the first mismatch. Both encoders live in the permanent
`cdcs` Criterion target; `benchmark run cdcs-corpus` runs the same matrix on
real trees, single packages or a whole rebuilt closure. See the benchmarks
README for the method and the gates.

Every row below reconstructed each rebuilt blob through its compressed frames
and matched the whole-blob BLAKE3 identity before it was reported. Physical
bytes are new payload plus 40-byte manifest entries (exact) or copy and
literal tokens plus a 32-byte entry per referenced source blob (slices); the
base tree is already stored. The slicing rows model an encoder that does not
exist in Casita. Exact rows model the current backend without pack, catalog,
or transfer costs.

Environment: AMD Ryzen 7 7840S with Radeon 780M Graphics, 16 logical CPUs, governor
`performance`, Linux-7.2.2-x86_64-with-glibc2.42, Casita `fe2007f3d2a4`
plus this uncommitted benchmark. The bench binary was built by the host
toolchain (rustc 1.97.1); the harness ran under the devenv shell. Encode
seconds are single-process wall-clock of an in-memory encoder, one repetition.

## Real rebuilt store paths

Six packages with several same-version rebuilds in the local Nix store. For
each name the corpus suite picked the first two store paths with identical
file layouts but different contents. The pairs differ only where store hashes
of their dependencies are embedded, which is the case the slicing proposal is
about. Totals across the six pairs:

| Strategy | Physical, six pairs | Percent of 51.3 MiB | Index entries |
|---|---:|---:|---:|
| exact/256KiB | 1.4 MiB | 2.647 | 302 |
| exact/64KiB | 740.0 KiB | 1.409 | 745 |
| exact/8KiB | 449.1 KiB | 0.855 | 4,938 |
| exact/1KiB | 1.6 MiB | 3.142 | 38,533 |
| slices/8KiB/16 | 1.0 MiB | 2.029 | 319 |
| slices/2KiB/16 | 525.3 KiB | 1.001 | 1,190 |
| slices/1KiB/16 | 270.5 KiB | 0.515 | 2,437 |
| slices/1KiB/4 | 111.0 KiB | 0.211 | 9,727 |
| slices/1KiB/1 | 60.7 KiB | 0.116 | 38,533 |

`slices/1KiB/4` stores between 2.1 times (libxcb) and 78 times (icu4c) fewer
bytes than `exact/8KiB`, and between 3.8 times (libxcb) and about 300 times (gmp) fewer
than the production `exact/256KiB`, on every pair. Its index holds about twice the
entries of an 8 KiB exact index and a quarter of a 1 KiB exact index.

### icu4c-76.1

8 regular files, 38.2 MiB. Base `/nix/store/0dmcpvbp2rvz8a7p2yhsrkpcb41kr6wl-icu4c-76.1`, rebuilt `/nix/store/657nyvrdribi7if64nhykz0vji7ljn1p-icu4c-76.1`; 26 candidates in 7 layouts.

| Strategy | Physical | Percent | Literal bytes | Copies | Literals | Depth | Index entries | Sources per group (mean, max) | Encode seconds |
|---|---:|---:|---:|---:|---:|---:|---:|---:|---:|
| exact/256KiB | 424.6 KiB | 1.085 | 1.2 MiB | 128 | 0 | 0 | 133 | 1.05, 2 | 0.023 |
| exact/64KiB | 118.8 KiB | 0.304 | 376.3 KiB | 480 | 0 | 0 | 485 | 1.19, 2 | 0.021 |
| exact/8KiB | 164.7 KiB | 0.421 | 98.8 KiB | 3751 | 0 | 0 | 3,738 | 2.53, 5 | 0.032 |
| exact/1KiB | 1.1 MiB | 3.007 | 8.1 KiB | 30044 | 0 | 0 | 29,350 | 13.26, 21 | 0.094 |
| slices/8KiB/16 | 74.8 KiB | 0.191 | 202.5 KiB | 7 | 7 | 1 | 256 | 1.00, 2 | 0.063 |
| slices/2KiB/16 | 18.7 KiB | 0.048 | 57.5 KiB | 9 | 7 | 1 | 925 | 1.00, 2 | 0.094 |
| slices/1KiB/16 | 30.5 KiB | 0.078 | 77.9 KiB | 10 | 6 | 1 | 1,856 | 1.00, 2 | 0.107 |
| slices/1KiB/4 | 2.1 KiB | 0.005 | 3.2 KiB | 12 | 6 | 1 | 7,435 | 1.00, 2 | 0.103 |
| slices/1KiB/1 | 1.2 KiB | 0.003 | 974.0 B | 14 | 5 | 2 | 29,350 | 1.00, 3 | 0.119 |

### gmp-6.3.0

2 regular files, 729.5 KiB. Base `/nix/store/0giizlwk3grpwgw30g2g0gvy2hgm37cf-gmp-6.3.0`, rebuilt `/nix/store/6mxnr68qgihr5pmp4avjbijjgwifdnml-gmp-6.3.0`; 40 candidates in 8 layouts.

| Strategy | Physical | Percent | Literal bytes | Copies | Literals | Depth | Index entries | Sources per group (mean, max) | Encode seconds |
|---|---:|---:|---:|---:|---:|---:|---:|---:|---:|
| exact/256KiB | 184.1 KiB | 25.234 | 429.1 KiB | 3 | 0 | 0 | 5 | 1.02, 2 | 0.002 |
| exact/64KiB | 32.5 KiB | 4.450 | 86.3 KiB | 9 | 0 | 0 | 11 | 1.15, 2 | 0.001 |
| exact/8KiB | 6.2 KiB | 0.857 | 16.0 KiB | 71 | 0 | 0 | 73 | 2.47, 4 | 0.001 |
| exact/1KiB | 22.6 KiB | 3.092 | 2.3 KiB | 567 | 0 | 0 | 568 | 13.02, 16 | 0.002 |
| slices/8KiB/16 | 21.0 KiB | 2.875 | 53.1 KiB | 1 | 2 | 1 | 3 | 1.02, 2 | 0.001 |
| slices/2KiB/16 | 21.0 KiB | 2.875 | 53.1 KiB | 1 | 2 | 1 | 16 | 1.02, 2 | 0.002 |
| slices/1KiB/16 | 635.0 B | 0.085 | 980.0 B | 2 | 2 | 1 | 28 | 1.02, 2 | 0.002 |
| slices/1KiB/4 | 635.0 B | 0.085 | 980.0 B | 2 | 2 | 1 | 132 | 1.02, 2 | 0.002 |
| slices/1KiB/1 | 188.0 B | 0.025 | 80.0 B | 3 | 2 | 1 | 568 | 1.04, 2 | 0.002 |

### zstd-1.5.7

3 regular files, 1.2 MiB. Base `/nix/store/00qn3vc7r4m32c072kjnrbxd86w9slzj-zstd-1.5.7`, rebuilt `/nix/store/4sq0awhi6w9zw9qb08amnams96a0kwp5-zstd-1.5.7`; 29 candidates in 7 layouts.

| Strategy | Physical | Percent | Literal bytes | Copies | Literals | Depth | Index entries | Sources per group (mean, max) | Encode seconds |
|---|---:|---:|---:|---:|---:|---:|---:|---:|---:|
| exact/256KiB | 187.2 KiB | 15.810 | 428.0 KiB | 5 | 0 | 0 | 7 | 1.03, 2 | 0.002 |
| exact/64KiB | 76.1 KiB | 6.427 | 174.8 KiB | 15 | 0 | 0 | 17 | 1.16, 2 | 0.001 |
| exact/8KiB | 14.0 KiB | 1.184 | 37.8 KiB | 107 | 0 | 0 | 110 | 2.36, 4 | 0.001 |
| exact/1KiB | 37.4 KiB | 3.155 | 4.5 KiB | 913 | 0 | 0 | 912 | 12.97, 17 | 0.003 |
| slices/8KiB/16 | 104.1 KiB | 8.794 | 313.9 KiB | 1 | 3 | 1 | 8 | 1.01, 2 | 0.004 |
| slices/2KiB/16 | 2.5 KiB | 0.208 | 7.5 KiB | 4 | 2 | 1 | 36 | 1.03, 2 | 0.003 |
| slices/1KiB/16 | 2.5 KiB | 0.208 | 7.5 KiB | 4 | 2 | 1 | 63 | 1.03, 2 | 0.003 |
| slices/1KiB/4 | 2.5 KiB | 0.208 | 7.5 KiB | 4 | 2 | 1 | 226 | 1.03, 2 | 0.004 |
| slices/1KiB/1 | 583.0 B | 0.048 | 1.0 KiB | 5 | 3 | 1 | 912 | 1.03, 2 | 0.003 |

### libxcb-1.17.0

50 regular files, 1.4 MiB. Base `/nix/store/2chpcgwndk5iphqgwf9r7x4yjysmkd2z-libxcb-1.17.0`, rebuilt `/nix/store/b9c155jq2sjw757ywkazvlnl9kyjmcc7-libxcb-1.17.0`; 30 candidates in 8 layouts.

| Strategy | Physical | Percent | Literal bytes | Copies | Literals | Depth | Index entries | Sources per group (mean, max) | Encode seconds |
|---|---:|---:|---:|---:|---:|---:|---:|---:|---:|
| exact/256KiB | 357.9 KiB | 25.693 | 1.4 MiB | 50 | 0 | 0 | 100 | 1.00, 1 | 0.005 |
| exact/64KiB | 351.9 KiB | 25.259 | 1.3 MiB | 59 | 0 | 0 | 114 | 1.07, 2 | 0.004 |
| exact/8KiB | 197.5 KiB | 14.177 | 722.5 KiB | 170 | 0 | 0 | 259 | 1.93, 4 | 0.003 |
| exact/1KiB | 150.2 KiB | 10.782 | 339.8 KiB | 1092 | 0 | 0 | 1,350 | 9.40, 16 | 0.005 |
| slices/8KiB/16 | 316.1 KiB | 22.689 | 1.2 MiB | 3 | 51 | 1 | 15 | 1.02, 2 | 0.005 |
| slices/2KiB/16 | 210.8 KiB | 15.129 | 775.6 KiB | 19 | 61 | 1 | 51 | 1.19, 2 | 0.006 |
| slices/1KiB/16 | 139.7 KiB | 10.028 | 501.6 KiB | 34 | 64 | 1 | 93 | 1.27, 2 | 0.005 |
| slices/1KiB/4 | 94.1 KiB | 6.752 | 318.1 KiB | 60 | 71 | 1 | 327 | 1.41, 2 | 0.005 |
| slices/1KiB/1 | 55.2 KiB | 3.962 | 186.8 KiB | 110 | 82 | 2 | 1,350 | 1.69, 3 | 0.004 |

### file-5.45

4 regular files, 8.3 MiB. Base `/nix/store/018l43lbsqbrsasrfdcxxb5s4s161fkr-file-5.45`, rebuilt `/nix/store/a3dmb85n5k4n0v0yp775y9hbma7dgggg-file-5.45`; 41 candidates in 6 layouts.

| Strategy | Physical | Percent | Literal bytes | Copies | Literals | Depth | Index entries | Sources per group (mean, max) | Encode seconds |
|---|---:|---:|---:|---:|---:|---:|---:|---:|---:|
| exact/256KiB | 103.5 KiB | 1.212 | 225.2 KiB | 20 | 0 | 0 | 23 | 1.01, 2 | 0.004 |
| exact/64KiB | 63.4 KiB | 0.742 | 131.4 KiB | 77 | 0 | 0 | 80 | 1.13, 2 | 0.004 |
| exact/8KiB | 34.0 KiB | 0.398 | 26.9 KiB | 601 | 0 | 0 | 604 | 2.10, 4 | 0.005 |
| exact/1KiB | 213.8 KiB | 2.504 | 3.2 KiB | 5427 | 0 | 0 | 5,430 | 11.11, 16 | 0.016 |
| slices/8KiB/16 | 102.8 KiB | 1.204 | 225.2 KiB | 1 | 3 | 1 | 31 | 1.00, 1 | 0.012 |
| slices/2KiB/16 | 28.1 KiB | 0.329 | 78.4 KiB | 2 | 3 | 1 | 139 | 1.00, 2 | 0.026 |
| slices/1KiB/16 | 1.7 KiB | 0.020 | 4.1 KiB | 4 | 3 | 1 | 334 | 1.00, 2 | 0.020 |
| slices/1KiB/4 | 1.7 KiB | 0.020 | 4.1 KiB | 4 | 3 | 1 | 1,394 | 1.00, 2 | 0.020 |
| slices/1KiB/1 | 317.0 B | 0.004 | 112.0 B | 6 | 3 | 1 | 5,430 | 1.01, 2 | 0.026 |

### gnumake-4.4.1

32 regular files, 1.5 MiB. Base `/nix/store/05sqpqfnha0pmb5aia3gz968im7n806v-gnumake-4.4.1`, rebuilt `/nix/store/15xrks0frcgils8qxfkhspyg6gi9rxdh-gnumake-4.4.1`; 65 candidates in 11 layouts.

| Strategy | Physical | Percent | Literal bytes | Copies | Literals | Depth | Index entries | Sources per group (mean, max) | Encode seconds |
|---|---:|---:|---:|---:|---:|---:|---:|---:|---:|
| exact/256KiB | 132.6 KiB | 8.692 | 257.8 KiB | 33 | 0 | 0 | 34 | 1.01, 2 | 0.002 |
| exact/64KiB | 97.3 KiB | 6.379 | 204.8 KiB | 36 | 0 | 0 | 38 | 1.04, 2 | 0.001 |
| exact/8KiB | 32.7 KiB | 2.145 | 61.9 KiB | 173 | 0 | 0 | 154 | 2.25, 4 | 0.001 |
| exact/1KiB | 49.1 KiB | 3.222 | 7.9 KiB | 1167 | 0 | 0 | 923 | 11.04, 16 | 0.003 |
| slices/8KiB/16 | 446.3 KiB | 29.253 | 1.1 MiB | 5 | 30 | 1 | 6 | 1.03, 2 | 0.006 |
| slices/2KiB/16 | 244.4 KiB | 16.017 | 603.6 KiB | 27 | 33 | 5 | 23 | 1.24, 2 | 0.008 |
| slices/1KiB/16 | 95.6 KiB | 6.266 | 234.7 KiB | 33 | 16 | 2 | 63 | 1.12, 2 | 0.007 |
| slices/1KiB/4 | 10.0 KiB | 0.657 | 18.3 KiB | 37 | 6 | 2 | 213 | 1.07, 2 | 0.005 |
| slices/1KiB/1 | 3.2 KiB | 0.211 | 3.3 KiB | 48 | 4 | 13 | 923 | 1.15, 2 | 0.005 |

## Synthetic fixture

Four 1 MiB files of alternating aperiodic text and random 64 KiB blocks with
a fake store path every 512 B, 4 KiB, 64 KiB and 1 MiB. The rebuilt tree maps
each of eight dependency hashes to a new hash. Files never share content, so
every saving comes from the base tree. This fixture is small, so the sampled
rows have high variance: one lost 64 KiB gap is 1.5 percent of the corpus.

| Edit spacing | Strategy | Physical | Percent | Literal bytes | Copies | Literals | Index entries | Sources per group (mean, max) | Encode seconds |
|---|---|---:|---:|---:|---:|---:|---:|---:|---:|
| 512B | exact/256KiB | 2.8 MiB | 71.16 | 4.0 MiB | 16 | 0 | 31 | 1.04, 2 | 0.018 |
| 512B | exact/64KiB | 2.9 MiB | 72.87 | 4.0 MiB | 52 | 0 | 103 | 1.19, 2 | 0.020 |
| 512B | exact/8KiB | 3.0 MiB | 74.96 | 4.0 MiB | 396 | 0 | 800 | 2.53, 4 | 0.019 |
| 512B | exact/1KiB | 3.4 MiB | 85.27 | 4.0 MiB | 3230 | 0 | 6,441 | 13.60, 17 | 0.033 |
| 512B | slices/8KiB/16 | 2.7 MiB | 67.19 | 4.0 MiB | 0 | 4 | 46 | 1.00, 1 | 0.019 |
| 512B | slices/2KiB/16 | 2.7 MiB | 67.19 | 4.0 MiB | 0 | 4 | 209 | 1.00, 1 | 0.025 |
| 512B | slices/1KiB/16 | 2.7 MiB | 67.19 | 4.0 MiB | 0 | 4 | 391 | 1.00, 1 | 0.024 |
| 512B | slices/1KiB/4 | 2.7 MiB | 67.19 | 4.0 MiB | 0 | 4 | 1,564 | 1.00, 1 | 0.028 |
| 512B | slices/1KiB/1 | 2.7 MiB | 67.19 | 4.0 MiB | 0 | 4 | 6,441 | 1.00, 1 | 0.027 |
| 4KiB | exact/256KiB | 3.1 MiB | 76.91 | 4.0 MiB | 14 | 0 | 28 | 1.04, 2 | 0.016 |
| 4KiB | exact/64KiB | 3.3 MiB | 81.77 | 4.0 MiB | 49 | 0 | 97 | 1.17, 2 | 0.011 |
| 4KiB | exact/8KiB | 3.2 MiB | 79.40 | 4.0 MiB | 399 | 0 | 799 | 2.54, 4 | 0.014 |
| 4KiB | exact/1KiB | 1.5 MiB | 36.58 | 1.7 MiB | 3211 | 0 | 4,436 | 13.53, 17 | 0.017 |
| 4KiB | slices/8KiB/16 | 2.9 MiB | 73.11 | 4.0 MiB | 0 | 4 | 61 | 1.00, 1 | 0.017 |
| 4KiB | slices/2KiB/16 | 2.9 MiB | 72.50 | 3.9 MiB | 32 | 35 | 158 | 1.16, 2 | 0.019 |
| 4KiB | slices/1KiB/16 | 2.7 MiB | 66.43 | 3.5 MiB | 117 | 121 | 249 | 1.46, 2 | 0.022 |
| 4KiB | slices/1KiB/4 | 1.8 MiB | 45.31 | 2.4 MiB | 421 | 423 | 1,031 | 1.91, 2 | 0.017 |
| 4KiB | slices/1KiB/1 | 200.8 KiB | 4.90 | 254.2 KiB | 972 | 968 | 4,436 | 2.00, 2 | 0.012 |
| 64KiB | exact/256KiB | 3.2 MiB | 80.32 | 4.0 MiB | 14 | 0 | 28 | 1.04, 2 | 0.012 |
| 64KiB | exact/64KiB | 3.2 MiB | 81.19 | 3.9 MiB | 50 | 0 | 95 | 1.18, 2 | 0.010 |
| 64KiB | exact/8KiB | 627.4 KiB | 15.32 | 776.1 KiB | 401 | 0 | 465 | 2.55, 4 | 0.006 |
| 64KiB | exact/1KiB | 241.4 KiB | 5.89 | 136.4 KiB | 3207 | 0 | 3,306 | 13.51, 17 | 0.012 |
| 64KiB | slices/8KiB/16 | 2.5 MiB | 61.98 | 3.3 MiB | 11 | 15 | 19 | 1.08, 2 | 0.014 |
| 64KiB | slices/2KiB/16 | 805.1 KiB | 19.66 | 993.5 KiB | 51 | 50 | 107 | 1.24, 2 | 0.011 |
| 64KiB | slices/1KiB/16 | 250.8 KiB | 6.12 | 289.8 KiB | 63 | 60 | 202 | 1.25, 2 | 0.015 |
| 64KiB | slices/1KiB/4 | 1.7 KiB | 0.04 | 2.0 KiB | 68 | 64 | 807 | 1.25, 2 | 0.012 |
| 64KiB | slices/1KiB/1 | 1.7 KiB | 0.04 | 2.0 KiB | 68 | 64 | 3,306 | 1.25, 2 | 0.012 |
| 1MiB | exact/256KiB | 1.1 MiB | 28.30 | 1.4 MiB | 14 | 0 | 18 | 1.04, 2 | 0.009 |
| 1MiB | exact/64KiB | 326.9 KiB | 7.98 | 356.2 KiB | 51 | 0 | 55 | 1.18, 2 | 0.002 |
| 1MiB | exact/8KiB | 53.7 KiB | 1.31 | 49.5 KiB | 401 | 0 | 405 | 2.55, 4 | 0.003 |
| 1MiB | exact/1KiB | 130.6 KiB | 3.19 | 5.7 KiB | 3206 | 0 | 3,210 | 13.51, 17 | 0.011 |
| 1MiB | slices/8KiB/16 | 379.0 KiB | 9.25 | 512.1 KiB | 7 | 4 | 14 | 1.02, 2 | 0.009 |
| 1MiB | slices/2KiB/16 | 364.0 B | 0.01 | 128.0 B | 8 | 4 | 105 | 1.02, 2 | 0.009 |
| 1MiB | slices/1KiB/16 | 364.0 B | 0.01 | 128.0 B | 8 | 4 | 190 | 1.02, 2 | 0.011 |
| 1MiB | slices/1KiB/4 | 364.0 B | 0.01 | 128.0 B | 8 | 4 | 771 | 1.02, 2 | 0.013 |
| 1MiB | slices/1KiB/1 | 364.0 B | 0.01 | 128.0 B | 8 | 4 | 3,210 | 1.02, 2 | 0.012 |

## A whole rebuilt closure

NixOS system generation 149 to 150 on this machine is a full rebuild: of the
2244 store paths in the new closure, 419 are shared with the old one, 1738
are same-name rebuilds and 25 are upgrades (chromium, firefox, gcc, rclone,
linux-firmware); 27 paths have no old counterpart and 35 are symlink-only.
`benchmark run cdcs-corpus --closure-base ... --closure-rebuilt ...` encoded
every rebuilt path against its old counterpart: 1763 pairs, 14.6 GiB, about
50 minutes for the nine strategies. Each pair sees only its own counterpart,
not the whole old closure, so cross-package matches are not counted.

| Strategy | Physical | Percent | Index entries | Max depth | Max sources/group | Encode seconds |
|---|---:|---:|---:|---:|---:|---:|
| exact/256KiB | 1.5 GiB | 10.562 | 180,827 | 0 | 2 | 22.2 |
| exact/64KiB | 1.4 GiB | 9.483 | 332,826 | 0 | 2 | 18.6 |
| exact/8KiB | 1.3 GiB | 9.119 | 1,764,569 | 0 | 5 | 24.5 |
| exact/1KiB | 2.0 GiB | 13.347 | 13,093,775 | 0 | 30 | 61.0 |
| slices/8KiB/16 | 1.5 GiB | 10.298 | 110,802 | 54 | 3 | 40.4 |
| slices/2KiB/16 | 1.3 GiB | 8.550 | 413,059 | 54 | 3 | 48.9 |
| slices/1KiB/16 | 1.2 GiB | 7.918 | 819,554 | 55 | 4 | 58.1 |
| slices/1KiB/4 | 1.0 GiB | 7.010 | 3,276,446 | 55 | 4 | 58.6 |
| slices/1KiB/1 | 979.4 MiB | 6.535 | 13,093,775 | 229 | 5 | 61.8 |

### Same-name rebuilds (1738 pairs, 12.1 GiB)

| Strategy | Physical | Percent |
|---|---:|---:|
| exact/256KiB | 958.8 MiB | 7.749 |
| exact/64KiB | 796.9 MiB | 6.441 |
| exact/8KiB | 741.1 MiB | 5.990 |
| exact/1KiB | 1.2 GiB | 9.893 |
| slices/8KiB/16 | 919.4 MiB | 7.431 |
| slices/2KiB/16 | 737.4 MiB | 5.960 |
| slices/1KiB/16 | 670.3 MiB | 5.418 |
| slices/1KiB/4 | 568.3 MiB | 4.593 |
| slices/1KiB/1 | 514.6 MiB | 4.159 |

### Upgrades (25 pairs, 2.6 GiB)

| Strategy | Physical | Percent |
|---|---:|---:|
| exact/256KiB | 623.9 MiB | 23.876 |
| exact/64KiB | 624.2 MiB | 23.887 |
| exact/8KiB | 625.4 MiB | 23.932 |
| exact/1KiB | 776.2 MiB | 29.705 |
| slices/8KiB/16 | 623.8 MiB | 23.872 |
| slices/2KiB/16 | 543.9 MiB | 20.813 |
| slices/1KiB/16 | 516.3 MiB | 19.757 |
| slices/1KiB/4 | 482.3 MiB | 18.457 |
| slices/1KiB/1 | 464.8 MiB | 17.787 |

### Split by file layout

Pairs whose file layouts are identical (same relative paths, sizes and link
targets) are the candidates for a hash-only rebuild; the rest changed shape.

| Pairs | Bytes | exact/256KiB | exact/8KiB | slices/1KiB/4 | slices/1KiB/1 |
|---|---:|---:|---:|---:|---:|
| 1275 with equal layouts | 8.38 GiB | 4.34% | 1.59% | 1.04% | 0.64% |
| 488 with changed layouts | 6.26 GiB | 18.89% | 19.20% | 15.01% | 14.44% |

### Largest rebuilt paths

| Path | Category | Bytes | exact/256KiB | exact/8KiB | slices/1KiB/4 | slices/1KiB/1 |
|---|---|---:|---:|---:|---:|---:|
| cef-binary-6533 | rebuild | 1.9 GiB | 0.02% | 0.36% | 0.02% | 0.00% |
| zoom-7.1.5.4332 | rebuild | 915.4 MiB | 0.01% | 0.37% | 0.02% | 0.00% |
| factorseal-0.1.0 | rebuild | 805.7 MiB | 16.33% | 18.98% | 13.64% | 13.27% |
| linux-firmware-20260910-zstd | upgrade | 796.9 MiB | 6.20% | 6.55% | 6.41% | 6.13% |
| chromium-unwrapped-153.0.8010.36 | upgrade | 701.7 MiB | 28.33% | 31.44% | 22.87% | 21.99% |
| llvm-21.1.8-lib | rebuild | 540.2 MiB | 0.08% | 0.37% | 0.00% | 0.00% |
| factorseal-desktop-0.1.0 | rebuild | 525.6 MiB | 18.99% | 23.84% | 17.73% | 17.70% |
| 1password-8.12.21 | rebuild | 512.2 MiB | 0.16% | 0.38% | 0.01% | 0.00% |
| 1password-8.12.21 | rebuild | 512.2 MiB | 0.16% | 0.38% | 0.01% | 0.00% |
| firefox-unwrapped-155.0.1 | upgrade | 378.0 MiB | 27.83% | 26.99% | 19.60% | 19.03% |
| electron-unwrapped-43.1.0 | rebuild | 350.5 MiB | 0.46% | 0.51% | 0.07% | 0.07% |
| mesa-26.1.8 | rebuild | 264.4 MiB | 1.27% | 0.43% | 0.01% | 0.01% |
| gcc-15.3.0 | upgrade | 264.1 MiB | 32.11% | 31.49% | 25.23% | 24.32% |
| gcc-15.2.0 | rebuild | 263.8 MiB | 1.72% | 0.45% | 0.26% | 0.04% |
| qtdeclarative-6.11.1 | rebuild | 174.8 MiB | 6.17% | 1.73% | 1.48% | 0.63% |
| ibus-1.5.33 | rebuild | 149.6 MiB | 0.71% | 0.69% | 0.25% | 0.21% |
| python3-3.13.15 | rebuild | 126.8 MiB | 45.02% | 38.46% | 35.55% | 34.17% |
| qemu-user-10.2.4 | rebuild | 119.6 MiB | 7.62% | 0.82% | 0.03% | 0.02% |
| moby-29.7.2 | rebuild | 106.6 MiB | 20.92% | 20.06% | 15.33% | 15.00% |
| rclone-1.75.1 | upgrade | 106.4 MiB | 33.28% | 33.12% | 23.96% | 22.89% |

### What the closure changes

1. Across the whole delta the gain is modest. Slicing at 1 KiB sampled 1 in
   4 stores 7.0 percent of the rebuilt bytes against 10.6 percent for the
   production backend and 9.1 percent for 8 KiB exact chunks: 1.5 times and
   1.3 times fewer bytes, below the 2 times bar set for the six packages.
2. The residual is genuine content change, not chunking loss. The ten
   largest residual paths hold 65 percent of the slicing total and 54 percent
   of the exact total: browser and compiler upgrades, factorseal, python3
   (whose 3431 files differ substantially between the two builds), moby and
   rclone. No chunking strategy recovers bytes that are new.
3. Even same-layout rebuilds are not the hash-only case the six packages
   were. Their exact/256KiB cost is 4.3 percent, not 0.05 percent, and slicing
   at 1 in 4 brings it to 1.0 percent: a 4 times gain over the production
   backend and 1.5 times over 8 KiB exact chunks.
4. Reference chains are a real problem at closure scale. With 1 in 4
   sampling, 99 of 1763 paths exceed depth 4 and chromium reaches 55;
   unsampled reaches 229. Translation and data directories with hundreds of
   similar files chain each file onto the previous one. A backend must bound
   depth at ingest, not only in a background pass.
5. Encode CPU is 2.6 times the production cost over the closure: 59 s
   against 22 s single-threaded for 14.6 GiB, about 250 MiB/s.

### Overlapping the encoder with the link

The encoder streams window by window, and the frame of one window drains to
the peer while the next window is read and encoded. `sliced_pipeline` compares
that with encoding a whole 32 MiB literal-only frame before writing it, through
a writer paced at a fixed rate:

| Link rate | Overlapped | Sequential |
|---|---:|---:|
| 128 MiB/s | 371 ms | 434 ms |
| 256 MiB/s | 245 ms | 316 ms |
| 512 MiB/s | 174 ms | 209 ms |
| 1 GiB/s | 144 ms | 168 ms |

The gain is 15 to 22 percent wherever the link and the encoder are within an
order of magnitude of each other. On the 20 Mbit links above it is invisible,
because there the encoder is under 2 percent of the transfer.

### Sampling rate stays fixed

Scaling the sampling rate with blob size, as rsync scales its block size with
the square root of file size, would bound the index but points the wrong way
for this corpus: the largest pairs are the ones whose edits are densest. At
one sampled chunk in sixteen the icu4c rebuild costs 30.5 KiB against 2.1 KiB
at one in four, and gnumake 95.6 KiB against 10.0 KiB. The index is already
bounded per counterpart and by its entry cap, so the rate stays fixed.

Sampling denser than one in four is not worth it either. On the rebuilt
closure the wire cost is 72.2 MB at one in four, 70.7 MB at one in two and
69.8 MB with every chunk indexed: 3.3 percent for four times the index entries
and four times the hashing. The 157.6 MB the rebuild carries as literals is
content that changed, not content a sparser index missed.

### Compression level stays at the default

Literal segments compress at zstd's default level. Raising it trades encoder
CPU for wire bytes, and which way that pays depends entirely on the link. Over
20 Mbit at 50 ms, level 6 moves the rebuilt closure's cold sync from 525.7 MB
and 254 s to 498.9 MB and 245 s, a clear win. On an unpaced link the same
change takes the icu4c cold sync from 318 ms to 524 ms for 4.5 percent fewer
bytes, and level 9 takes it to 704 ms. Break-even is around 26 Mbit, which is
the middle of the range this is used over, so the level stays where it is: it
would have to be chosen per link to be chosen well.

## Findings

1. Slicing wins decisively on rebuilt store paths, but only with dense
   discovery. At 1 KiB discovery sampled 1 in 4 it stores 0.005 to 6.8
   percent of the rebuilt bytes; the production 256 KiB exact backend stores
   1.1 to 25.7 percent, and 8 KiB exact chunks store 0.4 to 14.2 percent.
2. Sampling 1 in 16, as proposed, is too sparse for typical store paths.
   Most files are small, so a file or a gap between two edits often holds no
   sampled chunk and is stored as a literal: gnumake stores 6.3 percent at
   1 in 16 against 0.66 percent at 1 in 4, and 2 KiB discovery at 1 in 16
   loses to 8 KiB exact chunks on three of six pairs. The loss is
   probabilistic, `(1 - 1/sample)^(chunks in the gap)`, and the synthetic
   64 KiB row shows it: 1 in 16 stores 251 KiB, 1 in 4 stores 1.7 KiB.
3. Small exact chunks pay in manifests, not payload. `exact/1KiB` finds almost
   every byte (8 KiB of literals on icu4c) but stores 1.1 MiB because each
   chunk costs a 40-byte manifest entry, and a verified 16 KiB group read
   touches 13 chunk objects on average. Slicing keeps one or two sources per
   group.
4. Reference chains grow without a bound. With unsampled discovery the 32
   gnumake files copy from each other to a depth of 13; 1 in 4 sampling
   reached depth 2. A real backend needs the proposed depth bound and a
   flattening job.
5. Ingest CPU rises about four to five times. Encoding the 38 MiB icu4c
   rebuild took 0.023 s with 256 KiB exact chunks and 0.10 s with 1 KiB
   discovery, about 370 MiB/s single-threaded, dominated by cutting and
   fingerprinting 1 KiB chunks. Matching is 0.02 s.
6. The cliffs are where the fixture predicts them. With an edit every 512 B
   no discovery chunk survives and every strategy stores about 70 percent
   (the compressible half of the corpus). With an edit every 4 KiB only
   unsampled 1 KiB discovery recovers most of the file. FastCDC at a 1 KiB
   average actually cuts about 1.3 KiB chunks.

## Decision

On hash-only rebuild pairs slicing wins by 2 to 78 times over 8 KiB exact
chunks, but a whole system rebuild is not made of hash-only rebuilds. Over
the 14.6 GiB generation 149 to 150 delta, slicing at 1 KiB sampled 1 in 4
stores 1.5 times fewer bytes than the production backend and 1.3 times fewer
than 8 KiB exact chunks, because most of what remains is new content. That
does not justify a second physical backend with its own GC edges, sync path
and depth bounds. Two cheaper moves capture most of the measurable gain:
a smaller exact chunk size (8 KiB exact chunks already store 1.2 times fewer
bytes than 256 KiB over the closure and 2.7 times fewer on same-layout
rebuilds, at the cost of a 10 times larger index), and pack-level compression
across neighbouring chunks. Slicing remains the right tool for a store that
mostly holds hash-only rebuilds, such as a binary cache fed by rebuilding one
package set against changing dependencies; the corpus suite can settle that
for any closure pair in about four minutes per GiB.

## Reproduce

```sh
cargo bench --features experimental --bench cdcs -- chunk_slicing
benchmark run cdcs-corpus --store-name icu4c-76.1 --store-name gmp-6.3.0 \
    --store-name zstd-1.5.7 --store-name libxcb-1.17.0 --store-name file-5.45 \
    --store-name gnumake-4.4.1
benchmark run cdcs-corpus --closure-base /nix/var/nix/profiles/system-149-link \
    --closure-rebuilt /nix/var/nix/profiles/system-150-link \
    --closure-wire --links 2560:50
```

`corpus.json` and `synthetic.json` are the raw results with their
environment records. `closure.json` keeps the closure summary, per-strategy
totals and, for each of the 1763 pairs, physical and literal bytes, depth and
encode time per strategy; the full per-row output is 18 MB and not retained.

`--closure-wire` adds the whole-closure transfer to that run: it derives the
same pairs, moves every rebuilt root as one request over each link, and fails
if a destination does not verify, if the rebuild sends more than a quarter of
the cold sync's bytes, or if it costs more than one request per root above a
fixed handshake. Importing a closure costs far more than syncing it, so pass
`--keep-work DIR` to reuse the import across runs.

## Wire transfer

The decision above is about storage. The same encoder is now the payload
format of the SSH transfer (`crates/casita/src/sync/sliced.rs`):
the receiver names the current and next closure of each root a transfer will
replace, the serving process pairs the two trees by path, indexes each held
counterpart when its wanted payload is requested, and each payload arrives as
copies of ranges the receiver holds plus compressed literals. Nothing is
stored in sliced form, so none of the index, chain, or GC costs apply; the
sender pays the indexing and matching CPU for one counterpart at a time and
reads its base bytes once per match.

`cargo bench --features ssh,experimental --bench sliced_transfer` on an 8 MiB
blob rebuilt with a different 32-byte hash every `spacing` bytes, two
in-memory repositories over the stdio protocol, one repetition per row
(`wire.json`):

| Spacing | Phase | Wire bytes | Percent | Copied | Literal | Sync wall |
|---|---|---:|---:|---:|---:|---:|
| any | cold, empty destination | 8,389,315 | 100 | 0 | 8,388,608 | 28 ms |
| 512 B | rebuild | 8,389,332 | 100 | 0 | 8,388,608 | 49 ms |
| 64 KiB | rebuild | 11,006 | 0.131 | 8,384,512 | 4,096 | 51 ms |
| 1 MiB | rebuild | 1,046 | 0.0125 | 8,388,352 | 256 | 49 ms |

Criterion medians for the codec on the same blob: indexing the base 12 ms
(665 MiB/s); encoding 23 ms at 512 B spacing, 8 ms at 64 KiB and at 1 MiB;
decoding about 1 ms in every case because copies are local reads and random
literals are stored raw by zstd. A rebuild sync takes about 50 ms at every
spacing against 27 ms for a cold sync; the difference is indexing the base,
matching, and rewriting the copied bytes through the receiver's chunked
writer. Slice sources keep a 4 MiB decoded window per open blob, so
candidate checks and match extension no longer decode a stored chunk per
kilobyte compared; before that cache the 64 KiB rebuild took 125 ms.

Below the discovery chunk the frame is the whole blob in compressed
segments, so the transfer never costs more than a plain compressed stream.
The hash-only rebuild pairs measured at the top of this report would move at
the same ratios as their `slices/1KiB/4` rows.


## Rebuilt store paths over a slow link

`benchmark run cdcs-corpus --wire` imports each pair into a source and a
destination repository and syncs the rebuilt root through the stdio transfer
protocol behind a relay that delays each direction by half the round trip
and paces it at the link rate (`wire-corpus.json`). Cold is a sync into an
empty destination, rebuild a sync into the destination that holds the base;
every destination closure verified complete. Down is what the server sent.

| Pair | Rebuilt | Cold down | Rebuild down | Requests | unpaced cold / rebuild | 100 Mbit, 5 ms cold / rebuild | 20 Mbit, 50 ms cold / rebuild |
|---|---:|---:|---:|---:|---:|---:|---:|
| icu4c-76.1 | 38.2 MiB | 14.3 MiB | 5.0 KiB | 6 / 3 | 317 / 66 ms | 1293 / 76 ms | 6170 / 190 ms |
| gmp-6.3.0 | 729.5 KiB | 335.5 KiB | 2.3 KiB | 1 / 2 | 15 / 13 ms | 48 / 20 ms | 221 / 92 ms |
| zstd-1.5.7 | 1.2 MiB | 470.5 KiB | 3.5 KiB | 1 / 2 | 16 / 12 ms | 62 / 25 ms | 276 / 91 ms |
| libxcb-1.17.0 | 1.4 MiB | 378.0 KiB | 104.9 KiB | 1 / 2 | 21 / 23 ms | 51 / 34 ms | 236 / 134 ms |
| file-5.45 | 8.3 MiB | 457.2 KiB | 4.7 KiB | 2 / 2 | 44 / 8 ms | 79 / 15 ms | 338 / 86 ms |
| gnumake-4.4.1 | 1.5 MiB | 659.3 KiB | 22.6 KiB | 3 / 2 | 28 / 11 ms | 94 / 19 ms | 463 / 94 ms |

Bytes on the wire fall by 40 to 3,000 times on the hash-only rebuilds and by
4 to 30 times on libxcb and gnumake, whose content changed. Requests are
pipelined on one connection, the base offer does not wait for its answer, and
a discovery request is answered with the records of the reachable closure plus
the first payloads outside held subtrees, at most 64 of them and 4 MiB. What
bounds a small sync is therefore not how many requests it makes but how deep
the chain is. Measured as wall time per round trip at a 500 ms round trip,
which is stable to a few percent under load, a rebuild sync of five of these
six pairs costs 1.5 round trips: half of one for the handshake and one for the
discovery answer. On the 20 Mbit link those five rebuilds are two to five
times faster than their cold syncs and icu4c is 32 times faster, where before
discovery they gained a third at most. What remains for a small tree is the
handshake and the payloads above the answer's budget.

icu4c is the exception at 3.8 round trips, because its eight files are each
larger than a discovery answer's payload budget and follow in a wave of their
own. A payload batch of 16 MiB is what lets that wave be one request rather
than four.

The wall columns compare within a run and not across days; bytes and request
counts compare across both. A batch bound swept on one host moves the icu4c
rebuild by 14 ms across a fourfold change, which is inside the spread between
runs of a single binary, so a timing that moved between two tables measured on
different days says nothing about the code.

### A rebuilt closure over the wire

349 store paths of the generation 149 to 150 delta, 1.40 GiB, imported into
a source and a destination repository on disk and moved as one transfer
request carrying every rebuilt root, over 20 Mbit at 50 ms
(`closure-wire.json`). Cold is a sync into an empty destination, rebuild a
sync into one holding the old generation.

| Phase | Down | Requests | Copied | Literal | Wall |
|---|---:|---:|---:|---:|---:|
| cold | 525.7 MB | 179 | 0 | 1.50 GB | 258 s |
| rebuild | 65.5 MB | 134 | 903.2 MB | 157.5 MB | 54 s |

A rebuild moves 8.0 times fewer bytes than a cold sync and publishes 4,186
of the closure's 25,781 objects; the rest are unchanged and cost nothing.
That is still short of the 40 to 3,000 times the single hash-only packages
showed, because a system rebuild is not made of hash-only rebuilds: of the
1.06 GB the changed objects hold, 903.2 MB is copied from the receiver's
counterparts and 157.5 MB is content that genuinely changed.

Payloads were bounded at 64 MiB for slicing until the pack read path stopped
waiting on its shared buffer budget, and that bound was most of the gap. It
sent this corpus's largest paths, up to 350 MiB, as literals however little
of them changed: down was 171.4 MB, copies 641.5 MB, literals 419.3 MB. With
every payload sliced the wire cost falls by 2.4 times over the same 433
requests. The serving side deferred read-ahead 83 times out of 11,148 pack
range requests and never fetched uncharged, which is the budget doing its
work rather than running out.

The table is an optimized build. Of the rebuild's 54 s the link carries 25 s
and the round trips 7 s, and of the cold sync's 258 s the link carries 200 s
and the round trips 9 s, so both are bounded by bytes now rather than by
latency. Wall times still move with host load between runs, by a third in the
worst case seen here, and compare only within a run.

Discovery carries the closure: 21 discovery answers describe all 25,781
objects and no object batch is needed. An earlier cap on the record cache
refused new records once full, which cost 90 extra round trips and 12
percent more bytes on this corpus; rounds now drop what they finished with
instead.

What remains is one round trip per payload batch, and a round carries what one
batch can hold. At 1 MiB and 64 objects a batch that was the closure cost 926
requests cold and 433 on the rebuild; at 16 MiB and 256 objects the same bytes
cost 179 and 133. Sweeping the bound on the rebuild gives 227 requests and
62 s at 4 MiB, 168 and 58 s at 8, 133 and 55 s at 16, and 114 and 55 s at 32:
once round trips stop being the bound the curve flattens, and the larger
buffer past 16 MiB buys nothing.

Two bounds had to be separated from the batch to keep that free. A discovery
answer volunteers payloads the receiver has not asked for, and widening that
guess along with the batch sent 36 MB the receiver threw away, 7.1 MB of it on
the wire; the answer keeps its own narrower bound. And a receiver that held
only one answer's worth of volunteered payloads dropped 405 of them, 9.1 MB,
that had already arrived, because the next answer comes while the last is
still being staged; it now holds four.

Even at that bound the guess was wrong often enough to matter. A rebuild threw
away 957 volunteered payloads, 17.9 MB of plaintext and 6.6 MB on the wire, or
9 percent of everything it received. Volunteering now depends on what the
answer is for. A receiver that has offered no bases holds nothing, so every
payload it is sent is wanted, and the cold sync above is byte for byte what it
was. A receiver naming one or two objects is syncing a tree, and the answer is
all the work it has. A receiver naming a frontier of hundreds is walking a
closure with a payload batch pipeline behind it, and that pipeline asks the
destination what it already holds before requesting anything, which is the one
fact the serving side does not have. The rebuild costs 65.5 MB instead of
72.2 MB and one more request. Turning the guess off entirely costs 65.5 MB
too, so the rule keeps what it was worth and drops what it was not.

### Where the time goes

`sliced_cost` in the same target splits one 8 MiB rebuild between the two
sides against the real chunked backend, single threaded. Absolute times on
this host moved by about 30 percent between runs depending on other load, so
the columns below come from two runs at similar load, before and after
sampling from the gear hash and extending matches by 4 KiB block compares.

| Phase | Before | After | Note |
|---|---:|---:|---|
| index the counterpart | 14.8 ms | 7.6 ms | only sampled chunks are hashed |
| server encode, 512 B spacing | 12.4 ms | 6.2 ms | literal only: cut plus compress |
| server encode, 64 KiB spacing | 10.9 ms | 7.5 ms | cut, sampled hashes, base reads, block compares |
| server encode, 1 MiB spacing | 10.1 ms | 7.0 ms | |
| receiver decode to a sink, 64 KiB | 2.7 ms | 2.7 ms | base reads through the chunked store |
| receiver decode into the store, 64 KiB | 12.3 ms | 12.3 ms | plus re-chunking, hashing and compressing |
| plain write of the base bytes | 4.8 ms | 4.8 ms | cut and hash only, every chunk already present |

After the two changes a rebuild costs the server about 15 ms per 8 MiB
(indexing plus encoding, about 550 MiB/s) and the receiver about 12 ms,
against about 12 ms and 6 ms for a plain compressed send. The receiver's
remaining cost is rewriting copied bytes through the chunked writer, which
recompresses every chunk an edit touches; with an edit every 64 KiB that is
every chunk. Committing copied ranges as references to existing chunks would
remove it for sparse edits but costs more than a rewrite for dense ones, so
it is not done.

That was the argument; here is what it is worth. A rebuilt closure is mostly
sparse edits, so the rate to scale by is the one at 1 MiB spacing rather than
64 KiB, which the table above never carried:

| Step, 8 MiB | Time | Rate |
|---|---:|---:|
| decode to a sink, 1 MiB spacing | 3.1 ms | 2,697 MB/s |
| plain write, every chunk already present | 5.3 ms | 1,584 MB/s |
| decode into the store, 1 MiB spacing | 10.6 ms | 793 MB/s |

The rebuilt closure writes 1.06 GB through that last path, which is 1.3 s of
its 54 s. Committing references instead would bound it below by the decode,
since the payload hash has to be verified over the whole plaintext either way,
so the most the change can win is about a second: under 2 percent of the sync,
against a manifest that would have to name chunks it did not cut. The receiver
rewrite is the dominant cost of a single 8 MiB pair and a rounding error on a
closure, which is why it stays a rewrite.
