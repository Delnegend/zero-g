//! Handlers for dashboard user identity and session termination.

/// Current user identity details route.
mod current_user;
/// Session logout and cookie clearing route.
mod logout;

pub(crate) use current_user::__path_current_user;
pub(crate) use current_user::{DashboardUser, current_user};
pub(crate) use logout::__path_logout;
pub(crate) use logout::logout;
