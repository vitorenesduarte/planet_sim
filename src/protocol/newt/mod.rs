// This module contains the definition of `ProcessVotes`, `Votes` and `VoteRange`.
mod votes;

// This module contains the definition of `MultiVotesTable`.
mod votes_table;

// This module contains the definition of `KeyClocks` and `QuorumClocks`.
mod clocks;

use crate::command::{Command, CommandResult, Pending};
use crate::config::Config;
use crate::id::{Dot, ProcessId, Rifl};
use crate::kvs::{KVOp, KVStore, Key};
use crate::log;
use crate::planet::{Planet, Region};
use crate::protocol::newt::clocks::{KeysClocks, QuorumClocks};
use crate::protocol::newt::votes::{ProcessVotes, Votes};
use crate::protocol::newt::votes_table::MultiVotesTable;
use crate::protocol::{BaseProcess, Process, ToSend};
use std::collections::HashMap;

pub struct Newt {
    bp: BaseProcess,
    keys_clocks: KeysClocks,
    cmds_info: CommandsInfo,
    table: MultiVotesTable,
    store: KVStore,
    pending: Pending,
    commands_ready: Vec<CommandResult>,
}

impl Process for Newt {
    type Message = Message;

    /// Creates a new `Newt` process.
    fn new(process_id: ProcessId, region: Region, planet: Planet, config: Config) -> Self {
        // compute fast quorum size and stability threshold
        let q = Newt::fast_quorum_size(&config);
        let stability_threshold = Newt::stability_threshold(&config);

        // create `MultiVotesTable`
        let table = MultiVotesTable::new(config.n(), stability_threshold);

        // create `BaseProcess`, `Clocks`, dot_to_info, `KVStore` and `Pending`.
        let bp = BaseProcess::new(process_id, region, planet, config, q);
        let keys_clocks = KeysClocks::new(process_id);
        let cmds_info = CommandsInfo::new(q);
        let store = KVStore::new();
        let pending = Pending::new();
        let commands_ready = Vec::new();

        // create `Newt`
        Self {
            bp,
            keys_clocks,
            cmds_info,
            table,
            store,
            pending,
            commands_ready,
        }
    }

    /// Returns the process identifier.
    fn id(&self) -> ProcessId {
        self.bp.process_id
    }

    /// Updates the processes known by this process.
    fn discover(&mut self, processes: Vec<(ProcessId, Region)>) -> bool {
        self.bp.discover(processes)
    }

    /// Submits a command issued by some client.
    fn submit(&mut self, cmd: Command) -> ToSend<Self::Message> {
        self.handle_submit(cmd)
    }

    /// Handles protocol messages.
    fn handle(&mut self, from: ProcessId, msg: Self::Message) -> ToSend<Self::Message> {
        match msg {
            Message::MCollect {
                dot,
                cmd,
                quorum,
                clock,
            } => self.handle_mcollect(from, dot, cmd, quorum, clock),
            Message::MCollectAck {
                dot,
                clock,
                process_votes,
            } => self.handle_mcollectack(from, dot, clock, process_votes),
            Message::MCommit {
                dot,
                cmd,
                clock,
                votes,
            } => self.handle_mcommit(dot, cmd, clock, votes),
            Message::MPhantom { dot, process_votes } => self.handle_mphantom(dot, process_votes),
        }
    }

    /// Returns new commands results to be sent to clients.
    fn commands_ready(&mut self) -> Vec<CommandResult> {
        let mut ready = Vec::new();
        std::mem::swap(&mut ready, &mut self.commands_ready);
        ready
    }
}

impl Newt {
    /// Computes `Newt` fast quorum size.
    fn fast_quorum_size(config: &Config) -> usize {
        2 * config.f()
    }

