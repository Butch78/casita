# Casita S3 path-selected transfer benchmark

Endpoint: `rustfs+controlled-links://loopback`. Latency label: `controlled-rustfs-links`.

The benchmark uses the real S3/wal3 repository profile. Import, source open, and destination validation are outside each timed transfer.

| Transport | RTT | Depth | Subtree files | Cache | Phase | n | Median | p95 | Range GETs | Whole GETs | Cache hits | Source bytes | wal3 refreshes |
|---|---:|---:|---:|---:|---|---:|---:|---:|---:|---:|---:|---:|---:|
| direct-s3 | 0 ms | 0 | 1 | 0 MiB | cold | 10 | 0.0052 s | 0.0111 s | 3 | 0 | 0 | 4.2 KiB | 1 |
| direct-s3 | 0 ms | 0 | 1 | 0 MiB | warm | 10 | 0.0038 s | 0.0072 s | 3 | 0 | 0 | 4.2 KiB | 1 |
| direct-s3 | 0 ms | 0 | 1 | 64 MiB | cold | 10 | 0.0057 s | 0.0320 s | 2 | 1 | 0 | 4.4 KiB | 1 |
| direct-s3 | 0 ms | 0 | 1 | 64 MiB | warm | 10 | 0.0025 s | 0.0051 s | 0 | 1 | 2 | 4.1 KiB | 1 |
| direct-s3 | 0 ms | 0 | 16 | 0 MiB | cold | 10 | 0.0197 s | 0.0708 s | 18 | 0 | 0 | 64.8 KiB | 1 |
| direct-s3 | 0 ms | 0 | 16 | 0 MiB | warm | 10 | 0.0149 s | 0.0504 s | 18 | 0 | 0 | 64.8 KiB | 1 |
| direct-s3 | 0 ms | 0 | 16 | 64 MiB | cold | 10 | 0.0069 s | 0.0278 s | 2 | 2 | 14 | 69.9 KiB | 1 |
| direct-s3 | 0 ms | 0 | 16 | 64 MiB | warm | 10 | 0.0017 s | 0.0039 s | 0 | 0 | 18 | 0.0 B | 1 |
| direct-s3 | 0 ms | 0 | 64 | 0 MiB | cold | 10 | 0.0467 s | 0.2426 s | 66 | 0 | 0 | 258.9 KiB | 1 |
| direct-s3 | 0 ms | 0 | 64 | 0 MiB | warm | 10 | 0.0309 s | 0.1721 s | 66 | 0 | 0 | 258.9 KiB | 1 |
| direct-s3 | 0 ms | 0 | 64 | 64 MiB | cold | 10 | 0.0112 s | 0.0246 s | 2 | 2 | 62 | 266.6 KiB | 1 |
| direct-s3 | 0 ms | 0 | 64 | 64 MiB | warm | 10 | 0.0044 s | 0.0058 s | 0 | 0 | 66 | 0.0 B | 1 |
| direct-s3 | 0 ms | 4 | 1 | 0 MiB | cold | 10 | 0.0076 s | 0.0337 s | 7 | 0 | 0 | 4.6 KiB | 1 |
| direct-s3 | 0 ms | 4 | 1 | 0 MiB | warm | 10 | 0.0068 s | 0.0383 s | 7 | 0 | 0 | 4.6 KiB | 1 |
| direct-s3 | 0 ms | 4 | 1 | 64 MiB | cold | 10 | 0.0051 s | 0.0262 s | 2 | 1 | 4 | 5.1 KiB | 1 |
| direct-s3 | 0 ms | 4 | 1 | 64 MiB | warm | 10 | 0.0021 s | 0.0112 s | 0 | 1 | 6 | 20.3 KiB | 1 |
| direct-s3 | 0 ms | 4 | 16 | 0 MiB | cold | 10 | 0.0218 s | 0.0891 s | 22 | 0 | 0 | 65.3 KiB | 1 |
| direct-s3 | 0 ms | 4 | 16 | 0 MiB | warm | 10 | 0.0198 s | 0.0815 s | 22 | 0 | 0 | 65.3 KiB | 1 |
| direct-s3 | 0 ms | 4 | 16 | 64 MiB | cold | 10 | 0.0070 s | 0.0424 s | 2 | 2 | 18 | 87.0 KiB | 1 |
| direct-s3 | 0 ms | 4 | 16 | 64 MiB | warm | 10 | 0.0022 s | 0.0090 s | 0 | 0 | 22 | 0.0 B | 1 |
| direct-s3 | 0 ms | 4 | 64 | 0 MiB | cold | 10 | 0.0383 s | 0.3067 s | 70 | 0 | 0 | 259.4 KiB | 1 |
| direct-s3 | 0 ms | 4 | 64 | 0 MiB | warm | 10 | 0.0264 s | 0.0523 s | 70 | 0 | 0 | 259.4 KiB | 1 |
| direct-s3 | 0 ms | 4 | 64 | 64 MiB | cold | 10 | 0.0098 s | 0.0370 s | 2 | 2 | 66 | 283.7 KiB | 1 |
| direct-s3 | 0 ms | 4 | 64 | 64 MiB | warm | 10 | 0.0038 s | 0.0081 s | 0 | 0 | 70 | 0.0 B | 1 |
| direct-s3 | 0 ms | 16 | 1 | 0 MiB | cold | 10 | 0.0231 s | 0.0879 s | 19 | 0 | 0 | 6.1 KiB | 1 |
| direct-s3 | 0 ms | 16 | 1 | 0 MiB | warm | 10 | 0.0201 s | 0.0494 s | 19 | 0 | 0 | 6.1 KiB | 1 |
| direct-s3 | 0 ms | 16 | 1 | 64 MiB | cold | 10 | 0.0060 s | 0.0524 s | 2 | 1 | 16 | 7.3 KiB | 1 |
| direct-s3 | 0 ms | 16 | 1 | 64 MiB | warm | 10 | 0.0029 s | 0.0104 s | 0 | 1 | 18 | 69.1 KiB | 1 |
| direct-s3 | 0 ms | 16 | 16 | 0 MiB | cold | 10 | 0.0338 s | 0.1238 s | 34 | 0 | 0 | 66.8 KiB | 1 |
| direct-s3 | 0 ms | 16 | 16 | 0 MiB | warm | 10 | 0.0297 s | 0.0894 s | 34 | 0 | 0 | 66.8 KiB | 1 |
| direct-s3 | 0 ms | 16 | 16 | 64 MiB | cold | 10 | 0.0087 s | 0.0375 s | 2 | 2 | 30 | 137.9 KiB | 1 |
| direct-s3 | 0 ms | 16 | 16 | 64 MiB | warm | 10 | 0.0022 s | 0.0046 s | 0 | 0 | 34 | 0.0 B | 1 |
| direct-s3 | 0 ms | 16 | 64 | 0 MiB | cold | 10 | 0.0579 s | 0.2296 s | 82 | 0 | 0 | 260.9 KiB | 1 |
| direct-s3 | 0 ms | 16 | 64 | 0 MiB | warm | 10 | 0.0407 s | 0.1874 s | 82 | 0 | 0 | 260.9 KiB | 1 |
| direct-s3 | 0 ms | 16 | 64 | 64 MiB | cold | 10 | 0.0113 s | 0.0722 s | 2 | 2 | 78 | 334.6 KiB | 1 |
| direct-s3 | 0 ms | 16 | 64 | 64 MiB | warm | 10 | 0.0042 s | 0.0163 s | 0 | 0 | 82 | 0.0 B | 1 |
| direct-s3 | 30 ms | 0 | 1 | 0 MiB | cold | 10 | 0.1306 s | 0.1675 s | 3 | 0 | 0 | 4.2 KiB | 1 |
| direct-s3 | 30 ms | 0 | 1 | 0 MiB | warm | 10 | 0.1286 s | 0.1397 s | 3 | 0 | 0 | 4.2 KiB | 1 |
| direct-s3 | 30 ms | 0 | 1 | 64 MiB | cold | 10 | 0.1286 s | 0.1333 s | 2 | 1 | 0 | 4.4 KiB | 1 |
| direct-s3 | 30 ms | 0 | 1 | 64 MiB | warm | 10 | 0.0643 s | 0.0672 s | 0 | 1 | 2 | 4.1 KiB | 1 |
| direct-s3 | 30 ms | 0 | 16 | 0 MiB | cold | 10 | 0.1638 s | 0.1946 s | 18 | 0 | 0 | 64.8 KiB | 1 |
| direct-s3 | 30 ms | 0 | 16 | 0 MiB | warm | 10 | 0.1334 s | 0.1594 s | 18 | 0 | 0 | 64.8 KiB | 1 |
| direct-s3 | 30 ms | 0 | 16 | 64 MiB | cold | 10 | 0.1307 s | 0.2158 s | 2 | 2 | 14 | 69.9 KiB | 1 |
| direct-s3 | 30 ms | 0 | 16 | 64 MiB | warm | 10 | 0.0326 s | 0.0400 s | 0 | 0 | 18 | 0.0 B | 1 |
| direct-s3 | 30 ms | 0 | 64 | 0 MiB | cold | 10 | 0.2615 s | 0.2723 s | 66 | 0 | 0 | 258.9 KiB | 1 |
| direct-s3 | 30 ms | 0 | 64 | 0 MiB | warm | 10 | 0.2323 s | 0.2406 s | 66 | 0 | 0 | 258.9 KiB | 1 |
| direct-s3 | 30 ms | 0 | 64 | 64 MiB | cold | 10 | 0.1323 s | 0.1592 s | 2 | 2 | 62 | 266.6 KiB | 1 |
| direct-s3 | 30 ms | 0 | 64 | 64 MiB | warm | 10 | 0.0340 s | 0.0364 s | 0 | 0 | 66 | 0.0 B | 1 |
| direct-s3 | 30 ms | 4 | 1 | 0 MiB | cold | 10 | 0.2554 s | 0.2770 s | 7 | 0 | 0 | 4.6 KiB | 1 |
| direct-s3 | 30 ms | 4 | 1 | 0 MiB | warm | 10 | 0.2544 s | 0.2796 s | 7 | 0 | 0 | 4.6 KiB | 1 |
| direct-s3 | 30 ms | 4 | 1 | 64 MiB | cold | 10 | 0.1286 s | 0.1319 s | 2 | 1 | 4 | 5.1 KiB | 1 |
| direct-s3 | 30 ms | 4 | 1 | 64 MiB | warm | 10 | 0.0641 s | 0.0675 s | 0 | 1 | 6 | 20.3 KiB | 1 |
| direct-s3 | 30 ms | 4 | 16 | 0 MiB | cold | 10 | 0.2928 s | 0.3857 s | 22 | 0 | 0 | 65.3 KiB | 1 |
| direct-s3 | 30 ms | 4 | 16 | 0 MiB | warm | 10 | 0.2678 s | 0.3352 s | 22 | 0 | 0 | 65.3 KiB | 1 |
| direct-s3 | 30 ms | 4 | 16 | 64 MiB | cold | 10 | 0.1307 s | 0.1447 s | 2 | 2 | 18 | 87.0 KiB | 1 |
| direct-s3 | 30 ms | 4 | 16 | 64 MiB | warm | 10 | 0.0330 s | 0.0343 s | 0 | 0 | 22 | 0.0 B | 1 |
| direct-s3 | 30 ms | 4 | 64 | 0 MiB | cold | 10 | 0.3885 s | 0.7753 s | 70 | 0 | 0 | 259.4 KiB | 1 |
| direct-s3 | 30 ms | 4 | 64 | 0 MiB | warm | 10 | 0.3583 s | 0.5420 s | 70 | 0 | 0 | 259.4 KiB | 1 |
| direct-s3 | 30 ms | 4 | 64 | 64 MiB | cold | 10 | 0.1318 s | 0.1622 s | 2 | 2 | 66 | 283.7 KiB | 1 |
| direct-s3 | 30 ms | 4 | 64 | 64 MiB | warm | 10 | 0.0341 s | 0.0405 s | 0 | 0 | 70 | 0.0 B | 1 |
| direct-s3 | 30 ms | 16 | 1 | 0 MiB | cold | 10 | 0.6366 s | 0.6619 s | 19 | 0 | 0 | 6.1 KiB | 1 |
| direct-s3 | 30 ms | 16 | 1 | 0 MiB | warm | 10 | 0.6358 s | 0.6930 s | 19 | 0 | 0 | 6.1 KiB | 1 |
| direct-s3 | 30 ms | 16 | 1 | 64 MiB | cold | 10 | 0.1295 s | 0.1446 s | 2 | 1 | 16 | 7.3 KiB | 1 |
| direct-s3 | 30 ms | 16 | 1 | 64 MiB | warm | 10 | 0.0653 s | 0.0705 s | 0 | 1 | 18 | 69.1 KiB | 1 |
| direct-s3 | 30 ms | 16 | 16 | 0 MiB | cold | 10 | 0.6747 s | 0.8306 s | 34 | 0 | 0 | 66.8 KiB | 1 |
| direct-s3 | 30 ms | 16 | 16 | 0 MiB | warm | 10 | 0.6446 s | 0.7810 s | 34 | 0 | 0 | 66.8 KiB | 1 |
| direct-s3 | 30 ms | 16 | 16 | 64 MiB | cold | 10 | 0.1337 s | 0.1901 s | 2 | 2 | 30 | 137.9 KiB | 1 |
| direct-s3 | 30 ms | 16 | 16 | 64 MiB | warm | 10 | 0.0340 s | 0.0355 s | 0 | 0 | 34 | 0.0 B | 1 |
| direct-s3 | 30 ms | 16 | 64 | 0 MiB | cold | 10 | 0.7749 s | 0.9080 s | 82 | 0 | 0 | 260.9 KiB | 1 |
| direct-s3 | 30 ms | 16 | 64 | 0 MiB | warm | 10 | 0.7423 s | 0.8994 s | 82 | 0 | 0 | 260.9 KiB | 1 |
| direct-s3 | 30 ms | 16 | 64 | 64 MiB | cold | 10 | 0.1331 s | 0.1646 s | 2 | 2 | 78 | 334.6 KiB | 1 |
| direct-s3 | 30 ms | 16 | 64 | 64 MiB | warm | 10 | 0.0343 s | 0.0512 s | 0 | 0 | 82 | 0.0 B | 1 |
| direct-s3 | 80 ms | 0 | 1 | 0 MiB | cold | 10 | 0.3291 s | 0.3729 s | 3 | 0 | 0 | 4.2 KiB | 1 |
| direct-s3 | 80 ms | 0 | 1 | 0 MiB | warm | 10 | 0.3285 s | 0.3356 s | 3 | 0 | 0 | 4.2 KiB | 1 |
| direct-s3 | 80 ms | 0 | 1 | 64 MiB | cold | 10 | 0.3297 s | 0.4145 s | 2 | 1 | 0 | 4.4 KiB | 1 |
| direct-s3 | 80 ms | 0 | 1 | 64 MiB | warm | 10 | 0.1643 s | 0.1750 s | 0 | 1 | 2 | 4.1 KiB | 1 |
| direct-s3 | 80 ms | 0 | 16 | 0 MiB | cold | 10 | 0.4148 s | 0.4415 s | 18 | 0 | 0 | 64.8 KiB | 1 |
| direct-s3 | 80 ms | 0 | 16 | 0 MiB | warm | 10 | 0.3332 s | 0.3617 s | 18 | 0 | 0 | 64.8 KiB | 1 |
| direct-s3 | 80 ms | 0 | 16 | 64 MiB | cold | 10 | 0.3314 s | 0.3443 s | 2 | 2 | 14 | 69.9 KiB | 1 |
| direct-s3 | 80 ms | 0 | 16 | 64 MiB | warm | 10 | 0.0833 s | 0.0888 s | 0 | 0 | 18 | 0.0 B | 1 |
| direct-s3 | 80 ms | 0 | 64 | 0 MiB | cold | 10 | 0.6639 s | 0.9251 s | 66 | 0 | 0 | 258.9 KiB | 1 |
| direct-s3 | 80 ms | 0 | 64 | 0 MiB | warm | 10 | 0.5893 s | 0.8003 s | 66 | 0 | 0 | 258.9 KiB | 1 |
| direct-s3 | 80 ms | 0 | 64 | 64 MiB | cold | 10 | 0.3327 s | 0.3456 s | 2 | 2 | 62 | 266.6 KiB | 1 |
| direct-s3 | 80 ms | 0 | 64 | 64 MiB | warm | 10 | 0.0839 s | 0.0869 s | 0 | 0 | 66 | 0.0 B | 1 |
| direct-s3 | 80 ms | 4 | 1 | 0 MiB | cold | 10 | 0.6558 s | 0.6793 s | 7 | 0 | 0 | 4.6 KiB | 1 |
| direct-s3 | 80 ms | 4 | 1 | 0 MiB | warm | 10 | 0.6562 s | 0.6927 s | 7 | 0 | 0 | 4.6 KiB | 1 |
| direct-s3 | 80 ms | 4 | 1 | 64 MiB | cold | 10 | 0.3314 s | 0.3540 s | 2 | 1 | 4 | 5.1 KiB | 1 |
| direct-s3 | 80 ms | 4 | 1 | 64 MiB | warm | 10 | 0.1646 s | 0.1679 s | 0 | 1 | 6 | 20.3 KiB | 1 |
| direct-s3 | 80 ms | 4 | 16 | 0 MiB | cold | 10 | 0.7477 s | 0.7996 s | 22 | 0 | 0 | 65.3 KiB | 1 |
| direct-s3 | 80 ms | 4 | 16 | 0 MiB | warm | 10 | 0.6636 s | 0.7385 s | 22 | 0 | 0 | 65.3 KiB | 1 |
| direct-s3 | 80 ms | 4 | 16 | 64 MiB | cold | 10 | 0.3307 s | 0.3448 s | 2 | 2 | 18 | 87.0 KiB | 1 |
| direct-s3 | 80 ms | 4 | 16 | 64 MiB | warm | 10 | 0.0832 s | 0.0848 s | 0 | 0 | 22 | 0.0 B | 1 |
| direct-s3 | 80 ms | 4 | 64 | 0 MiB | cold | 10 | 0.9918 s | 1.2699 s | 70 | 0 | 0 | 259.4 KiB | 1 |
| direct-s3 | 80 ms | 4 | 64 | 0 MiB | warm | 10 | 0.9104 s | 1.0662 s | 70 | 0 | 0 | 259.4 KiB | 1 |
| direct-s3 | 80 ms | 4 | 64 | 64 MiB | cold | 10 | 0.3332 s | 0.3563 s | 2 | 2 | 66 | 283.7 KiB | 1 |
| direct-s3 | 80 ms | 4 | 64 | 64 MiB | warm | 10 | 0.0841 s | 0.0881 s | 0 | 0 | 70 | 0.0 B | 1 |
| direct-s3 | 80 ms | 16 | 1 | 0 MiB | cold | 10 | 1.6428 s | 1.7154 s | 19 | 0 | 0 | 6.1 KiB | 1 |
| direct-s3 | 80 ms | 16 | 1 | 0 MiB | warm | 10 | 1.6416 s | 1.7793 s | 19 | 0 | 0 | 6.1 KiB | 1 |
| direct-s3 | 80 ms | 16 | 1 | 64 MiB | cold | 10 | 0.3311 s | 0.3664 s | 2 | 1 | 16 | 7.3 KiB | 1 |
| direct-s3 | 80 ms | 16 | 1 | 64 MiB | warm | 10 | 0.1650 s | 0.1749 s | 0 | 1 | 18 | 69.1 KiB | 1 |
| direct-s3 | 80 ms | 16 | 16 | 0 MiB | cold | 10 | 1.7310 s | 1.7877 s | 34 | 0 | 0 | 66.8 KiB | 1 |
| direct-s3 | 80 ms | 16 | 16 | 0 MiB | warm | 10 | 1.6488 s | 1.6888 s | 34 | 0 | 0 | 66.8 KiB | 1 |
| direct-s3 | 80 ms | 16 | 16 | 64 MiB | cold | 10 | 0.3316 s | 0.3584 s | 2 | 2 | 30 | 137.9 KiB | 1 |
| direct-s3 | 80 ms | 16 | 16 | 64 MiB | warm | 10 | 0.0829 s | 0.0930 s | 0 | 0 | 34 | 0.0 B | 1 |
| direct-s3 | 80 ms | 16 | 64 | 0 MiB | cold | 10 | 1.9780 s | 2.4947 s | 82 | 0 | 0 | 260.9 KiB | 1 |
| direct-s3 | 80 ms | 16 | 64 | 0 MiB | warm | 10 | 1.9156 s | 2.2271 s | 82 | 0 | 0 | 260.9 KiB | 1 |
| direct-s3 | 80 ms | 16 | 64 | 64 MiB | cold | 10 | 0.3340 s | 0.4722 s | 2 | 2 | 78 | 334.6 KiB | 1 |
| direct-s3 | 80 ms | 16 | 64 | 64 MiB | warm | 10 | 0.0844 s | 0.0959 s | 0 | 0 | 82 | 0.0 B | 1 |

