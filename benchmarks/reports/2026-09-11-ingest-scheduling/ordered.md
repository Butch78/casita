# Ingest concurrency

Complete: True

fresh durable CLI import; source generation, init, fsck and checkout excluded; warm OS cache

Release baseline at 898257b; warm source files, fresh repositories, shared development host. File concurrency sweep with chunk concurrency fixed at 32.

| Corpus | Files | Chunks/writer | Repetition | Seconds | Peak RSS bytes |
|---|---:|---:|---:|---:|---:|
| above-chunker-minimum | 1 | 32 | 0 | 5.5778 | 43528192 |
| above-chunker-minimum | 16 | 32 | 0 | 0.9790 | 82300928 |
| mixed | 1 | 32 | 0 | 17.2757 | 132251648 |
| below-chunker-minimum | 1 | 32 | 1 | 5.9297 | 44421120 |
| above-chunker-minimum | 16 | 32 | 2 | 1.3409 | 86179840 |
| below-chunker-minimum | 1 | 32 | 0 | 4.7455 | 44818432 |
| large | 1 | 32 | 1 | 17.8993 | 268771328 |
| large | 16 | 32 | 2 | 6.8752 | 328192000 |
| below-chunker-minimum | 16 | 32 | 0 | 0.9964 | 61628416 |
| tiny | 16 | 32 | 0 | 22.7215 | 108290048 |
| large | 32 | 32 | 0 | 6.1629 | 334487552 |
| tiny | 16 | 32 | 1 | 21.5433 | 100847616 |
| mixed | 32 | 32 | 2 | 4.2811 | 180256768 |
| above-chunker-minimum | 32 | 32 | 0 | 0.6075 | 92917760 |
| below-chunker-minimum | 1 | 32 | 2 | 3.9206 | 44032000 |
| large | 16 | 32 | 1 | 6.0882 | 343687168 |
| above-chunker-minimum | 32 | 32 | 2 | 0.7312 | 101945344 |
| mixed | 1 | 32 | 2 | 15.9131 | 131870720 |
| mixed | 16 | 32 | 0 | 6.6586 | 136306688 |
| large | 1 | 32 | 2 | 7.3849 | 245792768 |
| below-chunker-minimum | 16 | 32 | 1 | 0.6959 | 61755392 |
| above-chunker-minimum | 32 | 32 | 1 | 0.5915 | 94191616 |
| above-chunker-minimum | 1 | 32 | 2 | 3.8897 | 42471424 |
| above-chunker-minimum | 16 | 32 | 1 | 0.6632 | 69816320 |
| large | 16 | 32 | 0 | 4.7260 | 325459968 |
| mixed | 16 | 32 | 2 | 4.3488 | 153124864 |
| tiny | 1 | 32 | 0 | 60.3981 | 62230528 |
| above-chunker-minimum | 1 | 32 | 1 | 2.2822 | 42598400 |
| below-chunker-minimum | 32 | 32 | 1 | 0.4135 | 75960320 |
| mixed | 32 | 32 | 1 | 3.2660 | 176005120 |
| mixed | 1 | 32 | 1 | 10.0855 | 130949120 |
| large | 32 | 32 | 2 | 5.1734 | 320823296 |
| mixed | 32 | 32 | 0 | 3.1070 | 175116288 |
| tiny | 1 | 32 | 2 | 64.6194 | 75243520 |
| large | 1 | 32 | 0 | 5.7438 | 265179136 |
| tiny | 16 | 32 | 2 | 15.0919 | 107401216 |
| large | 32 | 32 | 1 | 5.0114 | 309923840 |
| tiny | 32 | 32 | 0 | 11.9342 | 116367360 |
| tiny | 32 | 32 | 2 | 11.7248 | 114470912 |
| below-chunker-minimum | 16 | 32 | 2 | 0.4809 | 62451712 |
| below-chunker-minimum | 32 | 32 | 0 | 0.4059 | 77463552 |
| below-chunker-minimum | 32 | 32 | 2 | 0.7898 | 74878976 |
| tiny | 1 | 32 | 1 | 66.3954 | 64581632 |
| tiny | 32 | 32 | 1 | 11.9631 | 118198272 |
| mixed | 16 | 32 | 1 | 4.3540 | 150614016 |