    /// Computes `Newt` stability threshold.
    /// Typically the threshold should be n - q + 1, where n is the number of
    /// processes and q the size of the write quorum. In `Newt`, although
    /// the fast quorum is 2f (which would suggest q = 2f), in fact q = f + 1.
    /// The quorum size of 2f ensures that all clocks are computed from f + 1
    /// processes. So, n - q + 1 = n - (f + 1) + 1 = n - f
    fn stability_threshold(config: &Config) -> usize {
        config.n() - config.f()
    }

    /// Handles a submit operation by a client.
    fn handle_submit(&mut self, cmd: Command) -> ToSend<Message> {
        // start command in `Pending`
        self.pending.start(&cmd);

        // compute the command identifier
        let dot = self.bp.next_dot();

        // compute its clock
        let clock = self.keys_clocks.clock(&cmd) + 1;

        // get the fast quorum
        let fast_quorum = self.bp.fast_quorum();

        // create `MCollect`
        let mcollect = Message::MCollect {
            dot,
            cmd,
            clock,
            quorum: fast_quorum.clone(),
        };

        // return `ToSend`
        ToSend::ToProcesses(self.id(), fast_quorum, mcollect)
    }

    fn handle_mcollect(
        &mut self,
        from: ProcessId,
        dot: Dot,
        cmd: Command,
        quorum: Vec<ProcessId>,
        clock: u64,
    ) -> ToSend<Message> {
        log!(
            "p{}: MCollect({:?}, {:?}, {}) from {}",
            self.bp.process_id,
            dot,
            cmd,
            clock,
            from
        );

        // get cmd info
        let info = self.cmds_info.get(dot);

        // discard message if no longer in START
        if info.status != Status::START {
            return ToSend::Nothing;
        }

        // TODO can we somehow combine the next 2 operations in order to save map lookups?

        // compute command clock
        let cmd_clock = std::cmp::max(clock, self.keys_clocks.clock(&cmd) + 1);

        // compute votes consumed by this command
        // - this computation needs to occur before the next `bump_to`
        let process_votes = self.keys_clocks.process_votes(&cmd, cmd_clock);
        // check that there's one vote per key
        assert_eq!(process_votes.len(), cmd.key_count());

        // update command info:
        // - change status to collect
        // - save command and quorum
        // - set command clock
        info.status = Status::COLLECT;
        info.cmd = Some(cmd);
        info.quorum = quorum;
        info.clock = cmd_clock;

        // create `MCollectAck`
        let mcollectack = Message::MCollectAck {
            dot,
            clock: info.clock,
            process_votes,
        };

        // return `ToSend`
        ToSend::ToProcesses(self.id(), vec![from], mcollectack)
    }

    fn handle_mcollectack(
        &mut self,
        from: ProcessId,
        dot: Dot,
        clock: u64,
        remote_process_votes: ProcessVotes,
    ) -> ToSend<Message> {
        log!(
            "p{}: MCollectAck({:?}, {}, {:?}) from {}",
            self.bp.process_id,
            dot,
            clock,
            remote_process_votes,
            from
        );

        // get cmd info
        let info = self.cmds_info.get(dot);

        if info.status != Status::COLLECT || info.quorum_clocks.contains(from) {
            // do nothing if we're no longer COLLECT or if this is a
            // duplicated message
            return ToSend::Nothing;
        }

        // update votes with remote votes
        info.votes.add(remote_process_votes);

        // update quorum clocks while computing max clock and its number of
        // occurences
        let (max_clock, max_count) = info.quorum_clocks.add(from, clock);

        // // optimization: bump all keys clocks in `cmd` to be `max_clock`
        // // - this prevents us from generating votes (either when clients submit new operations or
        // //   when handling `MCollect` from other processes) that could potentially delay the
        // //   execution of this command
        // let cmd = info.cmd.as_ref().unwrap();
        // let process_votes = self.keys_clocks.process_votes(cmd, max_clock);
        // println!(
        //     "{}: dot {:?} local votes {:?}",
        //     self.bp.process_id, dot, process_votes
        // );

        // // update votes with local votes
        // info.votes.add(process_votes);

        // check if we have all necessary replies
        if info.quorum_clocks.all() {
            // fast path condition:
            // - if `max_clock` was reported by at least f processes
            if max_count >= self.bp.config.f() {
                // reset local votes as we're going to receive them right away at the same time
                // avoiding a `info.votes.clone()`
                let mut votes = Votes::new();
                std::mem::swap(&mut info.votes, &mut votes);

                // create `MCommit`
                let mcommit = Message::MCommit {
                    dot,
                    cmd: info.cmd.clone(),
                    clock: max_clock,
                    votes,
                };

                // return `ToSend`
                ToSend::ToProcesses(self.id(), self.bp.all(), mcommit)
            } else {
                // TODO slow path
                unimplemented!("slow path not implemented yet")
            }
        } else {
            ToSend::Nothing
        }
    }

