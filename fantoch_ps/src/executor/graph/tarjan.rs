use super::index::{VertexIndex, VertexRef};
use fantoch::command::Command;
use fantoch::id::{Dot, ProcessId, ShardId};
use fantoch::log;
use fantoch::HashSet;
use parking_lot::RwLock;
use std::cmp;
use std::collections::BTreeSet;
use std::sync::Arc;
use threshold::{AEClock, EventSet, VClock};

/// commands are sorted inside an SCC given their dot
pub type SCC = BTreeSet<Dot>;

#[derive(PartialEq)]
pub enum FinderResult {
    Found,
    MissingDependency(Dot),
    NotPending,
    NotFound,
}

#[derive(Clone)]
pub struct TarjanSCCFinder {
    process_id: ProcessId,
    shard_id: ShardId,
    transitive_conflicts: bool,
    id: usize,
    stack: Vec<Dot>,
    sccs: Vec<SCC>,
}

impl TarjanSCCFinder {
    /// Creates a new SCC finder that employs Tarjan's algorithm.
    pub fn new(
        process_id: ProcessId,
        shard_id: ShardId,
        transitive_conflicts: bool,
    ) -> Self {
        Self {
            process_id,
            shard_id,
            transitive_conflicts,
            id: 0,
            stack: Vec::new(),
            sccs: Vec::new(),
        }
    }

    /// Returns a list with the SCCs found.
    #[must_use]
    pub fn sccs(&mut self) -> Vec<SCC> {
        std::mem::take(&mut self.sccs)
    }

    /// Returns a set with all dots visited.
    /// It also resets the ids of all vertices still on the stack.
    #[must_use]
    pub fn finalize(&mut self, vertex_index: &VertexIndex) -> HashSet<Dot> {
        let _process_id = self.process_id;
        // reset id
        self.id = 0;
        // reset the id of each dot in the stack, while computing the set of
        // visited dots
        let mut visited = HashSet::new();
        while let Some(dot) = self.stack.pop() {
            log!(
                "p{}: Finder::finalize removing {:?} from stack",
                _process_id,
                dot
            );

            // find vertex and reset its id
            let vertex = if let Some(vertex) = vertex_index.find(&dot) {
                vertex
            } else {
                panic!(
                    "p{}: stack member {:?} should exist",
                    self.process_id, dot
                );
            };
            vertex.lock().id = 0;

            // add dot to set of visited
            visited.insert(dot);
        }
        // return visited dots
        visited
    }

