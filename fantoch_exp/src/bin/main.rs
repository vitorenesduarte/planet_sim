use color_eyre::eyre::WrapErr;
use color_eyre::Report;
use fantoch::client::{KeyGen, Workload};
use fantoch::config::Config;
use fantoch::planet::Planet;
use fantoch_exp::bench::ExperimentTimeouts;
use fantoch_exp::machine::Machines;
use fantoch_exp::progress::TracingProgressBar;
use fantoch_exp::{FantochFeature, Protocol, RunMode, Testbed};
use rusoto_core::Region;
use std::path::Path;
use std::time::Duration;
use tsunami::providers::aws::LaunchMode;
use tsunami::Tsunami;

// timeouts
const fn minutes(minutes: u64) -> Duration {
    let one_minute = 60;
    Duration::from_secs(one_minute * minutes)
}
const EXPERIMENT_TIMEOUTS: ExperimentTimeouts = ExperimentTimeouts {
    start: Some(minutes(20)),
    run: Some(minutes(20)),
    stop: Some(minutes(20)),
};

// latency dir
const LATENCY_AWS: &str = "../latency_aws/2020_06_05";
// const LATENCY_AWS: &str = "../latency_aws/2021_02_13";

// aws experiment config
const LAUCH_MODE: LaunchMode = LaunchMode::OnDemand;
// const SERVER_INSTANCE_TYPE: &str = "m5.4xlarge";
const SERVER_INSTANCE_TYPE: &str = "c5.2xlarge";
const CLIENT_INSTANCE_TYPE: &str = "m5.2xlarge";
const MAX_SPOT_INSTANCE_REQUEST_WAIT_SECS: u64 = 5 * 60; // 5 minutes

// processes config
const EXECUTE_AT_COMMIT: bool = false;
const EXECUTOR_CLEANUP_INTERVAL: Duration = Duration::from_millis(10);
const EXECUTOR_MONITOR_PENDING_INTERVAL: Option<Duration> = None;
const GC_INTERVAL: Option<Duration> = Some(Duration::from_millis(50));
const SEND_DETACHED_INTERVAL: Duration = Duration::from_millis(5);

// clients config
const COMMANDS_PER_CLIENT_WAN: usize = 500;
const COMMANDS_PER_CLIENT_LAN: usize = 5_000;

// batching config
const BATCH_MAX_DELAY: Duration = Duration::from_millis(5);

// fantoch run config
const BRANCH: &str = "master";

// tracing max log level: compile-time level should be <= run-time level
const MAX_LEVEL_COMPILE_TIME: tracing::Level = tracing::Level::INFO;
const MAX_LEVEL_RUN_TIME: tracing::Level = tracing::Level::INFO;

// release run
const FEATURES: &[FantochFeature] = &[FantochFeature::Jemalloc];
const RUN_MODE: RunMode = RunMode::Release;

// heaptrack run (no jemalloc feature as heaptrack doesn't support it)
// const FEATURES: &[FantochFeature] = &[];
// const RUN_MODE: RunMode = RunMode::Heaptrack;

// flamegraph run
// const FEATURES: &[FantochFeature] = &[FantochFeature::Jemalloc];
// const RUN_MODE: RunMode = RunMode::Flamegraph;

// list of protocol binaries to cleanup before running the experiment
const PROTOCOLS_TO_CLEANUP: &[Protocol] = &[
    Protocol::TempoAtomic,
    /*
    Protocol::Basic,
    Protocol::AtlasLocked,
    Protocol::EPaxosLocked,
    Protocol::CaesarLocked,
    Protocol::FPaxos,
    Protocol::CaesarLocked,
    */
];

