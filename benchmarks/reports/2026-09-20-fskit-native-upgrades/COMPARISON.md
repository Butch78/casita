# Completed submatrices from an incomplete run

The overall five-round workload run failed on a host awk timeout during round four. These tables validate all five ordinary-operation rounds and only the first three complete workload rounds. The incomplete fourth workload round is excluded in full. This is shared-host diagnostic evidence, not a completed full benchmark run.

## Repository versus host: five completed rounds

Median across five per-round p50 values, with the round range. Units: milliseconds.

| Case | Size / workers | Host | FSKit | FSKit / host |
| --- | --- | ---: | ---: | ---: |
| readdir |  | 0.166 (0.165–0.181) | 0.980 (0.938–1.402) | 5.91x |
| stat-256 |  | 1.619 (1.612–1.824) | 1.701 (1.642–1.983) | 1.05x |
| open-read-close | 4096 | 0.019 (0.019–0.021) | 0.019 (0.019–0.020) | 1.01x |
| open-read-close | 1048576 | 0.074 (0.073–0.079) | 0.073 (0.071–0.080) | 0.99x |
| held-fd-read | 4096 | 0.001 (0.001–0.001) | 0.001 (0.001–0.001) | 1.00x |
| held-fd-read | 1048576 | 0.061 (0.058–0.061) | 0.060 (0.058–0.061) | 0.98x |
| parallel-read-32 | 16 | 1.704 (1.164–1.731) | 1.667 (1.175–1.805) | 0.98x |
| execute-script |  | 3.991 (3.680–4.299) | 3.928 (3.637–4.118) | 0.98x |
| execute-native |  | 2.521 (2.358–2.705) | 2.602 (2.400–2.801) | 1.03x |

## Executable workloads

Median batch wall time in milliseconds, with the 3-round range. Fresh mount or host inode for first; immediate repeat for warm.

| Pattern | Workers | Phase | Host | FSKit | FSKit / host |
| --- | ---: | --- | ---: | ---: | ---: |
| shared | 1 | first | 219.158 (188.512–786.268) | 297.555 (281.560–2744.591) | 1.36x |
| shared | 1 | repeat | 11.136 (10.783–21.576) | 11.607 (11.120–23.598) | 1.04x |
| shared | 8 | first | 240.793 (216.886–832.122) | 364.047 (320.973–443.053) | 1.51x |
| shared | 8 | repeat | 43.653 (43.312–46.349) | 45.217 (42.323–46.007) | 1.04x |
| shared | 15 | first | 243.191 (241.643–266.095) | 365.481 (364.118–377.884) | 1.50x |
| shared | 15 | repeat | 89.085 (87.228–91.262) | 89.337 (80.050–91.112) | 1.00x |
| shared | 16 | first | 247.126 (232.161–251.846) | 368.506 (362.481–368.984) | 1.49x |
| shared | 16 | repeat | 91.887 (87.357–95.261) | 95.379 (87.520–95.833) | 1.04x |
| shared | 17 | first | 251.357 (246.889–273.438) | 376.147 (375.708–388.926) | 1.50x |
| shared | 17 | repeat | 92.987 (92.903–96.195) | 97.085 (96.380–103.520) | 1.04x |
| shared | 31 | first | 303.268 (298.618–314.299) | 442.446 (422.734–489.288) | 1.46x |
| shared | 31 | repeat | 185.471 (181.821–187.166) | 180.809 (175.194–188.298) | 0.97x |
| shared | 32 | first | 315.134 (294.986–317.663) | 435.657 (434.948–437.299) | 1.38x |
| shared | 32 | repeat | 193.052 (187.368–194.510) | 187.208 (159.723–196.146) | 0.97x |
| shared | 33 | first | 306.102 (299.138–319.177) | 468.745 (429.108–478.944) | 1.53x |
| shared | 33 | repeat | 188.607 (186.206–197.688) | 199.531 (198.613–205.176) | 1.06x |
| distinct | 1 | first | 221.752 (207.589–335.125) | 296.250 (292.605–303.035) | 1.34x |
| distinct | 1 | repeat | 11.214 (11.138–12.003) | 10.962 (10.939–11.093) | 0.98x |
| distinct | 8 | first | 1223.950 (1213.386–1244.062) | 1746.396 (1728.271–1747.264) | 1.43x |
| distinct | 8 | repeat | 50.537 (46.061–52.994) | 48.458 (43.606–52.790) | 0.96x |
| distinct | 15 | first | 2189.233 (2179.200–2228.481) | 3178.018 (3085.477–3238.097) | 1.45x |
| distinct | 15 | repeat | 88.530 (86.449–94.629) | 90.939 (79.288–93.587) | 1.03x |
| distinct | 16 | first | 2348.794 (2343.099–2419.211) | 3415.763 (3309.337–3476.065) | 1.45x |
| distinct | 16 | repeat | 90.334 (86.096–97.703) | 92.682 (83.078–99.841) | 1.03x |
| distinct | 17 | first | 2478.618 (2466.025–2485.693) | 3585.275 (3524.911–3696.468) | 1.45x |
| distinct | 17 | repeat | 99.604 (87.518–103.380) | 100.056 (85.750–103.078) | 1.00x |
| distinct | 31 | first | 4464.799 (4464.746–4502.214) | 6495.267 (6366.661–6530.918) | 1.45x |
| distinct | 31 | repeat | 184.449 (183.687–184.901) | 183.590 (183.367–185.560) | 1.00x |
| distinct | 32 | first | 4629.657 (4616.787–4635.108) | 6647.001 (6561.790–6763.004) | 1.44x |
| distinct | 32 | repeat | 182.821 (181.569–195.619) | 191.465 (187.171–193.127) | 1.05x |
| distinct | 33 | first | 4764.442 (4587.450–4887.595) | 7033.170 (6958.925–7099.876) | 1.48x |
| distinct | 33 | repeat | 194.201 (187.345–198.888) | 195.526 (180.357–199.214) | 1.01x |
