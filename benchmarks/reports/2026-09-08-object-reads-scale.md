# Local reader protection

Complete: True

Object and snapshot modes have different retention contracts. Both verify bytes and seek after independent GC/vacuum.
Admission includes metadata snapshot/lookup and durable pin registration. Resolve builds the physical plan (or eagerly reads a bare chunk).
Open contains admission and resolve; do not sum these overlapping timings. Handoff flush drains the temporary snapshot lease.
Read measures stream consumption after GC with pack cache disabled; OS caches are not flushed. Release includes flush.
Fixture creation, root removal, audits and vacuum are excluded from phase timings; process RSS includes them.

| Bytes | Garbage objects | Mode | Phase | Repetition | ms |
|---:|---:|---|---|---:|---:|
| 64 | 64 | object | open | 3 | 11.722 |
| 64 | 64 | object | admission | 3 | 11.170 |
| 64 | 64 | object | resolve | 3 | 0.484 |
| 64 | 64 | object | handoff | 3 | 4.316 |
| 64 | 64 | object | read | 3 | 0.005 |
| 64 | 64 | object | release | 3 | 3.213 |
| 64 | 64 | object | gc | 3 | 614.840 |
| 64 | 64 | snapshot | open | 3 | 6.250 |
| 64 | 64 | snapshot | admission | 3 | 5.815 |
| 64 | 64 | snapshot | resolve | 3 | 0.432 |
| 64 | 64 | snapshot | handoff | 3 | 0.008 |
| 64 | 64 | snapshot | read | 3 | 0.005 |
| 64 | 64 | snapshot | release | 3 | 4.070 |
| 64 | 64 | snapshot | gc | 3 | 16.851 |
| 64 | 64 | object | open | 1 | 8.904 |
| 64 | 64 | object | admission | 1 | 8.338 |
| 64 | 64 | object | resolve | 1 | 0.500 |
| 64 | 64 | object | handoff | 1 | 3.552 |
| 64 | 64 | object | read | 1 | 0.005 |
| 64 | 64 | object | release | 1 | 3.432 |
| 64 | 64 | object | gc | 1 | 646.252 |
| 1048593 | 64 | snapshot | open | 1 | 15.194 |
| 1048593 | 64 | snapshot | admission | 1 | 14.859 |
| 1048593 | 64 | snapshot | resolve | 1 | 0.331 |
| 1048593 | 64 | snapshot | handoff | 1 | 0.008 |
| 1048593 | 64 | snapshot | read | 1 | 3.292 |
| 1048593 | 64 | snapshot | release | 1 | 15.336 |
| 1048593 | 64 | snapshot | gc | 1 | 38.454 |
| 64 | 64 | snapshot | open | 2 | 26.353 |
| 64 | 64 | snapshot | admission | 2 | 25.864 |
| 64 | 64 | snapshot | resolve | 2 | 0.484 |
| 64 | 64 | snapshot | handoff | 2 | 0.011 |
| 64 | 64 | snapshot | read | 2 | 0.005 |
| 64 | 64 | snapshot | release | 2 | 25.901 |
| 64 | 64 | snapshot | gc | 2 | 60.523 |
| 1048593 | 64 | object | open | 3 | 80.897 |
| 1048593 | 64 | object | admission | 3 | 59.424 |
| 1048593 | 64 | object | resolve | 3 | 21.393 |
| 1048593 | 64 | object | handoff | 3 | 16.448 |
| 1048593 | 64 | object | read | 3 | 2.604 |
| 1048593 | 64 | object | release | 3 | 18.344 |
| 1048593 | 64 | object | gc | 3 | 3133.918 |
| 64 | 64 | snapshot | open | 1 | 14.934 |
| 64 | 64 | snapshot | admission | 1 | 14.616 |
| 64 | 64 | snapshot | resolve | 1 | 0.313 |
| 64 | 64 | snapshot | handoff | 1 | 0.008 |
| 64 | 64 | snapshot | read | 1 | 0.006 |
| 64 | 64 | snapshot | release | 1 | 16.946 |
| 64 | 64 | snapshot | gc | 1 | 31.630 |
| 1048593 | 64 | snapshot | open | 3 | 20.276 |
| 1048593 | 64 | snapshot | admission | 3 | 19.938 |
| 1048593 | 64 | snapshot | resolve | 3 | 0.335 |
| 1048593 | 64 | snapshot | handoff | 3 | 0.008 |
| 1048593 | 64 | snapshot | read | 3 | 3.131 |
| 1048593 | 64 | snapshot | release | 3 | 15.227 |
| 1048593 | 64 | snapshot | gc | 3 | 38.688 |
| 1048593 | 64 | object | open | 1 | 45.179 |
| 1048593 | 64 | object | admission | 1 | 30.865 |
| 1048593 | 64 | object | resolve | 1 | 14.245 |
| 1048593 | 64 | object | handoff | 1 | 13.785 |
| 1048593 | 64 | object | read | 1 | 3.140 |
| 1048593 | 64 | object | release | 1 | 29.939 |
| 1048593 | 64 | object | gc | 1 | 4484.985 |
| 64 | 64 | object | open | 2 | 33.707 |
| 64 | 64 | object | admission | 2 | 33.126 |
| 64 | 64 | object | resolve | 2 | 0.511 |
| 64 | 64 | object | handoff | 2 | 13.789 |
| 64 | 64 | object | read | 2 | 0.005 |
| 64 | 64 | object | release | 2 | 15.751 |
| 64 | 64 | object | gc | 2 | 3366.919 |
| 1048593 | 64 | snapshot | open | 2 | 15.140 |
| 1048593 | 64 | snapshot | admission | 2 | 14.842 |
| 1048593 | 64 | snapshot | resolve | 2 | 0.294 |
| 1048593 | 64 | snapshot | handoff | 2 | 0.008 |
| 1048593 | 64 | snapshot | read | 2 | 11.125 |
| 1048593 | 64 | snapshot | release | 2 | 21.983 |
| 1048593 | 64 | snapshot | gc | 2 | 31.942 |
| 1048593 | 64 | object | open | 2 | 46.255 |
| 1048593 | 64 | object | admission | 2 | 28.333 |
| 1048593 | 64 | object | resolve | 2 | 17.811 |
| 1048593 | 64 | object | handoff | 2 | 16.389 |
| 1048593 | 64 | object | read | 2 | 2.922 |
| 1048593 | 64 | object | release | 2 | 14.721 |
| 1048593 | 64 | object | gc | 2 | 3048.401 |
