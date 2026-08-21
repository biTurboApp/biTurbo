# Drain Log

## Run 2026-08-21T14:58Z

- Close-out: pr
- Workspace: branch `drain/ux` (from origin/main; local main behind 14)
- Selection (116 open, label ux): 121 120 119 118 117 116 115 114 113 112 111 110 109 108 107 106 105 104 103 102 101 100 99 98 97 96 95 94 93 92 91 90 89 88 87 86 85 84 83 82 81 80 79 78 77 76 75 74 73 72 71 70 69 68 67 66 65 64 63 62 61 60 59 58 57 56 55 54 53 52 51 50 49 48 47 46 45 44 43 42 41 40 39 38 37 36 35 34 33 32 31 30 29 28 27 26 25 24 23 22 21 20 19 18 17 16 15 14 13 12 11 10 9 8 7 6
- Batch (P1→P3, default budget 5): #60, #28, #9, #8, #6
- Outcomes:
  - #60 fixed (d1db1f4) — prompt() → modal; IPC args + success/error paths verified on running frontend
  - #28 fixed (5dc1b77) — search catch + alert/Retry; fail→banner, recover→retry→results verified
  - #9  fixed (7d121af) — danger confirm focuses Cancel; verified via activeElement on open dialog
  - #8  fixed (986caeb) — QuickAdd dialog semantics/trap/restore; all behaviors verified
  - #6  fixed (7e0e7d5) — global :focus-visible outline; keyboard vs mouse verified via computed styles
- PR: https://github.com/biTurboApp/biTurbo/pull/549 (merge closes all five)
- Notes: upstream lockfile was frozen-lockfile-broken; synced in d1db1f4. Temp ?uiproof verification shim removed before commit.

DRAIN: selected=116 attempted=5 fixed=5 blocked=0 deferred=0 close-out=pr

## Run 2026-08-21T15:37Z

- Close-out: pr
- Workspace: branch `drain/ux-2` (from origin/main @ a2829b0; PR #549 still open, its 5 issues excluded)
- Selection (111 open excl. #6 #8 #9 #28 #60 pending PR #549): 121 120 119 118 117 116 115 114 113 112 111 110 109 108 107 106 105 104 103 102 101 100 99 98 97 96 95 94 93 92 91 90 89 88 87 86 85 84 83 82 81 80 79 78 77 76 75 74 73 72 71 70 69 68 67 66 65 64 63 62 61 60 59 58 57 56 55 54 53 52 51 50 49 48 47 46 45 44 43 42 41 40 39 38 37 36 35 34 33 32 31 30 29 28 27 26 25 24 23 22 21 20 19 18 17 16 15 14 13 12 11 10 9 8 7 6
- Batch (P1→P3, budget 5): #11, #61, #49, #33, #76
- Outcomes:
  - #11 fixed (8ae48d4) — bootstrap error screen + Retry; fail→screen, recover→retry→boot verified
  - #61 fixed (1e8d20f) — delete-active fallback to default; verified via real delete flow
  - #49 fixed (a5c50f0) — related click hydrates off-page memory; uid-9 detail verified
  - #33 fixed (fa7572a) — cards keyboard-operable; focus/Enter/Shift+F10 verified
  - #76 fixed (5969975) — honest graph empty state; copy + navigation verified
- PR: https://github.com/biTurboApp/biTurbo/pull/550 (merge closes all five)
- Notes: temp ?uiproof shim removed before commit.

DRAIN: selected=111 attempted=5 fixed=5 blocked=0 deferred=0 close-out=pr