    /// Tries to find an SCC starting from root `dot`.
    pub fn strong_connect(
        &mut self,
        dot: Dot,
        vertex_ref: &VertexRef<'_>,
        executed_clock: &Arc<RwLock<AEClock<ProcessId>>>,
        vertex_index: &VertexIndex,
        found: &mut usize,
    ) -> FinderResult {
        // update id
        self.id += 1;

        // get vertex
        let mut vertex = vertex_ref.lock();

        // set id and low for vertex
        vertex.id = self.id;
        vertex.low = vertex.id;

        // add to the stack
        vertex.on_stack = true;
        self.stack.push(dot);

        log!(
            "p{}: Finder::strong_connect {:?} with id {}",
            self.process_id,
            dot,
            self.id
        );

        // TODO can we avoid vertex.clock().clone()
        // - if rust understood mutability of struct fields, the clone wouldn't
        //   be necessary
        // compute non-executed deps for each process
        for (process_id, to) in vertex.clock.clone().iter() {
            // get min event from which we need to start checking for
            // dependencies
            let to = to.frontier();
            let from = if self.transitive_conflicts {
                // if we can assume that conflicts are transitive, it is enough
                // to check for the highest dependency
                to
            } else {
                executed_clock
                    .read()
                    .get(process_id)
                    .expect("process should exist in the executed clock")
                    .frontier()
                    + 1
            };

            // OPTIMIZATION: start from the highest dep to the lowest:
            // - assuming we will give up, we give up faster this way
            // THE BENEFITS ARE HUGE!!!
            // - obviously, this is only relevant when we can't assume that
            //   conflicts are transitive
            // - when we can, the following loop has a single iteration
            for dep in (from..=to).rev() {
                // ignore dependency if already executed:
                // - we need this check because the clock may not be contiguous,
                //   i.e. `executed_clock_frontier` is simply a safe
                //   approximation of what's been executed
                if executed_clock.read().contains(process_id, dep) {
                    continue;
                }

                // create dot and find vertex
                let dep_dot = Dot::new(*process_id, dep);
                log!(
                    "p{}: Finder::strong_connect non-executed {:?}",
                    self.process_id,
                    dep_dot
                );

                // ignore dependency if self
                if dep_dot == dot {
                    continue;
                }

                match vertex_index.find(&dep_dot) {
                    None => {
                        // not necesserarily a missing dependency, since it may
                        // not conflict with `dot` but
                        // we can't be sure until we have it locally
                        log!(
                            "p{}: Finder::strong_connect missing {:?}",
                            self.process_id,
                            dep_dot
                        );
                        return FinderResult::MissingDependency(dep_dot);
                    }
                    Some(dep_vertex_ref) => {
                        // get vertex
                        let mut dep_vertex = dep_vertex_ref.lock();

                        // ignore non-conflicting commands:
                        // - this check is only necesssary if we can't assume
                        //   that conflicts are transitive
                        if !self.transitive_conflicts
                            && !vertex.conflicts(&dep_vertex)
                        {
                            log!(
                                "p{}: Finder::strong_connect non-conflicting {:?}",
                                self.process_id,
                                dep_dot
                            );
                            continue;
                        }

                        // if not visited, visit
                        if dep_vertex.id == 0 {
                            log!(
                                "p{}: Finder::strong_connect non-visited {:?}",
                                self.process_id,
                                dep_dot
                            );

                            // drop guards
                            drop(vertex);
                            drop(dep_vertex);

                            // OPTIMIZATION: passing the vertex as an argument
                            // to `strong_connect`
                            // is also essential to avoid double look-up
                            let result = self.strong_connect(
                                dep_dot,
                                &dep_vertex_ref,
                                executed_clock,
                                vertex_index,
                                found,
                            );

                            // if missing dependency, give up
                            if let FinderResult::MissingDependency(_) = result {
                                return result;
                            }

                            // get guards again
                            vertex = vertex_ref.lock();
                            dep_vertex = dep_vertex_ref.lock();

                            // min low with dep low
                            vertex.low = cmp::min(vertex.low, dep_vertex.low);

                            // drop dep guard
                            drop(dep_vertex);
                        } else {
                            // if visited and on the stack
                            if dep_vertex.on_stack {
                                log!("p{}: Finder::strong_connect dependency on stack {:?}", self.process_id, dep_dot);
                                // min low with dep id
                                vertex.low =
                                    cmp::min(vertex.low, dep_vertex.id);
                            }

                            // drop dep guard
                            drop(dep_vertex);
                        }
                    }
                }
            }
        }

        // if after visiting all neighbors, an SCC was found if vertex.id ==
        // vertex.low
        // - good news: the SCC members are on the stack
        if vertex.id == vertex.low {
            let mut scc = SCC::new();

            // drop guard
            drop(vertex);

            loop {
                // pop an element from the stack
                let member_dot = self
                    .stack
                    .pop()
                    .expect("there should be an SCC member on the stack");

                log!(
                    "p{}: Finder::strong_connect new SCC member {:?}",
                    self.process_id,
                    member_dot
                );

                // get its vertex and change its `on_stack` value
                let member_vertex_ref = vertex_index
                    .find(&member_dot)
                    .expect("stack member should exist");

                // increment number of commands found
                *found += 1;

                // get its vertex and change its `on_stack` value
                let mut member_vertex = member_vertex_ref.lock();
                member_vertex.on_stack = false;

                // add it to the SCC and check it wasn't there before
                assert!(scc.insert(member_dot));

                // update executed clock:
                // - this is a nice optimization (that I think we missed in
                //   Atlas); instead of waiting for the root-level recursion to
                //   finish in order to update `executed_clock` (which is
                //   consulted to decide what are the dependencies of a
                //   command), we can update it right here, possibly reducing a
                //   few iterations
                if !executed_clock
                    .write()
                    .add(&member_dot.source(), member_dot.sequence())
                {
                    panic!(
                        "p{}: Finder::strong_connect dot {:?} already executed",
                        self.process_id, dot
                    );
                }

                log!(
                    "p{}: Finder::strong_connect executed clock {:?}",
                    self.process_id,
                    executed_clock.read()
                );

                // quit if root is found
                if member_dot == dot {
                    break;
                }
            }
            // add scc to to the set of sccs
            self.sccs.push(scc);
            FinderResult::Found
        } else {
            FinderResult::NotFound
        }
    }
}

#[derive(Debug, Clone)]
pub struct Vertex {
    dot: Dot,
    pub cmd: Command,
    pub clock: VClock<ProcessId>,
    // specific to tarjan's algorithm
    id: usize,
    low: usize,
    on_stack: bool,
}

impl Vertex {
    pub fn new(dot: Dot, cmd: Command, clock: VClock<ProcessId>) -> Self {
        Self {
            dot,
            cmd,
            clock,
            id: 0,
            low: 0,
            on_stack: false,
        }
    }

    /// Consumes the vertex, returning its command.
    pub fn into_command(self) -> Command {
        self.cmd
    }

    /// Retrieves vertex's dot.
    pub fn dot(&self) -> Dot {
        self.dot
    }

    /// This vertex conflicts with another vertex by checking if their commands
    /// conflict.
    fn conflicts(&self, other: &Vertex) -> bool {
        self.cmd.conflicts(&other.cmd)
    }
}
