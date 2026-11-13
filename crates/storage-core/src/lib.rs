//! Storage core: scans the home folder, sorts every byte into a category, finds what can be
//! cleared (with extra care for coding agents' worktrees) and clears it safely.

pub mod bulk;
pub mod clean;
pub mod git;
pub mod remove;
pub mod rules;
pub mod scan;
pub mod sys;

pub use rules::{Cat, Tier};
pub use scan::{Entry, Progress, Report, Scanner};