    fn handle_mcommit(
        &mut self,
        dot: Dot,
        cmd: Option<Command>,
        clock: u64,
        mut votes: Votes,
    ) -> ToSend<Message> {
        log!(
            "p{}: MCommit({:?}, {}, {:?})",
            self.bp.process_id,
            dot,
            clock,
            votes
        );

        // get cmd info
        let info = self.cmds_info.get(dot);

        if info.status == Status::COMMIT {
            // do nothing if we're already COMMIT
            // TODO what about the executed status?
            return ToSend::Nothing;
        }

        // update command info:
        info.status = Status::COMMIT;
        info.cmd = cmd;
        info.clock = clock;

        // get local votes
        let mut local_votes = Votes::new();
        std::mem::swap(&mut info.votes, &mut local_votes);
        // merge local votes (probably from phantom messages) with received votes
        votes.merge(local_votes);

        // generate phantom votes if committed clock is higher than the local key's clock
        let to_send = if let Some(cmd) = info.cmd.as_ref() {
            // if not a no op, check if we can generate more votes that can speed-up execution
            let process_votes = self.keys_clocks.process_votes(cmd, info.clock);
            // create `MPhantom` if there are new votes
            if process_votes.is_empty() {
                ToSend::Nothing
            } else {
                let mphantom = Message::MPhantom { dot, process_votes };
                ToSend::ToProcesses(self.bp.process_id, self.bp.all(), mphantom)
            }
        } else {
            ToSend::Nothing
        };

        // update votes table and execute commands that can be executed
        let to_execute = self.table.add(dot, info.cmd.clone(), info.clock, votes);
        self.execute(to_execute);

        // return `ToSend`
        to_send
    }

    fn handle_mphantom(&mut self, dot: Dot, process_votes: ProcessVotes) -> ToSend<Message> {
        log!(
            "p{}: MPhantom({:?}, {:?})",
            self.bp.process_id,
            dot,
            process_votes
        );

        // get cmd info
        let info = self.cmds_info.get(dot);

        // TODO if there's ever a Status::EXECUTE, this check might be incorrect
        if info.status == Status::COMMIT {
            // only incorporate votes in the command votes table if the command has been committed
            let to_execute = self.table.add_process_votes(process_votes);
            self.execute(to_execute);
        } else {
            // if not committed yet, update votes with remote votes
            info.votes.add(process_votes);
        }
        ToSend::Nothing
    }

    fn execute(&mut self, to_execute: Option<Vec<(Key, Vec<(Rifl, KVOp)>)>>) {
        if let Some(to_execute) = to_execute {
            // borrow everything we'll need
            let commands_ready = &mut self.commands_ready;
            let store = &mut self.store;
            let pending = &mut self.pending;

            // get more commands that are ready to be executed
            let ready = to_execute
                .into_iter()
                // flatten each pair with a key and list of ready partial operations
                .flat_map(|(key, ops)| {
                    ops.into_iter()
                        .map(move |(rifl, op)| (key.clone(), rifl, op))
                })
                .filter_map(|(key, rifl, op)| {
                    // execute op in the `KVStore`
                    let op_result = store.execute(&key, op);

                    // add partial result to `Pending`
                    pending.add_partial(rifl, key, op_result)
                });
            commands_ready.extend(ready);
        }
    }
}

