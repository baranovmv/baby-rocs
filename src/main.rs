// src/main.rs
use anyhow::{Context, Result, anyhow, bail};
use clap::{Parser, ValueEnum};
use cpal::traits::{DeviceTrait, HostTrait, StreamTrait};
use cpal::{SampleFormat, SupportedStreamConfigRange};
use std::fs::File;
use std::io::BufWriter;
use std::path::{Path, PathBuf};
use std::sync::{mpsc, Mutex, Arc};

const REQUIRED_SAMPLE_RATE: u32 = 48_000;
const REQUIRED_CHANNELS: u16 = 1;
const REQUIRED_FORMAT: SampleFormat = SampleFormat::F32;

type WavWriterHandle = Arc<Mutex<Option<hound::WavWriter<BufWriter<File>>>>>;

#[derive(Copy, Clone, Debug, PartialEq, Eq, ValueEnum)]
enum Direction {
    Input,
    Output,
}

#[derive(Parser, Debug)]
#[command(about = "Open a 48 kHz / mono / f32 cpal stream on a named ALSA device")]
struct Args {
    /// ALSA device designator as shown by `aplay -L` / `arecord -L`
    /// (e.g. "default", "plughw:CARD=UA25,DEV=0", "hw:1,0", "pipewire").
    #[arg(short, long)]
    device: String,

    /// List available devices and exit.
    #[arg(long)]
    list: bool,

    /// Stream direction.
    #[arg(long, value_enum, default_value_t = Direction::Input)]
    direction: Direction,

    #[arg(long)]
    wav: PathBuf,
}

fn main() -> Result<()> {
    let args = Args::parse();
    let host = cpal::default_host();

    if args.list {
        return list_devices(&host);
    }

    let device = find_device(&host, &args.device, args.direction)
        .with_context(|| format!("resolving device `{}`", args.device))?;

    let dev_name = device.name().unwrap_or_else(|_| "<unnamed>".into());
    println!("Using device: {dev_name}");

    // Check the device actually supports 48 kHz / mono / f32.
    let config = pick_required_config(&device, args.direction)
        .with_context(|| format!("device `{dev_name}` cannot provide 48 kHz mono f32"))?;

    println!("Stream config: {config:?}");

    let (tx, rx) = mpsc::channel::<Result<()>>();

    let Ok(wav_writer) = wav_writer(&args.wav) else {
        eprintln!("Can't open wav file");
        return Ok(());
    };
    let wav_writer = Arc::new(Mutex::new(Some(wav_writer)));
    let stream = build_stream(&device, &config, args.direction, wav_writer, tx.clone())?;
    stream.play().context("starting stream")?;

    ctrlc::set_handler({
        let tx = tx.clone();
        move || {
            let _ = tx.send(Ok(()));
        }
    })
    .ok();

    match rx.recv() {
        Ok(Ok(())) => println!("Shutting down."),
        Ok(Err(e)) => eprintln!("Stream error: {e:?}"),
        Err(_) => {}
    }

    drop(stream);
    Ok(())
}

fn wav_writer(path: &Path) -> Result<hound::WavWriter<BufWriter<File>>> {
    let config = hound::WavSpec {
        channels: REQUIRED_CHANNELS,
        sample_rate: REQUIRED_SAMPLE_RATE,
        bits_per_sample: 32,
        sample_format: hound::SampleFormat::Float,
    };
    let writer = hound::WavWriter::create(path, config)?;
    return Ok(writer);
}

