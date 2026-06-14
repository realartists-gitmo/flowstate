• I can identify several concrete causes from the code. Visual data would help prioritize them, but is not required to establish that the current implementation is structurally unreliable.

  Primary Issues

  1. Nested drop targets fight each other
      - Every cell and its containing column have on_drag_move and on_drop.
      - Cell drag-move stops propagation, but column behavior still wins whenever the pointer crosses small gaps, controls, or card edges.
      - pending_cell_drop constantly switches between cell-relative and orphan-column destinations.

  2. No drag-leave cleanup
      - pending_cell_drop remains set after leaving a valid target.
      - Dropping later can execute the last stale destination rather than the visible/current target.

  3. Drop zones are too coarse
      - A cell’s rightmost 28% means “child.”
      - Everything else is split at one vertical midpoint.
      - This makes intended top/bottom drops difficult on short cards and child drops easy to trigger accidentally.

  4. Column insertion uses global sheet indexes incorrectly
      - update_column_drop derives an index from cells in one column, then uses the corresponding global sheet index.
      - Because sheet order mixes families and columns, the destination can differ substantially from the pointer’s visible vertical location.

  5. Gap inheritance is inconsistent
      - A gap becomes a sibling drop only when adjacent same-column cells share a parent.
      - Otherwise it becomes an orphan, even when visually inside a family run.

  6. Moving a parent destroys its existing family
      - move_cell unconditionally orphans all direct children.
      - Dragging a parent therefore moves only the parent, causing large unexpected layout changes.

  7. No invalid-drop validation before preview
      - The UI can preview child drops onto descendants, structurally invalid columns, or destinations that effectively do nothing.
      - Failure occurs silently when the model rejects or normalizes it.

  8. No explicit drop indicator geometry
      - Only the target card border changes.
      - Users cannot reliably distinguish before, after, child, or orphan insertion positions.

  9. Every drag movement triggers a full render
      - update_cell_drop always assigns and calls cx.notify(), even when the destination did not change.
      - This contributes to dodgy responsiveness.

  So yes, the implementation itself explains the general unreliability. Visual examples would only help confirm which behavior is most disruptive.
