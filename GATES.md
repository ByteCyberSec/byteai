# Gates: parallel-check task

OWNS: GATES.md

Scope: Run the two requested checks and prove they pass.

- [x] G1: echo parallel-check-ok outputs exactly "parallel-check-ok"
  CHECK: echo parallel-check-ok
  EXPECT: parallel-check-ok
  EVIDENCE: exit 0 · parallel-check-ok
- [x] G2: ls /tmp | head -2 outputs a non-empty list of /tmp entries
  CHECK: ls /tmp | head -2
  EXPECT: Jarvis.icns
  EVIDENCE: exit 0 · Jarvis.icns