fn find_device<H: HostTrait>(host: &H, designator: &str, direction: Direction) -> Result<H::Device>
where
    H::Device: DeviceTrait,
{
    if designator == "default" {
        return match direction {
            Direction::Input => host
                .default_input_device()
                .ok_or_else(|| anyhow!("no default input device")),
            Direction::Output => host
                .default_output_device()
                .ok_or_else(|| anyhow!("no default output device")),
        };
    }

    let mut candidates: Box<dyn Iterator<Item = H::Device>> = match direction {
        Direction::Input => Box::new(host.input_devices()?),
        Direction::Output => Box::new(host.output_devices()?),
    };

    candidates
        .find(|d| {
            let id_match = d.id().map(|i| i.to_string() == designator).unwrap_or(false);
            let name_match = d.name().map(|n| n == designator).unwrap_or(false);
            id_match || name_match
        })
        .ok_or_else(|| anyhow!("no {:?} device matches `{}`", direction, designator))
}

/// Verify the device supports exactly 48 kHz / mono / f32 and return a concrete
/// StreamConfig for it. Emits a descriptive error otherwise.
fn pick_required_config<D: DeviceTrait>(
    device: &D,
    direction: Direction,
) -> Result<cpal::StreamConfig> {
    let supported: Vec<SupportedStreamConfigRange> = match direction {
        Direction::Input => device.supported_input_configs()?.collect(),
        Direction::Output => device.supported_output_configs()?.collect(),
    };

    let sr = REQUIRED_SAMPLE_RATE;

    let matched = supported.iter().find(|r| {
        r.channels() == REQUIRED_CHANNELS
            && r.sample_format() == REQUIRED_FORMAT
            && r.min_sample_rate() <= sr
            && r.max_sample_rate() >= sr
    });

    match matched {
        Some(range) => {
            let cfg = range.clone().with_sample_rate(sr).config();
            Ok(cfg)
        }
        None => {
            // Build a readable summary of what the device *does* support to help
            // the user diagnose the mismatch.
            let summary: Vec<String> = supported
                .iter()
                .map(|r| {
                    format!(
                        "channels={}, format={:?}, sample_rate={}..={}",
                        r.channels(),
                        r.sample_format(),
                        r.min_sample_rate(),
                        r.max_sample_rate(),
                    )
                })
                .collect();

            bail!(
                "device does not support the required format \
                 (channels={REQUIRED_CHANNELS}, sample_rate={REQUIRED_SAMPLE_RATE}, \
                 format={REQUIRED_FORMAT:?}).\nSupported configs:\n  {}",
                summary.join("\n  ")
            );
        }
    }
}

fn build_stream<D: DeviceTrait>(
    device: &D,
    cfg: &cpal::StreamConfig,
    direction: Direction,
    wav_writer: WavWriterHandle,
    err_tx: mpsc::Sender<Result<()>>,
) -> Result<D::Stream> {
    let err_fn = move |e: cpal::StreamError| {
        let _ = err_tx.send(Err(anyhow!(e)));
    };

    let stream = match direction {
        Direction::Output => device.build_output_stream(
            cfg,
            |data: &mut [f32], _: &cpal::OutputCallbackInfo| {
                for s in data.iter_mut() {
                    *s = 0.0;
                }
            },
            err_fn,
            None,
        )?,
        Direction::Input => device.build_input_stream(
            cfg,
            move |_data: &[f32], _: &cpal::InputCallbackInfo| {
                process_input (_data, &wav_writer);
            },
            err_fn,
            None,
        )?,
    };

    Ok(stream)
}

fn list_devices<H: HostTrait>(host: &H) -> Result<()>
where
    H::Device: DeviceTrait,
{
    println!("Input devices:");
    for d in host.input_devices()? {
        println!(
            "  name={:?} id={:?}",
            d.name().ok(),
            d.id().ok().map(|i| i.to_string())
        );
    }
    println!("Output devices:");
    for d in host.output_devices()? {
        println!(
            "  name={:?} id={:?}",
            d.name().ok(),
            d.id().ok().map(|i| i.to_string())
        );
    }
    Ok(())
}

fn process_input(x: &[f32], wav_writer: &WavWriterHandle) {
    if let Ok(mut guard) = wav_writer.try_lock() {
        if let Some(writer) = guard.as_mut() {
            for sample in x.iter() {
                writer.write_sample(*sample);
            }
        }
    }
}