macro_rules! config {
    ($n:expr, $f:expr, $tiny_quorums:expr, $clock_bump_interval:expr, $skip_fast_ack:expr) => {{
        let mut config = Config::new($n, $f);
        config.set_tempo_tiny_quorums($tiny_quorums);
        if let Some(interval) = $clock_bump_interval {
            config.set_tempo_clock_bump_interval::<Option<Duration>>(interval);
        }
        config.set_skip_fast_ack($skip_fast_ack);
        config.set_execute_at_commit(EXECUTE_AT_COMMIT);
        config.set_executor_cleanup_interval(EXECUTOR_CLEANUP_INTERVAL);
        if let Some(interval) = EXECUTOR_MONITOR_PENDING_INTERVAL {
            config.set_executor_monitor_pending_interval(interval);
        }
        if let Some(interval) = GC_INTERVAL {
            config.set_gc_interval(interval);
        }
        config.set_tempo_detached_send_interval(SEND_DETACHED_INTERVAL);
        config
    }};
}

#[tokio::main]
async fn main() -> Result<(), Report> {
    // fairness_and_tail_latency_plot().await
    // increasing_load_plot().await
    // batching_plot().await
    partial_replication_plot().await
}

#[allow(dead_code)]
async fn partial_replication_plot() -> Result<(), Report> {
    // folder where all results will be stored
    let results_dir = "../results_partial_replication";

    // THROUGHPUT
    let regions = vec![Region::EuWest1, Region::UsWest1, Region::ApSoutheast1];
    let n = regions.len();

    let mut configs = vec![
        // (protocol, (n, f, tiny quorums, clock bump interval, skip fast ack))
        (Protocol::TempoAtomic, config!(n, 1, false, None, false)),
        // (Protocol::AtlasLocked, config!(n, 1, false, None, false)),
    ];

    let clients_per_region = vec![
        // 256,
        1024,
        // for Atlas s=2,4 zipf=0.7 r=50%:
        // 1024 * 2,
        // 1024 * 4,
        // 1024 * 8,
        // for Atlas s=2 zipf=0.7 r=95%:
        // 1024 * 10,
        // for Atlas s=2 zipf=0.7 r=95%:
        // 1024 * 12,
        // 1024 * 16,
        // for Atlas s=2 zipf=0.5 r=50% | Atlas s=4 zipf=0.7 r=95%:
        // 1024 * 20,
        // 1024 * 24,
        // 1024 * 32,
        // 1024 * 34,
        1024 * 36,
        // 1024 * 40,
        // 1024 * 44,
        1024 * 48,
        1024 * 52,
        1024 * 64,
        1024 * 72,
        1024 * 80,
        // 1024 * 88,
        // 1024 * 96,
        // 1024 * 104,
        // 1024 * 112,
        // 1024 * 128,
        // 1024 * 144,
    ];
    let batch_max_sizes = vec![1];

    // shard_counts: 2, 4, 6
    let shard_count = 6;
    let keys_per_command = 2;
    let payload_size = 100;
    let cpus = 12;

    let mut workloads = Vec::new();
    for coefficient in vec![0.5, 0.7] {
        // janus*:
        // for read_only_percentage in vec![100, 95, 50] {
        // tempo:
        for read_only_percentage in vec![0] {
            let key_gen = KeyGen::Zipf {
                total_keys_per_shard: 1_000_000,
                coefficient,
            };
            let mut workload = Workload::new(
                shard_count,
                key_gen,
                keys_per_command,
                COMMANDS_PER_CLIENT_WAN,
                payload_size,
            );
            workload.set_read_only_percentage(read_only_percentage);
            workloads.push(workload);
        }
    }

    // don't skip
    let skip = |_, _, _| false;

    // set shards in each config
    configs
        .iter_mut()
        .for_each(|(_protocol, config)| config.set_shard_count(shard_count));

    // init logging
    let progress = TracingProgressBar::init(
        (workloads.len()
            * clients_per_region.len()
            * configs.len()
            * batch_max_sizes.len()) as u64,
    );

    // create AWS planet
    let planet = Some(Planet::from(LATENCY_AWS));

    baremetal_bench(
        regions,
        shard_count,
        planet,
        configs,
        clients_per_region,
        workloads,
        batch_max_sizes,
        cpus,
        skip,
        progress,
        results_dir,
    )
    .await
}