// `CommandsInfo` contains `CommandInfo` for each `Dot`.
struct CommandsInfo {
    q: usize,
    dot_to_info: HashMap<Dot, CommandInfo>,
}

impl CommandsInfo {
    fn new(q: usize) -> Self {
        Self {
            q,
            dot_to_info: HashMap::new(),
        }
    }

    // Returns the `CommandInfo` associated with `Dot`.
    // If no `CommandInfo` is associated, an empty `CommandInfo` is returned.
    fn get(&mut self, dot: Dot) -> &mut CommandInfo {
        // TODO the borrow checker complains if `self.q` is passed to
        // `CommandInfo::new`
        let q = self.q;
        self.dot_to_info
            .entry(dot)
            .or_insert_with(|| CommandInfo::new(q))
    }
}

// `CommandInfo` contains all information required in the life-cyle of a
// `Command`
struct CommandInfo {
    status: Status,
    quorum: Vec<ProcessId>,
    cmd: Option<Command>, // `None` if noOp
    clock: u64,
    // `votes` is used by the coordinator to aggregate `ProcessVotes` from fast
    // quorum members
    votes: Votes,
    // `quorum_clocks` is used by the coordinator to compute the highest clock
    // reported by fast quorum members and the number of times it was reported
    quorum_clocks: QuorumClocks,
}

impl CommandInfo {
    fn new(q: usize) -> Self {
        Self {
            status: Status::START,
            quorum: vec![],
            cmd: None,
            clock: 0,
            votes: Votes::new(),
            quorum_clocks: QuorumClocks::new(q),
        }
    }
}

// `Newt` protocol messages
#[derive(Debug, Clone, PartialEq)]
pub enum Message {
    MCollect {
        dot: Dot,
        cmd: Command,
        quorum: Vec<ProcessId>,
        clock: u64,
    },
    MCollectAck {
        dot: Dot,
        clock: u64,
        process_votes: ProcessVotes,
    },
    MCommit {
        dot: Dot,
        cmd: Option<Command>,
        clock: u64,
        votes: Votes,
    },
    MPhantom {
        dot: Dot,
        process_votes: ProcessVotes,
    },
}

/// `Status` of commands.
#[derive(PartialEq)]
enum Status {
    START,
    COLLECT,
    COMMIT,
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::client::{Client, Workload};
    use crate::sim::Simulation;
    use crate::time::SimTime;

    #[test]
    fn newt_parameters() {
        let config = Config::new(7, 1);
        assert_eq!(Newt::fast_quorum_size(&config), 2);
        assert_eq!(Newt::stability_threshold(&config), 6);

        let config = Config::new(7, 2);
        assert_eq!(Newt::fast_quorum_size(&config), 4);
        assert_eq!(Newt::stability_threshold(&config), 5);
    }

