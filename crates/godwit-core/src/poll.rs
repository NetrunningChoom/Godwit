// SPDX-License-Identifier: AGPL-3.0-only
// Copyright (C) 2026 Godwit contributors
//! Creating a poll: what a room's network takes, and the check that runs
//! before anything is sent (the dialog and the dispatcher share it).

use serde::{Deserialize, Serialize};

/// What a network takes in a new poll; a room's `Capabilities::polls` holds
/// it when polls can be created there.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash, Serialize, Deserialize)]
pub struct PollLimits {
    /// Longest question, in characters.
    pub question: usize,
    /// Longest answer, in characters.
    pub answer: usize,
    /// Fewest and most answers.
    pub min_answers: usize,
    pub max_answers: usize,
}

/// How long a poll can run, in hours, as the official Discord client offers
/// (1 hour, 4 hours, 8 hours, 24 hours, 3 days, 1 week), with the label shown.
pub const POLL_DURATIONS: [(u32, &str); 6] = [
    (1, "1 hour"),
    (4, "4 hours"),
    (8, "8 hours"),
    (24, "24 hours"),
    (72, "3 days"),
    (168, "1 week"),
];

/// The duration chosen first (24 hours).
pub const POLL_DEFAULT_DURATION: usize = 3;

impl PollLimits {
    /// Checks a draft; the question and answers come back trimmed, with
    /// blank answers dropped. The error is a sentence for the person.
    pub fn check(&self, question: &str, answers: &[String]) -> Result<(String, Vec<String>), String> {
        let question = question.trim();
        if question.is_empty() {
            return Err("Ask a question first.".into());
        }
        if question.chars().count() > self.question {
            return Err(format!("The question can have at most {} characters.", self.question));
        }
        let answers: Vec<String> = answers
            .iter()
            .map(|a| a.trim().to_owned())
            .filter(|a| !a.is_empty())
            .collect();
        if answers.len() < self.min_answers {
            return Err(format!("A poll needs at least {} answers.", self.min_answers));
        }
        if answers.len() > self.max_answers {
            return Err(format!("A poll can have at most {} answers.", self.max_answers));
        }
        if answers.iter().any(|a| a.chars().count() > self.answer) {
            return Err(format!("An answer can have at most {} characters.", self.answer));
        }
        Ok((question.to_owned(), answers))
    }
}

/// Whether `hours` is one of the durations offered.
pub fn valid_duration(hours: u32) -> bool {
    POLL_DURATIONS.iter().any(|(h, _)| *h == hours)
}

#[cfg(test)]
mod tests {
    use super::*;

    const LIMITS: PollLimits = PollLimits {
        question: 300,
        answer: 55,
        min_answers: 2,
        max_answers: 10,
    };

    fn answers(n: usize) -> Vec<String> {
        (0..n).map(|i| format!("a{i}")).collect()
    }

    #[test]
    fn a_good_poll_is_trimmed() {
        let (q, a) = LIMITS
            .check("  Pizza? ", &[" yes ".into(), "".into(), "no".into()])
            .expect("valid");
        assert_eq!((q.as_str(), a), ("Pizza?", vec!["yes".to_owned(), "no".to_owned()]));
    }

    #[test]
    fn limits_are_enforced() {
        assert!(LIMITS.check("", &answers(2)).is_err());
        assert!(LIMITS.check(&"q".repeat(301), &answers(2)).is_err());
        assert!(LIMITS.check(&"q".repeat(300), &answers(2)).is_ok());
        assert!(LIMITS.check("q", &answers(1)).is_err());
        assert!(LIMITS.check("q", &answers(10)).is_ok());
        assert!(LIMITS.check("q", &answers(11)).is_err());
        assert!(LIMITS.check("q", &["x".repeat(56), "b".into()]).is_err());
        assert!(LIMITS.check("q", &["x".repeat(55), "b".into()]).is_ok());
    }

    #[test]
    fn durations_are_the_offered_ones() {
        assert!(valid_duration(24) && valid_duration(168));
        assert!(!valid_duration(5));
        assert_eq!(POLL_DURATIONS[POLL_DEFAULT_DURATION].0, 24);
    }
}
