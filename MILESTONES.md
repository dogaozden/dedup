# Milestones

## M1 — walk parity — CLAIMED

Implementation provides repository scaffolding, the CLI skeleton for the M1
scan surface, portable bulk walking through directory traversal, size grouping,
hardlink collapse, extension/name filters, and the JSON conformance report.

- Commit: to be filled after this claim commit is created.
- Gate command: `python3 ~/ox/dedup-oracle/conformance/run.py --gate m1 --candidate 'target/release/dedup-rs {root} --report-json {json} {flags}'`

Final gate output:

```text
M1 gate
  mixed                    pass
  mixed-minsize            pass
  mixed-ext                pass
  mixed-skipext            pass
  mixed-exclude            pass
  smallzoo                 pass
  smallzoo-j1              pass
  prefix                   pass
  bighead                  pass
  bighead-j8                pass
  links                    pass
  adversarial              pass
  deletion                 pass

13 check(s), 0 failed
gate m1: green
```
