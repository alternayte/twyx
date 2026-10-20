# Performance

twyx is measured against Zellij and Herdr on the same machine with `just bench`. The harness is the outer terminal: it writes keys into the client's PTY and reads what the client draws. Each number is the multiplexer's own cost plus the PTY round trip.

## What runs

- **Latency:** the harness types `§` into a shell in a pane and times its echo to the screen, 20 times. It reports the median (p50) and the 95th percentile.
- **cat:** `cat` of a 32 MiB file in a pane, until the harness reads the marker after it.
- **Producer peak:** the highest memory of the multiplexer's own processes while `yes` writes at full speed for 10 seconds.
- **Idle:** CPU and memory of the multiplexer's own processes over 10 seconds with 10 idle shell panes.

The client runs at 120 by 40 cells. Memory and CPU count the multiplexer's processes (client, server or daemon, and holders), never the shells in the panes. Each multiplexer starts with an empty config and state.

## Results

macOS 26.5.1, Apple M-series, 8 CPUs, release builds. twyx at commit `bc5a68d`.

| | Latency p50 | Latency p95 | cat | Producer peak | Idle CPU | Idle memory |
|---|---|---|---|---|---|---|
| twyx 0.1.0 | 2.2 ms | 3.7 ms | 22.1 MiB/s | 28.1 MiB | 1.95% | 61.3 MiB |
| Zellij 0.44.0 | 13.3 ms | 14.4 ms | 15.5 MiB/s | 137.0 MiB | 2.44% | 65.8 MiB |
| Herdr 0.9.5 | 1.6 ms | 4.2 ms | 28.0 MiB/s | 52.4 MiB | 0.88% | 42.6 MiB |

Raw output:

```
machine: Darwin 25.5.0 arm64, 8 cpus
twyx (twyx 0.1.0): latency p50 2.2 ms, p95 3.7 ms; cat 22.1 MiB/s; producer peak 28.1 MiB; idle with 10 panes 1.95% cpu, 61.3 MiB
zellij (zellij 0.44.0): latency p50 13.3 ms, p95 14.4 ms; cat 15.5 MiB/s; producer peak 137.0 MiB; idle with 10 panes 2.44% cpu, 65.8 MiB
herdr (herdr 0.9.5): latency p50 1.6 ms, p95 4.2 ms; cat 28.0 MiB/s; producer peak 52.4 MiB; idle with 10 panes 0.88% cpu, 42.6 MiB
```

## Reading the results

- twyx runs one holder process per pane, so a pane survives the client and the daemon. Each holder parses its own output. That costs idle CPU and memory per pane that a single-process multiplexer does not pay.
- Herdr has the lowest latency at the median and the highest throughput. twyx is behind it on both, and ahead of it on peak memory under a fast producer and on p95 latency.
- Zellij is behind both on latency and peak memory in this run.
- These numbers come from one machine. Linux numbers are not published yet.

## Run it

```sh
just bench
```

The recipe builds twyx in release mode and measures each of `zellij` and `herdr` that is on `PATH`.
