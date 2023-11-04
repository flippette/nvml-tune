use clap::Parser;
use env_logger::fmt::Formatter;
use eyre::{bail, ensure, eyre, Result};
use log::{error, info, Level, LevelFilter, Record};
use nvml_wrapper_sys::bindings::*;
use owo_colors::{AnsiColors, OwoColorize};
use std::{
    alloc::{alloc, Layout},
    io::{self, Write},
};

fn main() -> Result<()> {
    color_eyre::install()?;
    pretty_env_logger::formatted_builder()
        .filter_level(LevelFilter::Info)
        .parse_default_env()
        .format(log_fmt)
        .init();
    sudo::escalate_if_needed().map_err(|_| eyre!("failed to elevate privileges!"))?;

    let args = Args::parse();

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
            "failed to get device at index {}! (error {val})",
            args.index
        ),
    }

    if let Some(tdp) = args.tdp {
        match unsafe { lib.nvmlDeviceSetPowerManagementLimit(*device, tdp * 1000) } {
            0 => info!("set tdp to {tdp}W!"),
            val => error!("failed to set tdp! (error = {val})"),
        }
    }

    if let Some(mem_clock) = args.mem_clock {
        match unsafe { lib.nvmlDeviceSetMemClkVfOffset(*device, mem_clock as i32 * 2) } {
            0 => info!("set memory clock offset to +{mem_clock}MHz!"),
            val => error!("failed to set memory clock offset! (error {val})"),
        }
    }

    if let Some(gfx_clock) = args.gfx_clock {
        match unsafe { lib.nvmlDeviceSetGpuLockedClocks(*device, gfx_clock, gfx_clock) } {
            0 => info!("set graphics clock to {gfx_clock}MHz!"),
            val => error!("failed to set graphics clock! (error = {val})"),
        }
    }

    Ok(())
}

fn log_fmt(fmt: &mut Formatter, rec: &Record) -> io::Result<()> {
    writeln!(
        fmt,
        "{} > {}",
        rec.target().bold().color(match rec.level() {
            Level::Trace => AnsiColors::White,
            Level::Debug => AnsiColors::Cyan,
            Level::Info => AnsiColors::Green,
            Level::Warn => AnsiColors::Yellow,
            Level::Error => AnsiColors::Red,
        }),
        rec.args()
    )
}

#[derive(Debug, Parser)]
#[command(author, version, about, long_about = None)]
struct Args {
    #[arg(short, long, default_value_t = 0)]
    index: u32,

    #[arg(short, long)]
    tdp: Option<u32>,

    #[arg(short, long)]
    mem_clock: Option<u32>,

    #[arg(short, long)]
    gfx_clock: Option<u32>,
}
