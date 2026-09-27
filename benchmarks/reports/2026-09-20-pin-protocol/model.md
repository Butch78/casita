# Pin protocol experiment

protocol-model-only; JSON/file CAS, not Casita or S3 measurements

Complete: False

| Design | Processes | Objects | Fault | GET | PUT | Conflicts | Seconds |
|---|---:|---:|---|---:|---:|---:|---:|
| current-shape | 1 | 7 | none | 41 | 18 | 1 | 1.319 |
| batched | 1 | 7 | none | 16 | 11 | 0 | 0.607 |
| owned | 1 | 7 | none | 16 | 12 | 0 | 0.389 |
| current-shape | 1 | 7 | crash | 7 | 4 | 1 | 0.119 |
| batched | 1 | 7 | crash | 9 | 9 | 0 | 0.227 |
| owned | 1 | 7 | crash | 15 | 10 | 0 | 0.709 |
| current-shape | 1 | 7 | ambiguous | 47 | 18 | 1 | 0.705 |
| batched | 1 | 7 | ambiguous | 14 | 12 | 1 | 0.836 |
| owned | 1 | 7 | ambiguous | 15 | 12 | 0 | 1.208 |
| current-shape | 1 | 7 | cancel | 8 | 4 | 0 | 0.166 |
| batched | 1 | 7 | cancel | 14 | 11 | 1 | 0.328 |
| owned | 1 | 7 | cancel | 12 | 11 | 0 | 0.363 |
| current-shape | 1 | 8 | none | 49 | 21 | 2 | 0.821 |
| batched | 1 | 8 | none | 12 | 12 | 0 | 0.386 |
| owned | 1 | 8 | none | 18 | 13 | 0 | 1.238 |
| current-shape | 1 | 8 | crash | 7 | 4 | 1 | 0.443 |
| batched | 1 | 8 | crash | 14 | 11 | 1 | 0.817 |
| owned | 1 | 8 | crash | 14 | 11 | 0 | 0.385 |
| current-shape | 1 | 8 | ambiguous | 51 | 21 | 2 | 1.068 |
| batched | 1 | 8 | ambiguous | 16 | 13 | 1 | 0.436 |
| owned | 1 | 8 | ambiguous | 21 | 13 | 0 | 1.814 |
| current-shape | 1 | 8 | cancel | 7 | 4 | 0 | 0.193 |
| batched | 1 | 8 | cancel | 12 | 12 | 1 | 1.697 |
| owned | 1 | 8 | cancel | 16 | 12 | 0 | 0.603 |
| current-shape | 1 | 9 | none | 53 | 24 | 3 | 1.899 |
| batched | 1 | 9 | none | 17 | 16 | 2 | 0.770 |
| owned | 1 | 9 | none | 17 | 15 | 0 | 0.746 |
| current-shape | 1 | 9 | crash | 4 | 4 | 1 | 0.188 |
| batched | 1 | 9 | crash | 14 | 11 | 1 | 3.980 |
| owned | 1 | 9 | crash | 19 | 12 | 1 | 2.288 |
| current-shape | 1 | 9 | ambiguous | 58 | 21 | 0 | 1.092 |
| batched | 1 | 9 | ambiguous | 132 | 16 | 2 | 2.811 |
| owned | 1 | 9 | ambiguous | 23 | 15 | 0 | 1.533 |
| current-shape | 1 | 9 | cancel | 8 | 4 | 0 | 0.199 |
| batched | 1 | 9 | cancel | 11 | 11 | 0 | 0.383 |
| owned | 1 | 9 | cancel | 15 | 12 | 0 | 0.327 |
| current-shape | 10 | 7 | none | 735 | 573 | 403 | 7.287 |
| batched | 10 | 7 | none | 202 | 190 | 80 | 2.461 |
| owned | 10 | 7 | none | 938 | 155 | 35 | 7.466 |
| current-shape | 10 | 7 | crash | 678 | 513 | 357 | 13.284 |
| batched | 10 | 7 | crash | 201 | 182 | 74 | 1.917 |
| owned | 10 | 7 | crash | 607 | 143 | 25 | 4.170 |
| current-shape | 10 | 7 | ambiguous | 1400 | 623 | 453 | 6.547 |
| batched | 10 | 7 | ambiguous | 217 | 185 | 75 | 2.556 |
| owned | 10 | 7 | ambiguous | 539 | 156 | 36 | 2.748 |
| current-shape | 10 | 7 | cancel | 598 | 472 | 315 | 4.929 |
| batched | 10 | 7 | cancel | 242 | 210 | 101 | 2.298 |
| owned | 10 | 7 | cancel | 803 | 167 | 48 | 3.817 |
| current-shape | 10 | 8 | none | 813 | 646 | 456 | 4.978 |
| batched | 10 | 8 | none | 285 | 210 | 90 | 1.192 |
| owned | 10 | 8 | none | 524 | 172 | 42 | 1.470 |
| current-shape | 10 | 8 | crash | 701 | 571 | 397 | 5.197 |
