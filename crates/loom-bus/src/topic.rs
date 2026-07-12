// Copyright 2026 Loom Contributors
// SPDX-License-Identifier: Apache-2.0

//! [`Topic`] — a bus routing key.
//!
//! A topic is a `.`-delimited subject, mirroring a `NATS` subject
//! (control-plane §1). Subscriptions match **hierarchically by prefix**: a
//! subscription to the family `job` receives every concrete subject beneath it
//! (`job.scheduled`, `job.dispatched`, `job.succeeded`), so a consumer subscribes
//! once to "job lifecycle" and sees every fine-grained state-change nudge the
//! scheduler and API write to the `outbox`. There are no wildcards — matching is
//! exact-or-descendant, which keeps routing unambiguous.

use core::fmt;

/// A bus routing key: a `.`-delimited subject such as `job.scheduled` or
/// `agent.heartbeat`.
///
/// Used two ways: as the *subject* a message is published under, and as the
/// *family* a subscriber listens to. A subscription topic [`covers`](Topic::covers)
/// a published subject when they are equal or the subject is a dot-delimited
/// descendant of the subscription.
#[derive(Clone, Debug, PartialEq, Eq, Hash, PartialOrd, Ord)]
pub struct Topic(String);

impl Topic {
    /// The job-lifecycle family (`job`): covers `job.scheduled`, `job.dispatched`,
    /// `job.running`, `job.succeeded`, … — every lifecycle nudge the scheduler and
    /// API write to the `outbox` (control-plane §3).
    #[must_use]
    pub fn job_lifecycle() -> Self {
        Self("job".to_owned())
    }

    /// The agent-events family (`agent`): covers `agent.enrolled`,
    /// `agent.heartbeat`, `agent.attempt_state`, … routed off the agent-gateway
    /// onto the bus (control-plane §1).
    #[must_use]
    pub fn agent_events() -> Self {
        Self("agent".to_owned())
    }

    /// Constructs a topic from an arbitrary subject string. The relay uses this to
    /// carry an `outbox` row's stored `topic` onto the bus verbatim.
    #[must_use]
    pub fn new(subject: impl Into<String>) -> Self {
        Self(subject.into())
    }

    /// The subject as a string slice.
    #[must_use]
    pub fn as_str(&self) -> &str {
        &self.0
    }

    /// Whether this (subscription) topic covers `subject`.
    ///
    /// True when the subjects are equal, or when `subject` is a dot-delimited
    /// descendant of `self`: `job` covers `job` and `job.scheduled` but not
    /// `jobs` (the boundary is a literal `.`, never a bare prefix).
    #[must_use]
    pub fn covers(&self, subject: &Topic) -> bool {
        let parent = self.0.as_str();
        let child = subject.0.as_str();
        child == parent
            || child
                .strip_prefix(parent)
                .is_some_and(|rest| rest.starts_with('.'))
    }
}

impl fmt::Display for Topic {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(&self.0)
    }
}

impl From<&str> for Topic {
    fn from(subject: &str) -> Self {
        Self::new(subject)
    }
}

impl From<String> for Topic {
    fn from(subject: String) -> Self {
        Self::new(subject)
    }
}

#[cfg(test)]
#[allow(clippy::wildcard_imports)]
mod tests {
    use super::*;

    #[test]
    fn family_covers_its_descendants() {
        let job = Topic::job_lifecycle();
        assert!(job.covers(&Topic::new("job")));
        assert!(job.covers(&Topic::new("job.scheduled")));
        assert!(job.covers(&Topic::new("job.attempt.dispatched")));
    }

    #[test]
    fn family_matches_only_on_a_dot_boundary() {
        let job = Topic::job_lifecycle();
        // A bare prefix is not a match — `job` must not swallow `jobs`.
        assert!(!job.covers(&Topic::new("jobs")));
        assert!(!job.covers(&Topic::new("jobstore.x")));
        assert!(!job.covers(&Topic::new("agent.heartbeat")));
    }

    #[test]
    fn a_specific_subject_covers_only_itself_and_below() {
        let scheduled = Topic::new("job.scheduled");
        assert!(scheduled.covers(&Topic::new("job.scheduled")));
        assert!(!scheduled.covers(&Topic::new("job.dispatched")));
        // The family is broader than a specific subject, never the reverse.
        assert!(!scheduled.covers(&Topic::job_lifecycle()));
    }

    #[test]
    fn agent_family_is_independent_of_job_family() {
        let agent = Topic::agent_events();
        assert!(agent.covers(&Topic::new("agent.heartbeat")));
        assert!(!agent.covers(&Topic::new("job.scheduled")));
    }

    #[test]
    fn display_and_as_str_round_trip() {
        let t = Topic::new("job.scheduled");
        assert_eq!(t.as_str(), "job.scheduled");
        assert_eq!(t.to_string(), "job.scheduled");
        assert_eq!(Topic::from("job.scheduled"), t);
    }
}
