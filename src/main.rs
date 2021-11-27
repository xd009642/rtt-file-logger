use probe_rs::Probe;
use probe_rs_rtt::{Rtt, UpChannel};
use serde::Deserialize;
use std::fs;
use std::io::prelude::*;
use std::path::PathBuf;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::Arc;
use structopt::StructOpt;
use tracing::{debug, info, trace};
use tracing_subscriber::prelude::*;
use tracing_subscriber::{fmt, EnvFilter};

#[derive(Debug, Clone, StructOpt)]
pub struct Args {
    /// Index of the core to attach to
    #[structopt(long, default_value = "0")]
    core: usize,
    /// name of the chip
    #[structopt(long)]
    chip: String,
    /// Index of the probe to use
    #[structopt(long, default_value = "0")]
    probe: usize,
    /// A toml file specifying the configuration
    #[structopt(short, long)]
    config: Option<PathBuf>,
}

#[derive(Debug, Clone, Deserialize)]
pub struct Config {
    #[serde(rename = "default.rtt_file")]
    rtt_config: RttConfig,
}

#[derive(Debug, Clone, Deserialize)]
pub struct RttConfig {
    channels: Vec<Channel>,
}

#[derive(Debug, Clone, Deserialize)]
pub struct Channel {
    up: usize,
    name: String,
    path: PathBuf,
}

#[derive(Debug)]
pub struct ChannelSink {
    channel: UpChannel,
    name: String,
    file: fs::File,
    working: bool,
}

fn setup_tracing() {
    let fmt_layer = fmt::layer().with_target(false);
    let filter_layer = EnvFilter::try_from_default_env()
        .or_else(|_| EnvFilter::try_new("rtt_file_logger=trace"))
        .unwrap();

    tracing_subscriber::registry()
        .with(filter_layer)
        .with(fmt_layer)
        .init();
}

fn main() -> Result<(), Box<dyn std::error::Error>> {
    setup_tracing();

    let args = Args::from_args();
    info!("Getting probe: {}", args.probe);
    let probe = Probe::list_all()[args.probe].open()?;
    info!("Attaching to chip: {}", args.chip);
    let mut session = probe.attach(&args.chip)?;
    let memory_map = session.target().memory_map.clone();

    info!("Getting core: {}", args.core);
    let mut core = session.core(args.core)?;

    info!("Attaching via RTT");
    let mut rtt = Rtt::attach(&mut core, &memory_map)?;

    // Get channels dump to file
    let config_file = args.config.unwrap_or(PathBuf::from("Embed.toml"));

    info!("Reading configuration file");
    let config_toml = fs::read_to_string(config_file)?;

    info!("Deserializing config");
    let config: Config = toml::from_str(&config_toml)?;

    let mut sinks: Vec<ChannelSink> = config
        .rtt_config
        .channels
        .iter()
        .map(|x| {
            let channel = rtt.up_channels().take(x.up).expect("Channel missing");
            ChannelSink {
                channel,
                name: x.name.clone(),
                file: fs::File::create(&x.path).expect("Couldn't create output file"),
                working: true,
            }
        })
        .collect();

    debug!("Got sinks: {:?}", sinks);

    let mut buffer = [0u8; 1024];

    let running = Arc::new(AtomicBool::new(true));
    let r = running.clone();

    ctrlc::set_handler(move || {
        info!("Closing down dumper");
        r.store(false, Ordering::SeqCst);
    })
    .expect("Error setting Ctrl-C handler");

    while running.load(Ordering::SeqCst) {
        // To do move this into some sort of poll function
        for sink in &mut sinks {
            if !sink.working {
                continue;
            }
            let res = sink.channel.read(&mut core, &mut buffer[..]);
            match res {
                Ok(bytes) if bytes > 0 => {
                    trace!("Received data writing {} bytes from {}", bytes, sink.name);
                    if let Err(e) = sink.file.write_all(&buffer[..bytes]) {
                        println!("Failed to write data from {}: {}", sink.name, e);
                        sink.working = false;
                    }
                }
                Err(e) => {
                    println!("Channel error: {}", e);
                }
                Ok(_) => {}
            }
        }
    }
    info!("Closed");

    Ok(())
}
