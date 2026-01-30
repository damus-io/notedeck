//! UI components for the git app.
//!
//! This module contains all UI rendering logic for the git viewer.

mod app;
pub mod colors;
mod issue_list;
mod patch_list;
mod pr_list;
mod repo_detail;
mod repo_list;

pub use app::{GitAction, GitApp, GitResponse, GitRoute};