    #[test]
    fn newt_flow() {
        // processes ids
        let process_id_1 = 1;
        let process_id_2 = 2;
        let process_id_3 = 3;

        // regions
        let europe_west2 = Region::new("europe-west2");
        let europe_west3 = Region::new("europe-west2");
        let us_west1 = Region::new("europe-west2");

        // processes
        let processes = vec![
            (process_id_1, europe_west2.clone()),
            (process_id_2, europe_west3.clone()),
            (process_id_3, us_west1.clone()),
        ];

        // planet
        let planet = Planet::new("latency/");

        // create system time
        let time = SimTime::new();

        // n and f
        let n = 3;
        let f = 1;
        let config = Config::new(n, f);

        // newts
        let mut newt_1 = Newt::new(process_id_1, europe_west2.clone(), planet.clone(), config);
        let mut newt_2 = Newt::new(process_id_2, europe_west3.clone(), planet.clone(), config);
        let mut newt_3 = Newt::new(process_id_3, us_west1.clone(), planet.clone(), config);

        // discover processes in all newts
        newt_1.discover(processes.clone());
        newt_2.discover(processes.clone());
        newt_3.discover(processes.clone());

        // create simulation
        let mut simulation = Simulation::new();

        // register processes
        simulation.register_process(newt_1);
        simulation.register_process(newt_2);
        simulation.register_process(newt_3);

        // client workload
        let conflict_rate = 100;
        let total_commands = 10;
        let workload = Workload::new(conflict_rate, total_commands);

        // create client 1 that is connected to newt 1
        let client_id = 1;
        let client_region = europe_west2.clone();
        let mut client_1 = Client::new(client_id, client_region, planet.clone(), workload);

        // discover processes in client 1
        assert!(client_1.discover(processes));

        // start client
        let (target, cmd) = client_1.start(&time);

        // check that `target` is newt 1
        assert_eq!(target, process_id_1);

        // register clients
        simulation.register_client(client_1);

        // submit it in newt_0
        let mcollects = simulation.get_process(target).submit(cmd);

        // check that the mcollect is being sent to 2 processes
        assert!(mcollects.to_processes());
        if let ToSend::ToProcesses(_, to, _) = mcollects.clone() {
            assert_eq!(to.len(), 2 * f);
            assert_eq!(to, vec![1, 2]);
        } else {
            panic!("ToSend::ToProcesses not found!");
        }

        // handle in mcollects
        let mut mcollectacks = simulation.forward_to_processes(mcollects);

        // check that there are 2 mcollectacks
        assert_eq!(mcollectacks.len(), 2 * f);
        assert!(mcollectacks.iter().all(|to_send| to_send.to_processes()));

        // handle the first mcollectack
        let mut mcommits = simulation.forward_to_processes(mcollectacks.pop().unwrap());
        let mcommit_tosend = mcommits.pop().unwrap();
        // no mcommit yet
        assert!(mcommit_tosend.is_nothing());

        // handle the second mcollectack
        let mut mcommits = simulation.forward_to_processes(mcollectacks.pop().unwrap());
        let mcommit_tosend = mcommits.pop().unwrap();

        // check that there is an mcommit sent to everyone
        assert!(mcommit_tosend.to_processes());
        if let ToSend::ToProcesses(_, to, _) = mcommit_tosend.clone() {
            assert_eq!(to.len(), n);
        } else {
            panic!("ToSend::ToProcesses not found!");
        }

        // all processes handle it
        let to_sends = simulation.forward_to_processes(mcommit_tosend);
        // get the single mphantom from process 3
        let to_sends = to_sends
            .into_iter()
            .filter(|to_send| to_send.to_processes())
            .collect::<Vec<_>>();
        assert_eq!(to_sends.len(), 1);
        match to_sends.into_iter().next().unwrap() {
            ToSend::ToProcesses(from, target, Message::MPhantom { .. }) => {
                assert_eq!(from, process_id_3);
                assert_eq!(target, vec![process_id_1, process_id_2, process_id_3]);
            }
            _ => panic!("Message::MPhantom not found!"),
        }

        // process 1 should have a result to the client
        let commands_ready = simulation.get_process(process_id_1).commands_ready();
        assert_eq!(commands_ready.len(), 1);

        // handle what was sent to client
        let new_submit = simulation
            .forward_to_clients(commands_ready, &time)
            .into_iter()
            .next()
            .unwrap();
        assert!(new_submit.to_coordinator());

        let mcollect = simulation
            .forward_to_processes(new_submit)
            .into_iter()
            .next()
            .unwrap();
        if let ToSend::ToProcesses(from, _, Message::MCollect { dot, .. }) = mcollect {
            assert_eq!(from, target);
            assert_eq!(dot, Dot::new(target, 2));
        } else {
            panic!("Message::MCollect not found!");
        }
    }
}
