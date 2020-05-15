mod common;

use fantoch_ps::protocol::AtlasSequential;
use std::error::Error;

// TODO can we generate all the protocol binaries with a macro?

fn main() -> Result<(), Box<dyn Error>> {
    common::protocol::run::<AtlasSequential>()
}