## RTT sensitivity

The fitted milliseconds-per-millisecond slope estimates serialized network turns. It should be read beside the exact request ledger above.

| Transport | Depth | Subtree files | Cache | Phase | Fitted turns | 0 ms median | Max RTT median |
|---|---:|---:|---:|---|---:|---:|---:|
| direct-s3 | 0 | 1 | 0 MiB | cold | 4.04 | 5.2 ms | 329.1 ms at 80 ms |
| direct-s3 | 0 | 1 | 0 MiB | warm | 4.05 | 3.8 ms | 328.5 ms at 80 ms |
| direct-s3 | 0 | 1 | 64 MiB | cold | 4.05 | 5.7 ms | 329.7 ms at 80 ms |
| direct-s3 | 0 | 1 | 64 MiB | warm | 2.02 | 2.5 ms | 164.3 ms at 80 ms |
| direct-s3 | 0 | 16 | 0 MiB | cold | 4.95 | 19.7 ms | 414.8 ms at 80 ms |
| direct-s3 | 0 | 16 | 0 MiB | warm | 3.98 | 14.9 ms | 333.2 ms at 80 ms |
| direct-s3 | 0 | 16 | 64 MiB | cold | 4.05 | 6.9 ms | 331.4 ms at 80 ms |
| direct-s3 | 0 | 16 | 64 MiB | warm | 1.02 | 1.7 ms | 83.3 ms at 80 ms |
| direct-s3 | 0 | 64 | 0 MiB | cold | 7.75 | 46.7 ms | 663.9 ms at 80 ms |
| direct-s3 | 0 | 64 | 0 MiB | warm | 7.00 | 30.9 ms | 589.3 ms at 80 ms |
| direct-s3 | 0 | 64 | 64 MiB | cold | 4.02 | 11.2 ms | 332.7 ms at 80 ms |
| direct-s3 | 0 | 64 | 64 MiB | warm | 0.99 | 4.4 ms | 83.9 ms at 80 ms |
| direct-s3 | 4 | 1 | 0 MiB | cold | 8.09 | 7.6 ms | 655.8 ms at 80 ms |
| direct-s3 | 4 | 1 | 0 MiB | warm | 8.11 | 6.8 ms | 656.2 ms at 80 ms |
| direct-s3 | 4 | 1 | 64 MiB | cold | 4.08 | 5.1 ms | 331.4 ms at 80 ms |
| direct-s3 | 4 | 1 | 64 MiB | warm | 2.03 | 2.1 ms | 164.6 ms at 80 ms |
| direct-s3 | 4 | 16 | 0 MiB | cold | 9.08 | 21.8 ms | 747.7 ms at 80 ms |
| direct-s3 | 4 | 16 | 0 MiB | warm | 8.03 | 19.8 ms | 663.6 ms at 80 ms |
| direct-s3 | 4 | 16 | 64 MiB | cold | 4.04 | 7.0 ms | 330.7 ms at 80 ms |
| direct-s3 | 4 | 16 | 64 MiB | warm | 1.01 | 2.2 ms | 83.2 ms at 80 ms |
| direct-s3 | 4 | 64 | 0 MiB | cold | 11.93 | 38.3 ms | 991.8 ms at 80 ms |
| direct-s3 | 4 | 64 | 0 MiB | warm | 11.05 | 26.4 ms | 910.4 ms at 80 ms |
| direct-s3 | 4 | 64 | 64 MiB | cold | 4.04 | 9.8 ms | 333.2 ms at 80 ms |
| direct-s3 | 4 | 64 | 64 MiB | warm | 1.00 | 3.8 ms | 84.1 ms at 80 ms |
| direct-s3 | 16 | 1 | 0 MiB | cold | 20.23 | 23.1 ms | 1642.8 ms at 80 ms |
| direct-s3 | 16 | 1 | 0 MiB | warm | 20.25 | 20.1 ms | 1641.6 ms at 80 ms |
| direct-s3 | 16 | 1 | 64 MiB | cold | 4.06 | 6.0 ms | 331.1 ms at 80 ms |
| direct-s3 | 16 | 1 | 64 MiB | warm | 2.02 | 2.9 ms | 165.0 ms at 80 ms |
| direct-s3 | 16 | 16 | 0 MiB | cold | 21.21 | 33.8 ms | 1731.0 ms at 80 ms |
| direct-s3 | 16 | 16 | 0 MiB | warm | 20.22 | 29.7 ms | 1648.8 ms at 80 ms |
| direct-s3 | 16 | 16 | 64 MiB | cold | 4.03 | 8.7 ms | 331.6 ms at 80 ms |
| direct-s3 | 16 | 16 | 64 MiB | warm | 1.01 | 2.2 ms | 82.9 ms at 80 ms |
| direct-s3 | 16 | 64 | 0 MiB | cold | 24.01 | 57.9 ms | 1978.0 ms at 80 ms |
| direct-s3 | 16 | 64 | 0 MiB | warm | 23.44 | 40.7 ms | 1915.6 ms at 80 ms |
| direct-s3 | 16 | 64 | 64 MiB | cold | 4.03 | 11.3 ms | 334.0 ms at 80 ms |
| direct-s3 | 16 | 64 | 64 MiB | warm | 1.00 | 4.2 ms | 84.4 ms at 80 ms |

Depth counts directory edges above the selected directory. Each extra uncached level is causally dependent on decoding its parent. Independent selected-subtree payload reads can overlap only after resolution.

The warm phase reuses only the source handle and its pack cache. It always transfers into a fresh verified destination, so destination deduplication cannot hide source reads.

Configured RTT is applied to the direct S3 link by a rootless relay. Repository import and source open remain outside the timer.
