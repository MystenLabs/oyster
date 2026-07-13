//! Self-serve web signup: Turnstile anti-bot verification, Google
//! OAuth, and the `/signup` pages. Enabled only when
//! [`crate::config::SignupConfig`] is present.

pub mod google;
pub mod routes;
pub mod turnstile;
