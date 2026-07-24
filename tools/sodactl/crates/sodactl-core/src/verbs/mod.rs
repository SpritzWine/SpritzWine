//! The mutating verbs, one module each. Every verb is a pure PLANNING function
//! producing `plan::Plan`s (reviewable, refusable, testable without a repo on
//! disk being touched) plus, where the verb needs one, a thin apply wrapper
//! around the shared `plan::apply` engine.
//!
//! The division of labour, so the next verb lands in the right place:
//!
//! * `plan.rs` owns everything shared -- preflight, the journal, backup-ref
//!   creation, pre-verification, the execution engine, the error taxonomy.
//! * A verb module owns its DERIVATION (what should happen, computed from live
//!   state) and any verb-specific POSTCONDITION (restack's patch-id equality).
//! * The CLI owns rendering, confirmation and exit codes. Nothing here prints.
//!
//! Verbs never consult `check`: the gate lives on push, and a verb refuses
//! only on its OWN preconditions (in-flight replay, dirty tree, CAS mismatch,
//! failed postcondition). A verb that second-guessed the whole report would
//! block a repair exactly when the report is red -- which is when repairs run.

pub mod buildwrap;
pub mod pin;
pub mod pushprint;
pub mod regen;
pub mod release;
pub mod restack;
pub mod resume;
pub mod rrgc;
pub mod stitch;
pub mod tagverb;
pub mod undo;
