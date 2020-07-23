use super::Nickname;
use crate::exp::{self, Machines};
use crate::util;
use crate::{FantochFeature, RunMode, Testbed};
use color_eyre::eyre::WrapErr;
use color_eyre::Report;
use std::collections::HashMap;

const MACHINES: &str = "exp_files/machines";
const PRIVATE_KEY: &str = "~/.ssh/id_rsa";

pub async fn setup<'a>(
    launchers: &'a mut Vec<tsunami::providers::baremetal::Machine>,
    regions: Vec<rusoto_core::Region>,
    process_count: usize,
    shard_count: usize,
    client_count: usize,
    branch: String,
    run_mode: RunMode,
    features: Vec<FantochFeature>,
) -> Result<Machines<'a>, Report> {
    let machine_count = process_count * shard_count + client_count;

    // get ips and check that we have enough of them
    let content = tokio::fs::read_to_string(MACHINES).await?;
    let machines: Vec<_> = content.lines().take(machine_count).collect();
    assert_eq!(machines.len(), machine_count, "not enough machines");

    // create nicknames for all machines
    let nicknames = super::create_nicknames(shard_count, &regions);

    // get machine and launcher iterators
    let mut machines_iter = machines.into_iter();
    let mut launcher_iter = launchers.iter_mut();

    // setup machines
    let mut launches = Vec::with_capacity(machine_count);
    for nickname in nicknames {
        // find one machine and a launcher for this machine
        let machine = machines_iter.next().unwrap();
        let launcher = launcher_iter.next().unwrap();

        // create baremetal setup
        let setup = baremetal_setup(
            machine,
            branch.clone(),
            run_mode,
            features.clone(),
        )
        .await
        .wrap_err("baremetal setup")?;

        // save baremetal launch
        let launch = baremetal_launch(launcher, nickname, setup);
        launches.push(launch);
    }

    // create placement, servers, and clients
    let placement = super::create_placement(shard_count, regions);
    let mut servers = HashMap::with_capacity(process_count);
    let mut clients = HashMap::with_capacity(client_count);

    for result in futures::future::join_all(launches).await {
        let vm = result.wrap_err("baremetal launch")?;
        let Nickname { region, shard_id } = Nickname::from_string(&vm.nickname);

        let unique_insert = match shard_id {
            Some(shard_id) => {
                // it's a server; find it's process id
                let (process_id, _region_index) =
                    placement.get(&(region, shard_id)).expect(
                        "pair region and shard id should exist in placement",
                    );
                servers.insert(*process_id, vm).is_none()
            }
            None => {
                // it's a client
                clients.insert(region, vm).is_none()
            }
        };
        assert!(unique_insert);
    }

    // check that we have enough machines
    assert_eq!(
        servers.len(),
        process_count * shard_count,
        "not enough server vms"
    );
    assert_eq!(clients.len(), client_count, "not enough client vms");

    let machines = Machines::new(placement, servers, clients);
    Ok(machines)
}

async fn baremetal_setup(
    machine: &str,
    branch: String,
    run_mode: RunMode,
    features: Vec<FantochFeature>,
) -> Result<tsunami::providers::baremetal::Setup, Report> {
    let parts: Vec<_> = machine.split('@').collect();
    assert_eq!(parts.len(), 2, "machine should have the form username@addr");
    let username = parts[0].to_string();
    let hostname = parts[1].to_string();

    // fetch public ip
    let command = String::from("hostname -I");
    let ips = util::exec(
        &username,
        &hostname,
        &std::path::PathBuf::from(PRIVATE_KEY),
        command,
    )
    .await
    .wrap_err("hostname -I")?;

    // hostname should return at least one ip like so "10.10.5.61 172.17.0.1"
    let parts: Vec<_> = ips.split(' ').collect();
    let ip = parts[0];

    // append ssh port
    // - TODO: I think this should be fixed in tsunami, not here
    let addr = format!("{}:22", ip);

    let setup =
        tsunami::providers::baremetal::Setup::new(addr, Some(username))?
            .key_path(PRIVATE_KEY)
            .setup(exp::fantoch_setup(
                branch,
                run_mode,
                features,
                Testbed::Baremetal,
            ));
    Ok(setup)
}

async fn baremetal_launch(
    launcher: &mut tsunami::providers::baremetal::Machine,
    nickname: Nickname,
    setup: tsunami::providers::baremetal::Setup,
) -> Result<tsunami::Machine<'_>, Report> {
    // get region and nickname
    let region = nickname.region.name().clone();
    let nickname = nickname.to_string();

    // create launch descriptor
    let descriptor = tsunami::providers::LaunchDescriptor {
        region,
        max_wait: None,
        machines: vec![(nickname.clone(), setup)],
    };

    // do launch the machine
    use tsunami::providers::Launcher;
    launcher.launch(descriptor).await?;
    let mut machines = launcher.connect_all().await?;

    // check that a single machine was returned
    assert_eq!(
        machines.len(),
        1,
        "baremetal launched didn't return a single machine"
    );
    assert!(
        machines.contains_key(&nickname),
        "baremetal machines incorrectly identified"
    );
    // fetch the machine
    let machine = machines.remove(&nickname).unwrap();
    assert_eq!(
        machine.nickname, nickname,
        "baremetal machine has the wrong nickname"
    );
    Ok(machine)
}
