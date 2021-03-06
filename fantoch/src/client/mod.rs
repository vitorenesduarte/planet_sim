// This module contains the definition of `Workload`
pub mod workload;

// This module contains the definition of `KeyGenerator` and
// `KeyGeneratorState`.
pub mod key_gen;

// This module contains the definition of `Pending`
pub mod pending;

// This module contains the definition of `ClientData`
pub mod data;

// Re-exports.
pub use data::ClientData;
pub use key_gen::KeyGen;
pub use pending::Pending;
pub use workload::Workload;

use crate::command::Command;
use crate::id::{ClientId, ProcessId, Rifl, RiflGen, ShardId};
use crate::time::SysTime;
use crate::HashMap;
use crate::{info, trace};
use key_gen::KeyGenState;

pub struct Client {
    /// id of this client
    client_id: ClientId,
    /// mapping from shard id to the process id of that shard this client is
    /// connected to
    processes: HashMap<ShardId, ProcessId>,
    /// rifl id generator
    rifl_gen: RiflGen,
    /// workload configuration
    workload: Workload,
    /// state needed by key generator
    key_gen_state: KeyGenState,
    /// map from pending command RIFL to its start time
    pending: Pending,
    /// mapping from
    data: ClientData,
    /// frequency of status messages; if set with Some(1), a status message
    /// will be shown after each command completes
    status_frequency: Option<usize>,
}

impl Client {
    /// Creates a new client.
    pub fn new(
        client_id: ClientId,
        workload: Workload,
        status_frequency: Option<usize>,
    ) -> Self {
        // create key gen state
        let key_gen_state = workload
            .key_gen()
            .initial_state(workload.shard_count(), client_id);
        // create client
        Self {
            client_id,
            processes: HashMap::new(),
            rifl_gen: RiflGen::new(client_id),
            workload,
            key_gen_state,
            pending: Pending::new(),
            data: ClientData::new(),
            status_frequency,
        }
    }

    /// Returns the client identifier.
    pub fn id(&self) -> ClientId {
        self.client_id
    }

    /// "Connect" to the closest process on each shard.
    pub fn connect(&mut self, processes: HashMap<ShardId, ProcessId>) {
        self.processes = processes;
    }

    /// Retrieves the closest process on this shard.
    pub fn shard_process(&self, shard_id: &ShardId) -> ProcessId {
        *self
            .processes
            .get(shard_id)
            .expect("client should be connected to all shards")
    }

    /// Generates the next command in this client's workload.
    pub fn cmd_send(
        &mut self,
        time: &dyn SysTime,
    ) -> Option<(ShardId, Command)> {
        // generate next command in the workload if some process_id
        self.workload
            .next_cmd(&mut self.rifl_gen, &mut self.key_gen_state)
            .map(|(target_shard, cmd)| {
                // if a new command was generated, start it in pending
                let rifl = cmd.rifl();
                trace!(
                    "c{}: new rifl pending {:?} | time = {}",
                    self.client_id,
                    rifl,
                    time.micros()
                );
                self.pending.start(rifl, time);
                (target_shard, cmd)
            })
    }

    /// Handle executed command and return a boolean indicating whether we have
    /// generated all commands and receive all the corresponding command
    /// results.
    pub fn cmd_recv(&mut self, rifl: Rifl, time: &dyn SysTime) {
        // end command in pending and save command latency
        let (latency, end_time) = self.pending.end(rifl, time);
        trace!(
            "c{}: rifl {:?} ended after {} micros at {}",
            self.client_id,
            rifl,
            latency.as_micros(),
            end_time
        );
        self.data.record(latency, end_time);

        if let Some(frequency) = self.status_frequency {
            if self.workload.issued_commands() % frequency == 0 {
                info!(
                    "c{:?}: {} of {}",
                    self.client_id,
                    self.workload.issued_commands(),
                    self.workload.commands_per_client()
                );
            }
        }
    }

    pub fn workload_finished(&self) -> bool {
        self.workload.finished()
    }

    pub fn finished(&self) -> bool {
        // we're done once:
        // - the workload is finished and
        // - pending is empty
        self.workload.finished() && self.pending.is_empty()
    }

