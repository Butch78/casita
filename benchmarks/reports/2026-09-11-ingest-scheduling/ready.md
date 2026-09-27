# Ingest concurrency

Complete: True

fresh durable CLI import; source generation, init, fsck and checkout excluded; warm OS cache

Release candidate using ready-first admission and indexed page slots; warm source files, fresh repositories, shared development host. No overlapping local builds launched by this investigation.

| Corpus | Files | Chunks/writer | Repetition | Seconds | Peak RSS bytes |
|---|---:|---:|---:|---:|---:|
| above-chunker-minimum | 1 | 32 | 0 | 5.8597 | 49803264 |
| above-chunker-minimum | 16 | 32 | 0 | 0.7710 | 84877312 |
| mixed | 1 | 32 | 0 | 16.7922 | 133664768 |
| below-chunker-minimum | 1 | 32 | 1 | 4.0018 | 45953024 |
| above-chunker-minimum | 16 | 32 | 2 | 0.8974 | 82317312 |
| below-chunker-minimum | 1 | 32 | 0 | 4.2630 | 45875200 |
| large | 1 | 32 | 1 | 8.9078 | 251703296 |
| large | 16 | 32 | 2 | 6.6052 | 324898816 |
| below-chunker-minimum | 16 | 32 | 0 | 0.7767 | 63987712 |
| tiny | 16 | 32 | 0 | 22.6582 | 114778112 |
| large | 32 | 32 | 0 | 6.1667 | 315621376 |
| tiny | 16 | 32 | 1 | 21.3264 | 120184832 |
| mixed | 32 | 32 | 2 | 3.2619 | 194854912 |
| above-chunker-minimum | 32 | 32 | 0 | 0.4565 | 103268352 |
| below-chunker-minimum | 1 | 32 | 2 | 2.1228 | 47984640 |
| large | 16 | 32 | 1 | 4.8709 | 345255936 |
| above-chunker-minimum | 32 | 32 | 2 | 0.4667 | 100347904 |
| mixed | 1 | 32 | 2 | 8.7259 | 132976640 |
| mixed | 16 | 32 | 0 | 3.2217 | 142209024 |
| large | 1 | 32 | 2 | 5.6429 | 258347008 |
| below-chunker-minimum | 16 | 32 | 1 | 0.5734 | 66162688 |
| above-chunker-minimum | 32 | 32 | 1 | 0.3845 | 101548032 |
| above-chunker-minimum | 1 | 32 | 2 | 2.2066 | 46514176 |
| above-chunker-minimum | 16 | 32 | 1 | 0.5355 | 77156352 |
| large | 16 | 32 | 0 | 4.7320 | 323715072 |
| mixed | 16 | 32 | 2 | 2.9739 | 138530816 |
| tiny | 1 | 32 | 0 | 70.4524 | 73293824 |
| above-chunker-minimum | 1 | 32 | 1 | 2.3724 | 45260800 |
| below-chunker-minimum | 32 | 32 | 1 | 0.4711 | 79757312 |
| mixed | 32 | 32 | 1 | 2.5831 | 187797504 |
| mixed | 1 | 32 | 1 | 9.2634 | 134107136 |
| large | 32 | 32 | 2 | 5.1662 | 320221184 |
| mixed | 32 | 32 | 0 | 2.8314 | 196890624 |
| tiny | 1 | 32 | 2 | 61.7501 | 77234176 |
| large | 1 | 32 | 0 | 5.5739 | 229040128 |
| tiny | 16 | 32 | 2 | 15.7088 | 121896960 |
| large | 32 | 32 | 1 | 4.5972 | 330362880 |
| tiny | 32 | 32 | 0 | 12.5583 | 110714880 |
| tiny | 32 | 32 | 2 | 12.2998 | 113692672 |
| below-chunker-minimum | 16 | 32 | 2 | 0.4467 | 63963136 |
| below-chunker-minimum | 32 | 32 | 0 | 0.3791 | 79278080 |
| below-chunker-minimum | 32 | 32 | 2 | 0.3960 | 87801856 |
| tiny | 1 | 32 | 1 | 64.3310 | 64765952 |
| tiny | 32 | 32 | 1 | 11.9434 | 111194112 |
| mixed | 16 | 32 | 1 | 2.5571 | 172118016 |
