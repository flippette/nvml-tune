use clap::Parser;
use eyre::{bail, eyre, Result};
use nom::IResult;
use nvml_wrapper_sys::bindings::*;
use std::{fs::File, io, mem::MaybeUninit, path::PathBuf, sync::mpsc, thread, time::Duration};
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

    let mut device = MaybeUninit::uninit();
    match unsafe { lib.nvmlDeviceGetHandleByIndex_v2(args.index, device.as_mut_ptr()) } {
        0 => info!("got device at index {}! (addr = {:p})", args.index, &device),
        val => bail!(
            "failed to get device at index {}! (error = {val})",
            args.index
        ),
    }
    let device = unsafe { device.assume_init() };

    if let Some(tdp) = args.tdp {
        match unsafe { lib.nvmlDeviceSetPowerManagementLimit(device, tdp * 1000) } {
            0 => info!("set tdp to {tdp}W!"),
            val => error!("failed to set tdp! (error = {val})"),
        }
    }

    if let Some(mem_clock) = args.mclk_offset {
        match unsafe { lib.nvmlDeviceSetMemClkVfOffset(device, mem_clock * 2) } {
            0 => info!("set memory clock offset to +{mem_clock}MHz!"),
            val => error!("failed to set memory clock offset! (error = {val})"),
        }
    }

    if let Some(gfx_clock) = args.gclk_offset {
        match unsafe { lib.nvmlDeviceSetGpcClkVfOffset(device, gfx_clock) } {
            0 => info!("set graphics clock offset to +{gfx_clock}MHz!"),
            val => error!("failed to set graphics clock! (error = {val})"),
        }
    }

    if let Some(fan_curve) = args.fan_curve {
        let (tx, rx) = mpsc::channel();
        ctrlc::set_handler(move || tx.send(()).unwrap())?;

        if fan_curve.len() == 1 && fan_curve[0].0 > 0 {
            error!("single point fan curve must have a 0c point!");
        } else {
            loop {
                if let Ok(()) = rx.try_recv() {
                    break;
                }

                let mut temp = 0;
                match unsafe { lib.nvmlDeviceGetTemperature(device, 0, &mut temp) } {
                    0 => info!("read current temperature! ({temp}c)"),
                    val => error!("failed to read current temperature! (error = {val})"),
                }

                // find neighboring keypoints
                let ((temp_pre, duty_pre), (temp_post, duty_post)) = match &fan_curve[..] {
                    [point] => (*point, (100, 100)),
                    points => points
                        .windows(2)
                        .find(|window| window[0].0 < temp && window[1].0 > temp)
                        .map(|window| (window[0], window[1]))
                        .unwrap_or(((0, 0), (100, 100))),
                };

                let slope = (duty_post + duty_pre) as f64 / (temp_post + temp_pre) as f64;
                let duty = (temp as f64 * slope) as u32;
                match unsafe { lib.nvmlDeviceSetFanSpeed_v2(device, 0, duty) } {
                    0 => info!("set fan duty to {duty}%!"),
                    val => error!("failed to set fan duty! (error = {val})"),
                }

                thread::sleep(Duration::from_secs(args.fan_update_duration));
            }
        }
    }

    unsafe {
        lib.nvmlShutdown();
    }

    Ok(())
}

#[derive(Debug, Parser)]
#[command(author, version, about, long_about = None)]
struct Args {
    /// the index of the gpu
    #[arg(short, long, default_value_t = 0)]
    index: u32,

    /// tdp
    #[arg(short, long, value_name = "W")]
    tdp: Option<u32>,

    /// memory clock offset, can be negative
    #[arg(short, long, value_name = "MHZ", allow_negative_numbers = true)]
    mclk_offset: Option<i32>,

    /// graphics clock offset, can be negative
    #[arg(short, long, value_name = "MHZ", allow_negative_numbers = true)]
    gclk_offset: Option<i32>,

    /// fan speed curve in comma-separated (temp:duty) pairs
    #[arg(short = 'c', long, value_name = "(CEL:PERCENT),", value_parser = parse_fan_curve)]
    fan_curve: Option<std::vec::Vec<(u32, u32)>>, // see clap issue #4481

    /// how long to sleep in between fan speed changes
    #[arg(short = 'r', long, value_name = "(SECS)", default_value_t = 2)]
    fan_update_duration: u64,

    /// logfile location
    #[arg(short, long, default_value = "nvml-tune.log")]
    logfile: PathBuf,
}

fn parse_fan_curve(i: &str) -> Result<Vec<(u32, u32)>, clap::Error> {
    use nom::{branch::*, bytes::complete::*, sequence::*};

    fn parse_pair(i: &str) -> IResult<&str, (u32, u32)> {
        let (i, _) = tag("(")(i)?;
        let (i, (temp, duty)) = separated_pair(
            take_while(|c: char| c.is_ascii_digit()),
            tag(":"),
            take_while(|c: char| c.is_ascii_digit()),
        )(i)?;
        let (i, _) = tag(")")(i)?;

        let temp = temp.parse::<u32>().unwrap();
        if temp > 100 {
            return Err(nom::Err::Error(nom::error::Error::new(
                i,
                nom::error::ErrorKind::Digit,
            )));
        }
        let duty = duty.parse::<u32>().unwrap();
        if duty > 100 {
            return Err(nom::Err::Error(nom::error::Error::new(
                i,
                nom::error::ErrorKind::Digit,
            )));
        }

        Ok((i, (temp, duty)))
    }

    let mut curve = Vec::new();
    let mut i = i;

    while let Ok((i_next, point)) = alt((terminated(parse_pair, tag(",")), parse_pair))(i) {
        i = i_next;

        if let Some(idx) = curve.iter().position(|(temp, _)| *temp == point.0) {
            if point.1 < curve[idx].1 {
                continue;
            }
            curve.remove(idx);
        }
        curve.push(point);
    }

    if curve.is_empty() {
        return Err(clap::Error::raw(
            clap::error::ErrorKind::InvalidValue,
            "fan curve must not be empty!",
        ));
    }
    curve.sort_by_key(|(temp, _)| *temp);

    if curve[0].0 != 0 {
        curve.insert(0, (0, 0));
    }

    if curve.last().is_some_and(|(temp, _)| *temp < 100) {
        curve.push((100, 100));
    }

    Ok(curve)
}