#[allow(dead_code)]
async fn batching_plot() -> Result<(), Report> {
    // folder where all results will be stored
    let results_dir = "../results_batching";

    // THROUGHPUT
    let regions = vec![
        Region::EuWest1,
        Region::UsWest1,
        Region::ApSoutheast1,
        Region::CaCentral1,
        Region::SaEast1,
    ];
    let n = regions.len();

    let mut configs = vec![
        // (protocol, (n, f, tiny quorums, clock bump interval, skip fast ack))
        (Protocol::TempoAtomic, config!(n, 1, false, None, false)),
        // (Protocol::FPaxos, config!(n, 1, false, None, false)),
    ];

    let clients_per_region = vec![
        1024,
        // 1024 * 2,
        // 1024 * 4,
        1024 * 8,
        1024 * 16,
        1024 * 20,
        1024 * 24,
        1024 * 28,
        // 1024 * 44,
        // 1024 * 48,
        // 1024 * 52,
        // 1024 * 56,
        // 1024 * 60,
        // 1024 * 64,
    ];
    let batch_max_sizes = vec![1, 10000];

    let shard_count = 1;
    let keys_per_command = 1;
    let payload_sizes = vec![256, 1024, 4096];
    let cpus = 12;

    let key_gen = KeyGen::ConflictPool {
        conflict_rate: 2,
        pool_size: 1,
    };

    let mut workloads = Vec::new();
    for payload_size in payload_sizes {
        let workload = Workload::new(
            shard_count,
            key_gen,
            keys_per_command,
            COMMANDS_PER_CLIENT_WAN,
            payload_size,
        );
        workloads.push(workload);
    }

    let skip = |_, _, _| false;

    // set shards in each config
    configs
        .iter_mut()
        .for_each(|(_protocol, config)| config.set_shard_count(shard_count));

    // init logging
    let progress = TracingProgressBar::init(
        (workloads.len()
            * clients_per_region.len()
            * configs.len()
            * batch_max_sizes.len()) as u64,
    );

    // create AWS planet
    let planet = Some(Planet::from(LATENCY_AWS));
    baremetal_bench(
        regions.clone(),
        shard_count,
        planet.clone(),
        configs.clone(),
        clients_per_region.clone(),
        workloads.clone(),
        batch_max_sizes,
        cpus,
        skip,
        progress.clone(),
        results_dir,
    )
    .await?;
    Ok(())
}

#[allow(dead_code)]
async fn increasing_load_plot() -> Result<(), Report> {
    // folder where all results will be stored
    let results_dir = "../results_increasing_load";

    // THROUGHPUT
    let regions = vec![
        Region::EuWest1,
        Region::UsWest1,
        Region::ApSoutheast1,
        Region::CaCentral1,
        Region::SaEast1,
    ];
    let n = regions.len();

    let mut configs = vec![
        // (protocol, (n, f, tiny quorums, clock bump interval, skip fast ack))
        // (Protocol::Basic, config!(n, 1, false, None, false)),
        (Protocol::TempoAtomic, config!(n, 1, false, None, false)),
        (Protocol::TempoAtomic, config!(n, 2, false, None, false)),
        (Protocol::FPaxos, config!(n, 1, false, None, false)),
        (Protocol::FPaxos, config!(n, 2, false, None, false)),
        (Protocol::AtlasLocked, config!(n, 1, false, None, false)),
        (Protocol::AtlasLocked, config!(n, 2, false, None, false)),
        // (Protocol::EPaxosLocked, config!(n, 2, false, None, false)),
        (Protocol::CaesarLocked, config!(n, 2, false, None, false)),
    ];

    let clients_per_region = vec![
        32,
        512,
        1024,
        1024 * 2,
        1024 * 4,
        1024 * 8,
        1024 * 16,
        1024 * 20,
    ];
    let batch_max_sizes = vec![1];

    let shard_count = 1;
    let keys_per_command = 1;
    let payload_size = 4096;
    let cpus = 12;

    let key_gens = vec![
        KeyGen::ConflictPool {
            conflict_rate: 2,
            pool_size: 1,
        },
        KeyGen::ConflictPool {
            conflict_rate: 10,
            pool_size: 1,
        },
    ];

    let mut workloads = Vec::new();
    for key_gen in key_gens {
        let workload = Workload::new(
            shard_count,
            key_gen,
            keys_per_command,
            COMMANDS_PER_CLIENT_WAN,
            payload_size,
        );
        workloads.push(workload);
    }

    let skip = |protocol, _, clients| {
        // skip Atlas with more than 4096 clients
        protocol == Protocol::AtlasLocked && clients > 1024 * 20
    };

    // set shards in each config
    configs
        .iter_mut()
        .for_each(|(_protocol, config)| config.set_shard_count(shard_count));

    // init logging
    let progress = TracingProgressBar::init(
        (workloads.len()
            * clients_per_region.len()
            * configs.len()
            * batch_max_sizes.len()) as u64,
    );

    // create AWS planet
    let planet = Some(Planet::from(LATENCY_AWS));

    baremetal_bench(
        regions,
        shard_count,
        planet,
        configs,
        clients_per_region,
        workloads,
        batch_max_sizes,
        cpus,
        skip,
        progress,
        results_dir,
    )
    .await
}

