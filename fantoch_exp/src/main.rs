mod bench;
mod ping;
mod util;

use color_eyre::Report;
use rusoto_core::Region;

// experiment config
const INSTANCE_TYPE: &str = "c5.2xlarge";
const MAX_SPOT_INSTANCE_REQUEST_WAIT_SECS: u64 = 5 * 60; // 5 minutes
const MAX_INSTANCE_DURATION_HOURS: usize = 1;

// bench-specific config
const BRANCH: &str = "exp";

// ping-specific config
const PING_DURATION_SECS: usize = 30 * 60; // 30 minutes

#[tokio::main]
async fn main() -> Result<(), Report> {
    let args: Vec<String> = std::env::args().collect();
    assert!(args.len() <= 2, "at most one argument should be provided");
    let instance_type = if args.len() == 2 {
        &args[1]
    } else {
        INSTANCE_TYPE
    };

    // init logging
    tracing_subscriber::fmt::init();

    let server_instance_type = instance_type.to_string();
    let client_instance_type = instance_type.to_string();
    let branch = BRANCH.to_string();
    bench(server_instance_type, client_instance_type, branch).await
    // ping(instance_type).await
}

async fn bench(
    server_instance_type: String,
    client_instance_type: String,
    branch: String,
) -> Result<(), Report> {
    let regions = vec![
        Region::EuWest1,
        Region::ApSoutheast1,
        Region::CaCentral1,
        Region::ApSouth1,
        Region::UsWest1,
    ];
    let ns = vec![3];
    for n in ns {
        let regions = regions.clone().into_iter().take(n).collect();
        bench::bench_experiment(
            server_instance_type.clone(),
            client_instance_type.clone(),
            regions,
            MAX_SPOT_INSTANCE_REQUEST_WAIT_SECS,
            MAX_INSTANCE_DURATION_HOURS,
            branch.clone(),
        )
        .await?
    }
    Ok(())
}

#[allow(dead_code)]
async fn ping(instance_type: &str) -> Result<(), Report> {
    // all AWS regions
    let regions = vec![
        Region::AfSouth1,
        Region::ApEast1,
        Region::ApNortheast1,
        // Region::ApNortheast2, special-region
        Region::ApSouth1,
        Region::ApSoutheast1,
        Region::ApSoutheast2,
        Region::CaCentral1,
        Region::EuCentral1,
        Region::EuNorth1,
        Region::EuSouth1,
        Region::EuWest1,
        Region::EuWest2,
        Region::EuWest3,
        Region::MeSouth1,
        Region::SaEast1,
        Region::UsEast1,
        Region::UsEast2,
        Region::UsWest1,
        Region::UsWest2,
    ];

    ping::ping_experiment(
        regions,
        instance_type,
        MAX_SPOT_INSTANCE_REQUEST_WAIT_SECS,
        MAX_INSTANCE_DURATION_HOURS,
        PING_DURATION_SECS,
    )
    .await
}
