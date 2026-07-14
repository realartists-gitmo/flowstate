# Flow drag-and-drop ergonomics test (run straight through)

A fixed battery of drag targets, paired with drag telemetry, to turn "a lot of moves I couldn't get
across" into specific, located problems.

## 1. Setup

Launch with logging on and the fixture open:

```fish
rm -f ~/flow-drag-log.jsonl; env FLOWSTATE_DRAG_LOG=$HOME/flow-drag-log.jsonl cargo run --release -p flowstate --manifest-path /home/adam/Projects/flowstate-fixflow-collab/Cargo.toml -- /home/adam/Projects/flowstate-fixflow-collab/flow-ergonomics-fixture.fl0
```

Each **drag** appends one JSON line to `~/flow-drag-log.jsonl`. Send me that file when done.

## 2. The reset protocol (this is what lets you run straight through)

Every step below starts from the **fresh** layout in the map. To get back there after each step:

> **After each step, press the ribbon's `Undo` button once per move you made this step**, until the
> board matches the map again. (Do NOT save.)

- A move that "didn't take" (no change) has nothing to undo — you're already fresh.
- If you attempted a step several times, Undo that many times.
- If the board ever looks wrong or Undo won't go further, **hard-reset**: close the tab and reopen
  `flow-ergonomics-fixture.fl0` (don't save first). Telemetry keeps logging across reopens.

Undo is *not* part of the test — only your drags are logged, so undoing between steps costs us nothing.

## 3. Board map — the "Ergonomics" sheet (every step starts here)

Columns left → right: **1AC · 1NC · 2AC · Block · 1AR · 2NR · 2AR**

```
1AC       1NC         2AC         Block    1AR    2NR
A1 ──┬──  B1 ───────  C1 ───────  D1 ────  E1 ──  F1
     └──  B2
A2 ─────  B3 ──┬───   C2
               └───   C3
A3 ─────  B5
          B4 (orphan, no parent)
                      C4 (orphan, no parent)
```

- Chain: `A1→B1→C1→D1→E1→F1`. Also `A2→B3→{C2,C3}` and `A3→B5`.
- `B4` (in 1NC) and `C4` (in 2AC) are parentless orphans.
- Heights vary on purpose. There's a throwaway second sheet **Scratch** if you want extra room.

For each step: **Move** = what to do · **Expect** = where it lands (from the fresh map) · **Watch** =
the feel to judge. Then **↩ reset** and go to the next.

## 4. The battery

1. **Reorder siblings.** Move **B2** above **B1**---feilt weird
   *Expect:* under A1, order becomes B2, B1 (both still 1NC responses to A1). *Watch:* landing "just
   above B1" precisely. **↩ Undo once.**

2. **Reorder roots.** Move **A3** to the very top of 1AC (above A1).
   *Expect:* 1AC reads A3, A1, A2. *Watch:* dropping at the very top of a column. **↩ Undo once.**

3. **Demote a root to a child (rightward).** Drop **A2** onto **A1** as its last response.---EXTREMELY DIFFICULT AND NARROW
   *Expect:* A2 becomes A1's last 1NC child; its subtree B3→2AC, C2/C3→Block. *Watch:* the right-edge
   "make child" zone — findable? triggers when you mean it? **↩ Undo once.**

4. **First-child insert.** Make **A3** the **first** response of **A1**.---did something weird, moved entire childset down a lot, no natural gap between it and second child
   *Expect:* A3 is A1's first 1NC child, above B1/B2. *Watch:* first vs last child. **↩ Undo once.**

5. **Promote a child to a root (leftward).** Move **C1** into 1AC, dropped between A1 and A2.
   *Expect:* C1 root in 1AC between A1 and A2; D1→1NC, E1→2AC, F1→Block (chain preserved). *Watch:* can
   you place it at the vertical spot you aim at? **↩ Undo once.**

6. **Reparent a subtree.** Move **B3** (carrying C2/C3) to be a response to **A1**.
   *Expect:* B3 becomes A1's child; C2/C3 stay B3's children in 2AC. *Watch:* subtree stays intact and
   follows the pointer. **↩ Undo once.**

7. **Insert into the middle of a sibling run.** Move **C4** (orphan) to sit **between C2 and C3** under
   B3.
   *Expect:* 2AC under B3 reads C2, C4, C3 (C4 now B3's child). *Watch:* precise mid-run insertion.
   **↩ Undo once.**

8. **Orphan → response.** Move **B4** (orphan) to be a response to **A1**, above B2.
   *Expect:* B4 becomes a 1NC child of A1, above B2 (it should *inherit* A1 as parent). *Watch:* does
   dropping next to a parented cell adopt that parent? **↩ Undo once.**

9. **Reach the far-right column.** Move **F1** to be a standalone root in the last column, **2AR**.
   *Expect:* F1 is a parentless root in 2AR. *Watch:* can you target/reach the rightmost column?
   **↩ Undo once.**

10. **Standalone at top of a column.** Move **C3** to the very top of 2AC as a standalone (no parent).---weird, same drop and no gap effect as earlier
    *Expect:* C3 is parentless at the top of 2AC. *Watch:* standalone drop vs auto-adopting a parent.
    **↩ Undo once.**

11. **First-child of a subtree, multi-level shift.** Make **C1** (with its chain) the **first**---same weirdness
    response of **A2**.
    *Expect:* C1→1NC (first child of A2), D1→2AC, E1→Block, F1→1AR. *Watch:* the whole chain shifting
    columns together. **↩ Undo once.**

12. **Invalid move (should refuse).** Try to drop **A1** onto its own descendant (**B1** or **C1**) as
    a child.
    *Expect:* **nothing** — no landing preview, no move. *Watch:* does it feel clearly "not allowed"?
    **↩ Nothing to undo** (it shouldn't have moved).

13. **Long move + auto-scroll.** Scroll so the top-left and bottom-right aren't both visible, then drag
    **F1** all the way to above **A1**.
    *Expect:* F1 becomes a root in 1AC above A1. *Watch:* does edge auto-scroll carry you there?
    **↩ Undo once** (scroll back up first if needed).

14. **Free-for-all.** From fresh, do 3–5 moves that *should* be easy — especially any that beat you
    last time. Attempt each; the log records every attempt and where the pointer actually was. Undo
    between them if you like.---woah B3 child of B2 caused C1 child of B1 to go BELOW B3? That's not the hierarchy at all...

## 5. Sending results

Send me `~/flow-drag-log.jsonl`. A one-line note per step number where the feel was off ("#5 still
snaps", "#9 couldn't reach 2AR") plus the offsets makes the diagnosis fast. Anything you'd rather
show than describe, type into the relevant cell and save a copy — I can read notes straight out of the
`.fl0`.
