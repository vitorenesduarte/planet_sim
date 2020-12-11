use crate::executor::pred::PredecessorsGraph;
use crate::protocol::common::pred::Clock;
use fantoch::command::Command;
use fantoch::config::Config;
use fantoch::executor::{Executor, ExecutorMetrics, ExecutorResult};
use fantoch::id::{Dot, ProcessId, ShardId};
use fantoch::kvs::KVStore;
use fantoch::protocol::MessageIndex;
use fantoch::time::SysTime;
use fantoch::trace;
use fantoch::HashSet;
use serde::{Deserialize, Serialize};

#[derive(Clone)]
pub struct PredecessorsExecutor {
    process_id: ProcessId,
    shard_id: ShardId,
    config: Config,
    graph: PredecessorsGraph,
    store: KVStore,
    to_clients: Vec<ExecutorResult>,
}

impl Executor for PredecessorsExecutor {
    type ExecutionInfo = PredecessorsExecutionInfo;

    fn new(process_id: ProcessId, shard_id: ShardId, config: Config) -> Self {
        let graph = PredecessorsGraph::new(process_id, &config);
        let store = KVStore::new();
        let to_clients = Vec::new();
        Self {
            process_id,
            shard_id,
            config,
            graph,
            store,
            to_clients,
        }
    }

    fn handle(&mut self, info: PredecessorsExecutionInfo, time: &dyn SysTime) {
        if self.config.execute_at_commit() {
            self.execute(info.cmd);
        } else {
            // handle new command
            self.graph
                .add(info.dot, info.cmd, info.clock, info.deps, time);

            // get more commands that are ready to be executed
            while let Some(cmd) = self.graph.command_to_execute() {
                trace!(
                    "p{}: PredecessorsExecutor::comands_to_execute {:?} | time = {}",
                    self.process_id,
                    cmd.rifl(),
                    time.millis()
                );
                self.execute(cmd);
            }
        }
    }

    fn to_clients(&mut self) -> Option<ExecutorResult> {
        self.to_clients.pop()
    }

    fn parallel() -> bool {
        true
    }

    fn metrics(&self) -> &ExecutorMetrics {
        &self.graph.metrics()
    }
}

impl PredecessorsExecutor {
    fn execute(&mut self, cmd: Command) {
        // execute the command
        let results = cmd.execute(self.shard_id, &mut self.store);
        self.to_clients.extend(results);
    }
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct PredecessorsExecutionInfo {
    dot: Dot,
    cmd: Command,
    clock: Clock,
    deps: HashSet<Dot>,
}

impl PredecessorsExecutionInfo {
    pub fn new(
        dot: Dot,
        cmd: Command,
        clock: Clock,
        deps: HashSet<Dot>,
    ) -> Self {
        Self {
            dot,
            cmd,
            clock,
            deps,
        }
    }
}

impl MessageIndex for PredecessorsExecutionInfo {
    fn index(&self) -> Option<(usize, usize)> {
        None
    }
}