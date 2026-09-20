---
name: Bug report
about: Create a report to help improve dry-exec kernel primitives or SDK
title: "[BUG]: "
labels: bug
assignees: ''

---

**Describe the Bug**
A clear and concise description of what the unexpected execution failure was.

**Execution Environment**
 - Host OS: [e.g. Ubuntu 22.04, Debian 12]
 - Linux Kernel Version: [e.g. 6.5.0-generic]
 - Python Version: [e.g. 3.11.4]
 - `dry-exec` Version: [e.g. 0.1.0]

**Control Flow & Reproduction**
Steps to reproduce the behavior:
1. Define Environment with boundaries: `...`
2. Dispatch Action: `...`
3. Execute via `DryExecClient`: `...`
4. See unexpected exception or state delta discrepancy.

**Expected Behavior**
A clear and concise description of what state delta or execution boundary behavior was expected.

**Traceback / Diagnostic Logs**
```text
Paste logs from dry-exec inspect or DeltaLogger receipts here.
```

**Additional Context**
Add any other context about the syscall interception or namespace configuration here.
