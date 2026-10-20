---
"cuenv": patch
---

Make `dependsOn` evaluation linear in dependency-chain depth

`dependsOn` entries on tasks, task groups, container images and services are no
longer typed as `#TaskNode` unions. Typing them made CUE re-evaluate every
referenced task, including its own `dependsOn`, at each reference, so a chain
only two edges deep could exceed the 60-second evaluation timeout. A
twenty-edge chain now evaluates in about a second. Strings and output
references (`build.stdout`, `images.base.ref`) are still rejected with a
`dependsOn[N]` error; every example project exports identical JSON.
