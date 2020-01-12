// This module contains the definition of `KeysClocks` and `QuorumClocks`.
mod clocks;

// Re-exports.
pub use clocks::{KeysClocks, QuorumClocks};

use crate::id::ProcessId;
use crate::kvs::Key;
use serde::{Deserialize, Serialize};
use std::collections::hash_map::{self, HashMap};
use std::fmt;

/// `ProcessVotes` are the Votes by some process on some command.
pub type ProcessVotes = HashMap<Key, VoteRange>;

/// Votes are all Votes on some command.
#[derive(Debug, Clone, PartialEq, Default, Serialize, Deserialize)]
pub struct Votes {
    votes: HashMap<Key, Vec<VoteRange>>,
}

impl Votes {
    /// Creates an empty `Votes` instance.
    pub fn new() -> Self {
        Default::default()
    }

    /// Add `ProcessVotes` to `Votes`.
    pub fn add(&mut self, process_votes: ProcessVotes) {
        process_votes.into_iter().for_each(|(key, vote)| {
            // add new vote to current set of votes
            let current_votes = self.get_key_votes(key);
            current_votes.push(vote);
        });
    }

    /// Merge with another `Votes`.
    /// Performance should be better if `self.votes.len() > remote_votes.len()` than with the
    /// opposite.
    pub fn merge(&mut self, remote_votes: Votes) {
        remote_votes.into_iter().for_each(|(key, key_votes)| {
            // add new votes to current set of votes
            let current_votes = self.get_key_votes(key);
            current_votes.extend(key_votes);
        });
    }

    /// Removes the votes on some key.
    pub fn remove_votes(&mut self, key: &Key) -> Option<Vec<VoteRange>> {
        self.votes.remove(key)
    }

    fn get_key_votes(&mut self, key: Key) -> &mut Vec<VoteRange> {
        self.votes.entry(key).or_insert_with(Vec::new)
    }
}

impl IntoIterator for Votes {
    type Item = (Key, Vec<VoteRange>);
    type IntoIter = hash_map::IntoIter<Key, Vec<VoteRange>>;

    /// Returns a `Votes` into-iterator.
    fn into_iter(self) -> Self::IntoIter {
        self.votes.into_iter()
    }
}

// `VoteRange` encodes a set of votes performed by some processed:
// - this will be used to fill the `VotesTable`
#[derive(Clone, PartialEq, Eq, PartialOrd, Ord, Serialize, Deserialize)]
pub struct VoteRange {
    by: ProcessId,
    start: u64,
    end: u64,
}

impl VoteRange {
    /// Create a new `VoteRange` instance.
    pub fn new(by: ProcessId, start: u64, end: u64) -> Self {
        assert!(start <= end);
        Self { by, start, end }
    }

    /// Get which process voted.
    pub fn voter(&self) -> ProcessId {
        self.by
    }

    /// Get range start.
    pub fn start(&self) -> u64 {
        self.start
    }

    /// Get range end.
    pub fn end(&self) -> u64 {
        self.end
    }
}

