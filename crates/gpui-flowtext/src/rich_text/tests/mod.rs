// Shared scaffolding for the include!-spliced suites below. The split files
// are textually part of THIS module, so all imports live here — no `use`
// statements inside the included files.
//
// Loro-first invariant 5: an editor WITHOUT a write authority is read-only, so
// every MUTATING behavior test lives in tests/editor_behavior.rs (integration
// target), where the real flowstate-collab write authority attaches without
// the unit-test dual-crate-instance problem. Suites here are pure layout /
// hit-testing / selection geometry.

use super::*;
use gpui::{Bounds, black, point, px, size};

include!("edit_layout.rs");
include!("selection.rs");
include!("positions_formatting.rs");
include!("decorations_drag_search.rs");
include!("layout_scaling.rs");
include!("prep_equivalence.rs");