#[allow(dead_code)]
async fn fairness_and_tail_latency_plot() -> Result<(), Report> {
    let results_dir = "../results_fairness_and_tail_latency";
    let regions = vec![
        Region::EuWest1,
        Region::UsWest1,
        Region::ApSoutheast1,
        Region::CaCentral1,
        Region::SaEast1,
    ];
    let n = regions.len();

    let mut configs = vec![
        // (protocol, (n, f, tiny quorums, clock bump interval, skip fast ack))
        (Protocol::FPaxos, config!(n, 1, false, None, false)),
        (Protocol::FPaxos, config!(n, 2, false, None, false)),
        (Protocol::TempoAtomic, config!(n, 1, false, None, false)),
        (Protocol::TempoAtomic, config!(n, 2, false, None, false)),
        (Protocol::AtlasLocked, config!(n, 1, false, None, false)),
        (Protocol::AtlasLocked, config!(n, 2, false, None, false)),
        (Protocol::EPaxosLocked, config!(n, 2, false, None, false)),
        (Protocol::CaesarLocked, config!(n, 2, false, None, false)),
    ];

    let clients_per_region = vec![256, 512];
    let batch_max_sizes = vec![1];

    let shard_count = 1;
    let keys_per_command = 1;
    let payload_size = 100;
    let cpus = 8;

    let key_gen = KeyGen::ConflictPool {
        conflict_rate: 2,
        pool_size: 1,
    };
    let key_gens = vec![key_gen];

    let mut workloads = Vec::new();
    for key_gen in key_gens {
        let workload = Workload::new(
            shard_count,
            key_gen,
            keys_per_command,
            COMMANDS_PER_CLIENT_WAN,
            payload_size,
        );
        workloads.push(workload);
    }

    let skip = |protocol, _, clients| {
        // only run FPaxos with 512 clients
        protocol == Protocol::FPaxos && clients != 512
    };

    // set shards in each config
    configs
        .iter_mut()
        .for_each(|(_protocol, config)| config.set_shard_count(shard_count));

    // init logging
    let progress = TracingProgressBar::init(
        (workloads.len()
            * clients_per_region.len()
            * configs.len()
            * batch_max_sizes.len()) as u64,
    );

    // create AWS planet
    // let planet = Some(Planet::from("../latency_aws"));

    // baremetal_bench(
    aws_bench(
        regions,
        shard_count,
        // planet,
        configs,
        clients_per_region,
        workloads,
        batch_max_sizes,
        cpus,
        skip,
        progress,
        results_dir,
    )
    .await
}