impl fmt::Debug for VoteRange {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        if self.start == self.end {
            write!(f, "<{}, {}>", self.by, self.start)
        } else {
            write!(f, "<{}, {}-{}>", self.by, self.start, self.end)
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::command::Command;
    use crate::id::Rifl;
    use crate::protocol::common::table::KeysClocks;
    use std::cmp::max;

    impl VoteRange {
        /// Get all votes in this range.
        fn votes(&self) -> Vec<u64> {
            (self.start..=self.end).collect()
        }
    }

    #[test]
    fn votes_flow() {
        // create clocks
        let mut clocks_p0 = KeysClocks::new(0);
        let mut clocks_p1 = KeysClocks::new(1);

        // keys
        let key_a = String::from("A");
        let key_b = String::from("B");

        // command a
        let cmd_a_rifl = Rifl::new(100, 1); // client 100, 1st op
        let cmd_a = Command::get(cmd_a_rifl, key_a.clone());
        let mut votes_a = Votes::new();

        // command b
        let cmd_ab_rifl = Rifl::new(101, 1); // client 101, 1st op
        let cmd_ab = Command::multi_get(cmd_ab_rifl, vec![key_a.clone(), key_b.clone()]);
        let mut votes_ab = Votes::new();

        // orders on each process:
        // - p0: Submit(a),  MCommit(a),  MCollect(ab)
        // - p1: Submit(ab), MCollect(a), MCommit(ab)

        // ------------------------
        // submit command a by p0
        let clock_a = clocks_p0.clock(&cmd_a) + 1;
        assert_eq!(clock_a, 1);

        // ------------------------
        // (local) MCollect handle by p0 (command a)
        let clock_a_p0 = max(clock_a, clocks_p0.clock(&cmd_a) + 1);
        let process_votes_a_p0 = clocks_p0.process_votes(&cmd_a, clock_a_p0);

        // -------------------------
        // submit command ab by p1
        let clock_ab = clocks_p1.clock(&cmd_ab) + 1;
        assert_eq!(clock_ab, 1);

        // -------------------------
        // (local) MCollect handle by p1 (command ab)
        let clock_ab_p1 = max(clock_ab, clocks_p1.clock(&cmd_ab) + 1);
        let process_votes_ab_p1 = clocks_p1.process_votes(&cmd_ab, clock_ab_p1);

        // -------------------------
        // (remote) MCollect handle by p1 (command a)
        let clock_a_p1 = max(clock_a, clocks_p1.clock(&cmd_a) + 1);
        let process_votes_a_p1 = clocks_p1.process_votes(&cmd_a, clock_a_p1);

        // -------------------------
        // (remote) MCollect handle by p0 (command ab)
        let clock_ab_p0 = max(clock_ab, clocks_p0.clock(&cmd_ab) + 1);
        let process_votes_ab_p0 = clocks_p0.process_votes(&cmd_ab, clock_ab_p0);

        // -------------------------
        // MCollectAck handles by p0 (command a)
        votes_a.add(process_votes_a_p0);
        votes_a.add(process_votes_a_p1);

        // there's a single key
        assert_eq!(votes_a.votes.len(), 1);

        // there are two voters
        let key_votes = votes_a.votes.get(&key_a).unwrap();
        assert_eq!(key_votes.len(), 2);

        // p0 voted with 1
        let mut key_votes = key_votes.into_iter();
        let key_votes_by_p0 = key_votes.next().unwrap();
        assert_eq!(key_votes_by_p0.voter(), 0);
        assert_eq!(key_votes_by_p0.votes(), vec![1]);

        // p1 voted with 2
        let key_votes_by_p1 = key_votes.next().unwrap();
        assert_eq!(key_votes_by_p1.voter(), 1);
        assert_eq!(key_votes_by_p1.votes(), vec![2]);

        // -------------------------
        // MCollectAck handles by p1 (command ab)
        votes_ab.add(process_votes_ab_p1);
        votes_ab.add(process_votes_ab_p0);

        // there are two keys
        assert_eq!(votes_ab.votes.len(), 2);

        // key a:
        // there are two voters
        let key_votes = votes_ab.votes.get(&key_a).unwrap();
        assert_eq!(key_votes.len(), 2);

        // p1 voted with 1
        let mut key_votes = key_votes.into_iter();
        let key_votes_by_p1 = key_votes.next().unwrap();
        assert_eq!(key_votes_by_p1.voter(), 1);
        assert_eq!(key_votes_by_p1.votes(), vec![1]);

        // p0 voted with 2
        let key_votes_by_p0 = key_votes.next().unwrap();
        assert_eq!(key_votes_by_p0.voter(), 0);
        assert_eq!(key_votes_by_p0.votes(), vec![2]);

        // key b:
        // there are two voters
        let key_votes = votes_ab.votes.get(&key_b).unwrap();
        assert_eq!(key_votes.len(), 2);

        // p1 voted with 1
        let mut key_votes = key_votes.into_iter();
        let key_votes_by_p1 = key_votes.next().unwrap();
        assert_eq!(key_votes_by_p1.voter(), 1);
        assert_eq!(key_votes_by_p1.votes(), vec![1]);

        // p0 voted with 1 and 2
        let key_votes_by_p0 = key_votes.next().unwrap();
        assert_eq!(key_votes_by_p0.voter(), 0);
        assert_eq!(key_votes_by_p0.votes(), vec![1, 2]);
    }
}