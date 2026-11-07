//! A safe-only plan's apply, place by place (F8, design note Harness/notes/2026-11-07-lords-f8-delivery.md §4 D3): each
//! place's items are checked and staged, then swapped in one by one, and a failure puts the place's earlier items back,
//! so a place is fully current or untouched.