#[allow(dead_code)]
async fn whatever_plot() -> Result<(), Report> {
    // folder where all results will be stored
    let results_dir = "../results_scalability";

    // THROUGHPUT
    let regions = vec![
        Region::EuWest1,
        Region::UsWest1,
        Region::ApSoutheast1,
        /* Region::CaCentral1,
         * Region::SaEast1, */
    ];
    let n = regions.len();

    let mut configs = vec![
        // (protocol, (n, f, tiny quorums, clock bump interval, skip fast ack))
        (Protocol::TempoAtomic, config!(n, 1, false, None, false)),
        /* (Protocol::TempoAtomic, config!(n, 2, false, None, false)), */
        /*
        (Protocol::FPaxos, config!(n, 1, false, None, false)),
        (Protocol::FPaxos, config!(n, 2, false, None, false)),
        (Protocol::AtlasLocked, config!(n, 1, false, None, false)),
        (Protocol::AtlasLocked, config!(n, 2, false, None, false)),
        */
    ];

    let clients_per_region = vec![
        // 32,
        // 64,
        // 128,
        // 256,
        // 512, 768, 1024,
        // 1280,
        // 1536,
        // 1792,
        2048,
        // 2560,
        // 3072,
        // 3584,
        // 4096,
        // 1024 * 4,
        // 1024 * 8,
        // 1024 * 12,
        // 1024 * 16,
        // 1024 * 32,
    ];
    let batch_max_sizes = vec![1];

    let shard_count = 1;
    let keys_per_command = 1;
    let payload_size = 100;
    let cpus = 2;

    let coefficients = vec![
        // 0.5, 0.75, 1.0,
        // 1.25, 1.5, 1.75,
        2.0, 2.5, 3.0, 3.5, 4.0, 5.0, 6.0, 7.0, 8.0, 9.0, 10.0,
    ];
    let key_gens = coefficients.into_iter().map(|coefficient| KeyGen::Zipf {
        total_keys_per_shard: 1_000_000,
        coefficient,
    });

    let mut workloads = Vec::new();
    for key_gen in key_gens {
        let workload = Workload::new(
            shard_count,
            key_gen,
            keys_per_command,
            COMMANDS_PER_CLIENT_LAN,
            payload_size,
        );
        workloads.push(workload);
    }

    let skip = |protocol, _, clients| {
        // skip Atlas with more than 4096 clients
        protocol == Protocol::AtlasLocked && clients > 1024 * 20
    };

    /*
    // MULTI_KEY
    let configs = vec![
        // (protocol, (n, f, tiny quorums, clock bump interval, skip fast ack))
        (Protocol::TempoAtomic, config!(n, 1, false, None, false)),
        (Protocol::TempoAtomic, config!(n, 2, false, None, false)),
        (Protocol::TempoLocked, config!(n, 1, false, None, false)),
        (Protocol::TempoLocked, config!(n, 2, false, None, false)),
    ];

    let clients_per_region =
        vec![256, 1024, 1024 * 4, 1024 * 8, 1024 * 16, 1024 * 32];
    let zipf_key_count = 1_000_000;

    let skip = |_, _, _| false;
    */

    /*
    // PARTIAL REPLICATION
    let regions = vec![
        Region::EuWest1,
        Region::UsWest1,
        Region::ApSoutheast1,
    ];

    let n = regions.len();
    let mut configs = vec![
        // (protocol, (n, f, tiny quorums, clock bump interval, skip fast ack))
        // (Protocol::AtlasLocked, config!(n, 1, false, None, false)),
        (Protocol::TempoAtomic, config!(n, 1, false, None, false)),
    ];

    let clients_per_region = vec![
        // 1024 / 4,
        // 1024 / 2,
        1024,
        // 1024 * 2,
        1024 * 4,
        1024 * 8,
        // 1024 * 12,
        1024 * 16,
        // 1024 * 20,
        // 1024 * 24,
        1024 * 32,
        // 1024 * 36,
        // 1024 * 40,
        1024 * 48,
        // 1024 * 56,
        1024 * 64,
        // 1024 * 96,
        // 1024 * 128,
        // 1024 * 160,
        // 1024 * 192,
        // 1024 * 224,
        // 1024 * 240,
        // 1024 * 256,
        // 1024 * 272,
    ];
    let clients_per_region = vec![
        // 1024,
        // 1024 * 2,
        // 1024 * 4,
        // 1024 * 6,
        // 1024 * 8,
        // 1024 * 12,
        // 1024 * 16,
        // 1024 * 20,
        1024 * 24,
    ];
    let clients_per_region = vec![
        1024,
        1024 * 4,
        1024 * 8,
        1024 * 16,
        1024 * 32,
        1024 * 48,
        1024 * 64,
    ];
    let shard_count = 5;
    let keys_per_shard = 1_000_000;
    let key_gen = KeyGen::Zipf {
        coefficient: 128.0,
        keys_per_shard,
    };
    let keys_per_command = 2;
    let payload_size = 0;

    let mut workloads = Vec::new();
    let workload = Workload::new(
        shard_count,
        key_gen,
        keys_per_command,
        COMMANDS_PER_CLIENT,
        payload_size,
    );
    workloads.push(workload);

    let skip = |_, _, _| false;
    */

    // set shards in each config
    configs
        .iter_mut()
        .for_each(|(_protocol, config)| config.set_shard_count(shard_count));

    // init logging
    let progress = TracingProgressBar::init(
        (workloads.len()
            * clients_per_region.len()
            * configs.len()
            * batch_max_sizes.len()) as u64,
    );

    // create AWS planet
    // let planet = Some(Planet::from("../latency_aws"));
    let planet = None; // if delay is not to be injected

    baremetal_bench(
        regions,
        shard_count,
        planet,
        configs,
        clients_per_region,
        workloads,
        batch_max_sizes,
        cpus,
        skip,
        progress,
        results_dir,
    )
    .await
    /*
    local_bench(
        regions,
        shard_count,
        planet,
        configs,
        clients_per_region,
        workloads,
        batch_max_size,
        skip,
        progress,
    )
    .await
    aws_bench(
        regions,
        shard_count,
        configs,
        clients_per_region,
        workloads,
        batch_max_size,
        skip,
        progress,
    ).await
    */
}

