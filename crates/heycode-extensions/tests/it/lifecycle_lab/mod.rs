//! Q06 plugin lifecycle/HMR lab.
//!
//! K09 makes one activation a verified transaction and K10 makes a reload a
//! generation swap. Both contracts are stated over *arbitrary* register /
//! dispose / reload orders, and hand-written scenarios only ever pin the orders
//! someone thought of. This lab generates the orders instead: every property
//! below runs a seeded pseudo-random operation sequence against an independent
//! model of what should be live, and shrinks any disagreement down to the
//! shortest sequence that still disagrees.
//!
//! The lab lives in `heycode-extensions` because this crate is the plugin
//! package boundary — it owns install, enable, disable, update, rollback and
//! remove (PL06) and therefore owns the question "does a plugin actually leave
//! when it is told to?". Nothing here is exported: the machinery has no
//! consumer outside these tests.

mod lab;
mod package;
mod registration;
mod reload;
mod seam;