    pub fn data(&self) -> &ClientData {
        &self.data
    }

    /// Returns the number of commands already issued.
    pub fn issued_commands(&self) -> usize {
        self.workload.issued_commands()
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::planet::{Planet, Region};
    use crate::time::SimTime;
    use crate::util;
    use std::collections::BTreeMap;
    use std::iter::FromIterator;
    use std::time::Duration;

    // Generates some client.
    fn gen_client(commands_per_client: usize) -> Client {
        // workload
        let shard_count = 1;
        let keys_per_command = 1;
        let conflict_rate = 100;
        let pool_size = 1;
        let key_gen = KeyGen::ConflictPool {
            conflict_rate,
            pool_size,
        };
        let payload_size = 100;
        let workload = Workload::new(
            shard_count,
            key_gen,
            keys_per_command,
            commands_per_client,
            payload_size,
        );

        // client
        let id = 1;
        let status_frequency = None;
        Client::new(id, workload, status_frequency)
    }

    #[test]
    fn discover() {
        // create planet
        let planet = Planet::new();

        // there are two shards
        let shard_0_id = 0;
        let shard_1_id = 1;

        // processes
        let processes = vec![
            (0, shard_0_id, Region::new("asia-east1")),
            (1, shard_0_id, Region::new("australia-southeast1")),
            (2, shard_0_id, Region::new("europe-west1")),
            (3, shard_1_id, Region::new("europe-west2")),
        ];

        // client
        let region = Region::new("europe-west2");
        let commands_per_client = 0;
        let mut client = gen_client(commands_per_client);

        // check discover with empty vec
        let closest = util::closest_process_per_shard(&region, &planet, vec![]);
        client.connect(closest);
        assert!(client.processes.is_empty());

        // check discover with processes
        let closest =
            util::closest_process_per_shard(&region, &planet, processes);
        client.connect(closest);
        assert_eq!(
            BTreeMap::from_iter(client.processes),
            // connected to process 2 on shard 0 and process 3 on shard 1
            BTreeMap::from_iter(vec![(shard_0_id, 2), (shard_1_id, 3)])
        );
    }

    #[test]
    fn client_flow() {
        // create planet
        let planet = Planet::new();

        // there's a single shard
        let shard_id = 0;

        // processes
        let processes = vec![
            (0, shard_id, Region::new("asia-east1")),
            (1, shard_id, Region::new("australia-southeast1")),
            (2, shard_id, Region::new("europe-west1")),
        ];

        // client
        let region = Region::new("europe-west2");
        let commands_per_client = 2;
        let mut client = gen_client(commands_per_client);

        // discover
        let closest =
            util::closest_process_per_shard(&region, &planet, processes);
        client.connect(closest);

        // create system time
        let mut time = SimTime::new();

        // start client at time 0
        let (shard_id, cmd) = client
            .cmd_send(&time)
            .expect("there should a first operation");
        let process_id = client.shard_process(&shard_id);
        // process_id should be 2
        assert_eq!(process_id, 2);

        // handle result at time 10
        time.add_millis(10);
        client.cmd_recv(cmd.rifl(), &time);
        let next = client.cmd_send(&time);

        // check there's next command
        assert!(next.is_some());
        let (shard_id, cmd) = next.unwrap();
        let process_id = client.shard_process(&shard_id);
        // process_id should be 2
        assert_eq!(process_id, 2);

        // handle result at time 15
        time.add_millis(5);
        client.cmd_recv(cmd.rifl(), &time);
        let next = client.cmd_send(&time);

        // check there's no next command
        assert!(next.is_none());

        // check latency
        let mut latency: Vec<_> = client.data().latency_data().collect();
        latency.sort();
        assert_eq!(
            latency,
            vec![Duration::from_millis(5), Duration::from_millis(10)]
        );

        // check throughput
        let mut throughput: Vec<_> = client.data().throughput_data().collect();
        throughput.sort();
        assert_eq!(throughput, vec![(10, 1), (15, 1)],);
    }
}