#[allow(dead_code)]
async fn local_bench(
    regions: Vec<Region>,
    shard_count: usize,
    planet: Option<Planet>,
    configs: Vec<(Protocol, Config)>,
    clients_per_region: Vec<usize>,
    workloads: Vec<Workload>,
    batch_max_sizes: Vec<usize>,
    cpus: usize,
    skip: impl Fn(Protocol, Config, usize) -> bool,
    progress: TracingProgressBar,
    results_dir: impl AsRef<Path>,
) -> Result<(), Report>
where
{
    // setup baremetal machines
    let machines = fantoch_exp::testbed::local::setup(
        regions,
        shard_count,
        BRANCH.to_string(),
        RUN_MODE,
        all_features(),
    )
    .await
    .wrap_err("local spawn")?;

    // run benchmarks
    run_bench(
        machines,
        Testbed::Local,
        planet,
        configs,
        clients_per_region,
        workloads,
        batch_max_sizes,
        cpus,
        skip,
        progress,
        results_dir,
    )
    .await
    .wrap_err("run bench")?;

    Ok(())
}

#[allow(dead_code)]
async fn baremetal_bench(
    regions: Vec<Region>,
    shard_count: usize,
    planet: Option<Planet>,
    configs: Vec<(Protocol, Config)>,
    clients_per_region: Vec<usize>,
    workloads: Vec<Workload>,
    batch_max_sizes: Vec<usize>,
    cpus: usize,
    skip: impl Fn(Protocol, Config, usize) -> bool,
    progress: TracingProgressBar,
    results_dir: impl AsRef<Path>,
) -> Result<(), Report>
where
{
    // create launcher
    let mut launchers = fantoch_exp::testbed::baremetal::create_launchers(
        &regions,
        shard_count,
    );

    // setup baremetal machines
    let machines = fantoch_exp::testbed::baremetal::setup(
        &mut launchers,
        regions,
        shard_count,
        BRANCH.to_string(),
        RUN_MODE,
        all_features(),
    )
    .await
    .wrap_err("baremetal spawn")?;

    // run benchmarks
    run_bench(
        machines,
        Testbed::Baremetal,
        planet,
        configs,
        clients_per_region,
        workloads,
        batch_max_sizes,
        cpus,
        skip,
        progress,
        results_dir,
    )
    .await
    .wrap_err("run bench")?;

    Ok(())
}

