mod common;

use fantoch_ps::protocol::AtlasLocked;
use std::error::Error;

// TODO can we generate all the protocol binaries with a macro?

fn main() -> Result<(), Box<dyn Error>> {
    let (
        process_id,
        sorted_processes,
        ip,
        port,
        client_port,
        addresses,
        config,
        tcp_nodelay,
        tcp_buffer_size,
        tcp_flush_interval,
        channel_buffer_size,
        multiplexing,
        execution_log,
    ) = common::protocol::parse_args();

    // create process
    let process = fantoch::run::process::<AtlasLocked, String>(
        process_id,
        sorted_processes,
        ip,
        port,
        client_port,
        addresses,
        config,
        tcp_nodelay,
        tcp_buffer_size,
        tcp_flush_interval,
        channel_buffer_size,
        multiplexing,
        execution_log,
    );
    common::tokio_runtime().block_on(process)
}
