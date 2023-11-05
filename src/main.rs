use clap::Parser;
use eyre::{bail, ensure, eyre, Result};
use nvml_wrapper_sys::bindings::*;
use std::{
    alloc::{alloc, Layout},
    fs::File,
    io,
    path::PathBuf,
};
use sudo::RunningAs;
use tracing::{error, info, Level};
use tracing_subscriber::{prelude::*, EnvFilter};

fn main() -> Result<()> {
    color_eyre::install()?;

    let args = Args::parse();

    let filter_layer = EnvFilter::builder()
        .with_default_directive(Level::INFO.into())
        .from_env_lossy();
    let format_layer = tracing_subscriber::fmt::layer()
        .with_writer(io::stderr)
        .without_time();
    let logfile = match sudo::check() {
        RunningAs::User => File::create(args.logfile)?,
        _ => File::options()
            .write(true)
            .truncate(true)
            .open(args.logfile)?,
    };
    let logfile_layer = tracing_subscriber::fmt::layer()
        .with_writer(logfile)
        .without_time()
        .json();
    tracing_subscriber::registry()
        .with(filter_layer)
        .with(format_layer)
        .with(logfile_layer)
        .init();

    sudo::escalate_if_needed().map_err(|_| eyre!("failed to elevate privileges!"))?;

    let lib = unsafe { NvmlLib::new("libnvidia-ml.so")? };
    info!("loaded nvml!");

    match unsafe { lib.nvmlInit_v2() } {
        0 => info!("initialized nvml!"),
        val => bail!("failed to initialize nvml! (error {val})"),
    }

    let layout = Layout::new::<nvmlDevice_t>();
    ensure!(layout.size() > 0, "nvmlDevice_t is zero-sized!");
    let device = unsafe { alloc(layout) } as *mut nvmlDevice_t;
    match unsafe { lib.nvmlDeviceGetHandleByIndex_v2(args.index, device) } {
        0 => info!("got device at index {}! (addr = {device:p})", args.index),
        val => bail!(
            "failed to get device at index {}! (error = {val})",
            args.index
        ),
    }

    if let Some(tdp) = args.tdp {
        match unsafe { lib.nvmlDeviceSetPowerManagementLimit(*device, tdp * 1000) } {
            0 => info!("set tdp to {tdp}W!"),
            val => error!("failed to set tdp! (error = {val})"),
        }
    }

    if let Some(mem_clock) = args.mclk_offset {
        match unsafe { lib.nvmlDeviceSetMemClkVfOffset(*device, mem_clock * 2) } {
            0 => info!("set memory clock offset to +{mem_clock}MHz!"),
            val => error!("failed to set memory clock offset! (error = {val})"),
        }
    }

    if let Some(gfx_clock) = args.gclk_offset {
        match unsafe { lib.nvmlDeviceSetGpcClkVfOffset(*device, gfx_clock) } {
            0 => info!("set graphics clock offset to +{gfx_clock}MHz!"),
            val => error!("failed to set graphics clock! (error = {val})"),
        }
    }

    if let Some(fan_speed) = args.fan_speed {
        match unsafe { lib.nvmlDeviceSetFanSpeed_v2(*device, 0, fan_speed) } {
            0 => info!("set fan speed to {fan_speed}%!"),
            val => error!("failed to set fan speed! (error = {val})"),
        }
    }

    Ok(())
}

#[derive(Debug, Parser)]
#[command(author, version, about, long_about = None)]
struct Args {
    /// the index of the gpu
    #[arg(short, long, default_value_t = 0)]
    index: u32,

    /// tdp in watts
    #[arg(short, long)]
    tdp: Option<u32>,

    /// memory clock offset in mhz, can be negative
    #[arg(short, long, allow_negative_numbers = true)]
    mclk_offset: Option<i32>,

    /// graphics clock offset in mhz, can be negative
    #[arg(short, long, allow_negative_numbers = true)]
    gclk_offset: Option<i32>,

    /// fan speed in %
    #[arg(short, long)]
    fan_speed: Option<u32>,

    /// logfile location
    #[arg(short, long, default_value = "nvml-tune.log")]
    logfile: PathBuf,
}