#[allow(dead_code)]
async fn aws_bench(
    regions: Vec<Region>,
    shard_count: usize,
    configs: Vec<(Protocol, Config)>,
    clients_per_region: Vec<usize>,
    workloads: Vec<Workload>,
    batch_max_sizes: Vec<usize>,
    cpus: usize,
    skip: impl Fn(Protocol, Config, usize) -> bool,
    progress: TracingProgressBar,
    results_dir: impl AsRef<Path>,
) -> Result<(), Report> {
    let mut launcher: tsunami::providers::aws::Launcher<_> = Default::default();
    let res = do_aws_bench(
        &mut launcher,
        regions,
        shard_count,
        configs,
        clients_per_region,
        workloads,
        batch_max_sizes,
        cpus,
        skip,
        progress,
        results_dir,
    )
    .await;

    // trap errors to make sure there's time for a debug
    if let Err(e) = &res {
        tracing::warn!("aws bench experiment error: {:?}", e);
    }
    tracing::info!("will wait 5 minutes before terminating spot instances");
    tokio::time::sleep(tokio::time::Duration::from_secs(60 * 5)).await;

    launcher.terminate_all().await?;
    Ok(())
}

async fn do_aws_bench(
    launcher: &mut tsunami::providers::aws::Launcher<
        rusoto_credential::DefaultCredentialsProvider,
    >,
    regions: Vec<Region>,
    shard_count: usize,
    configs: Vec<(Protocol, Config)>,
    clients_per_region: Vec<usize>,
    workloads: Vec<Workload>,
    batch_max_sizes: Vec<usize>,
    cpus: usize,
    skip: impl Fn(Protocol, Config, usize) -> bool,
    progress: TracingProgressBar,
    results_dir: impl AsRef<Path>,
) -> Result<(), Report> {
    // setup aws machines
    let machines = fantoch_exp::testbed::aws::setup(
        launcher,
        LAUCH_MODE,
        regions,
        shard_count,
        SERVER_INSTANCE_TYPE.to_string(),
        CLIENT_INSTANCE_TYPE.to_string(),
        MAX_SPOT_INSTANCE_REQUEST_WAIT_SECS,
        BRANCH.to_string(),
        RUN_MODE,
        all_features(),
    )
    .await
    .wrap_err("aws spawn")?;

    // no need for aws planet
    let planet = None;

    // run benchmarks
    run_bench(
        machines,
        Testbed::Aws,
        planet,
        configs,
        clients_per_region,
        workloads,
        batch_max_sizes,
        cpus,
        skip,
        progress,
        results_dir,
    )
    .await
    .wrap_err("run bench")?;

    Ok(())
}

async fn run_bench(
    machines: Machines<'_>,
    testbed: Testbed,
    planet: Option<Planet>,
    configs: Vec<(Protocol, Config)>,
    clients_per_region: Vec<usize>,
    workloads: Vec<Workload>,
    batch_max_sizes: Vec<usize>,
    cpus: usize,
    skip: impl Fn(Protocol, Config, usize) -> bool,
    progress: TracingProgressBar,
    results_dir: impl AsRef<Path>,
) -> Result<(), Report> {
    fantoch_exp::bench::bench_experiment(
        machines,
        RUN_MODE,
        &MAX_LEVEL_RUN_TIME,
        all_features(),
        testbed,
        planet,
        configs,
        clients_per_region,
        workloads,
        batch_max_sizes,
        BATCH_MAX_DELAY,
        cpus,
        skip,
        EXPERIMENT_TIMEOUTS,
        PROTOCOLS_TO_CLEANUP.to_vec(),
        progress,
        results_dir,
    )
    .await
}

fn all_features() -> Vec<FantochFeature> {
    let mut features = FEATURES.to_vec();
    if let Some(feature) = FantochFeature::max_level(&MAX_LEVEL_COMPILE_TIME) {
        features.push(feature);
    }
    features
}
