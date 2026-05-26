use anyhow::{anyhow, Error};
use hound::{WavIntoSamples, WavReader, WavWriter};
use regex::Regex;
use serde::{Deserialize, Serialize};
use std::{
    fs::{self, File},
    io::{BufReader, BufWriter},
    path::{Path, PathBuf},
    sync::{
        atomic::{AtomicBool, Ordering},
        Arc,
        mpsc::{*}
    },
    thread,
    time::Duration,
};
use structopt::StructOpt;
use webrtc_audio_processing::Processor;
use webrtc_audio_processing_config::Config;
use crossbeam_queue::ArrayQueue;

mod common;
use common::{deinterleave, interleave};

const AUDIO_SAMPLE_RATE: u32 = 48_000;
const AUDIO_INTERLEAVED: bool = true;

type SampleBuffer = Arc<Vec<f32>>;

#[derive(Debug, StructOpt)]
struct Args {
    /// Configuration file that stores JSON serialization of [`Option`] struct.
    #[structopt(short, long)]
    pub config_file: Option<PathBuf>,

    /// List available audio devices and exit.
    #[structopt(long)]
    pub list_devices: bool,
}

#[derive(Deserialize, Serialize, Default, Clone, Debug)]
struct CaptureOptions {
    /// Name of the audio capture device.
    device_name: String,
    /// The number of audio capture channels.
    num_channels: u16,
    /// If specified, it reads the capture stream from the WAV file instead of the device.
    source_path: Option<PathBuf>,
    /// If specified, it writes the capture stream to the WAV file before applying the processing.
    preprocess_sink_path: Option<PathBuf>,
    /// If specified, it writes the capture stream to the WAV file after applying the processing.
    postprocess_sink_path: Option<PathBuf>,
}

#[derive(Deserialize, Serialize, Default, Clone, Debug)]
struct RenderOptions {
    /// Name of the audio playback device.
    device_name: String,
    /// The number of audio playback channels.
    num_channels: u16,
    /// If specified, it plays back the audio stream from the WAV file. Otherwise, a stream of
    /// zeros are sent to the audio device.
    source_path: Option<PathBuf>,
    /// If true, the output is muted.
    #[serde(default)]
    mute: bool,
}

#[derive(Deserialize, Serialize, Default, Clone, Debug)]
struct Options {
    /// Options for audio capture / recording.
    capture: CaptureOptions,
    /// Options for audio render / playback.
    render: RenderOptions,
    /// Configurations of the audio processing pipeline.
    config: Config,
}

fn match_device(
    pa: &portaudio::PortAudio,
    device_name: Regex,
) -> Result<portaudio::DeviceIndex, Error> {
    for device in (pa.devices()?).flatten() {
        if device_name.is_match(device.1.name) {
            return Ok(device.0);
        }
    }
    Err(anyhow!("Audio device matching \"{}\" not found.", device_name))
}

fn create_stream_settings(
    pa: &portaudio::PortAudio,
    processor: &Processor,
    opt: &Options,
) -> Result<portaudio::DuplexStreamSettings<f32, f32>, Error> {
    let input_device = match_device(pa, Regex::new(&opt.capture.device_name)?)?;
    let input_device_info = &pa.device_info(input_device)?;
    let input_params = portaudio::StreamParameters::<f32>::new(
        input_device,
        opt.capture.num_channels as i32,
        AUDIO_INTERLEAVED,
        input_device_info.default_low_input_latency,
    );

    let output_device = match_device(pa, Regex::new(&opt.render.device_name)?)?;
    let output_device_info = &pa.device_info(output_device)?;
    let output_params = portaudio::StreamParameters::<f32>::new(
        output_device,
        opt.render.num_channels as i32,
        AUDIO_INTERLEAVED,
        output_device_info.default_low_output_latency,
    );

    pa.is_duplex_format_supported(input_params, output_params, f64::from(AUDIO_SAMPLE_RATE))?;

    Ok(portaudio::DuplexStreamSettings::new(
        input_params,
        output_params,
        f64::from(AUDIO_SAMPLE_RATE),
        processor.num_samples_per_frame() as u32,
    ))
}

fn open_wav_writer(path: &Path, channels: u16) -> Result<WavWriter<BufWriter<File>>, Error> {
    let sink = hound::WavWriter::<BufWriter<File>>::create(
        path,
        hound::WavSpec {
            channels,
            sample_rate: AUDIO_SAMPLE_RATE,
            bits_per_sample: 32,
            sample_format: hound::SampleFormat::Float,
        },
    )?;

    Ok(sink)
}

fn open_wav_reader(path: &Path) -> Result<WavIntoSamples<BufReader<File>, f32>, Error> {
    let reader = WavReader::<BufReader<File>>::open(path)?;
    Ok(reader.into_samples())
}

// The destination array is an interleaved audio stream.
// Returns false if there are no more entries to read from the source.
fn copy_stream(source: &mut WavIntoSamples<BufReader<File>, f32>, dest: &mut [f32]) -> bool {
    let mut dest_iter = dest.iter_mut();
    for sample in source.flatten() {
        *dest_iter.next().unwrap() = sample;
        if dest_iter.len() == 0 {
            break;
        }
    }

    let source_eof = dest_iter.len() > 0;

    // Zero-fill the remainder of the destination array if we finish consuming
    // the source.
    for sample in dest_iter {
        *sample = 0.0;
    }

    !source_eof
}

fn main() -> Result<(), Error> {
    let args = Args::from_args();

    let pa = portaudio::PortAudio::new()?;

    if args.list_devices {
        for device in (pa.devices()?).flatten() {
            let (idx, info) = device;
            println!(
                "{:?}: {:?} (in: {}, out: {})",
                idx, info.name, info.max_input_channels, info.max_output_channels
            );
        }
        println!("\nDefault input: {:?}", pa.default_input_device());
        println!("Default output: {:?}", pa.default_output_device());
        return Ok(());
    }

    let config_file = args.config_file.ok_or_else(|| anyhow!("--config-file is required"))?;
    let opt: Options = json5::from_str(&fs::read_to_string(&config_file)?)?;

    let processor = Arc::new(Processor::new(AUDIO_SAMPLE_RATE)?);

    processor.set_config(opt.config);

    let running = Arc::new(AtomicBool::new(true));

    let mut capture_source =
        if let Some(path) = &opt.capture.source_path { Some(open_wav_reader(path)?) } else { None };
    let mut capture_postprocess_sink = if let Some(path) = &opt.capture.postprocess_sink_path {
        Some(open_wav_writer(path, opt.capture.num_channels)?)
    } else {
        None
    };
    let mut render_source =
        if let Some(path) = &opt.render.source_path { Some(open_wav_reader(path)?) } else { None };

    let buffer_pool = Arc::new(ArrayQueue::<Arc<Vec::<f32>>>::new(64));
    for _ in 0..64 {
        buffer_pool.push(Arc::new(
            vec![0f32; processor.num_samples_per_frame() * opt.capture.num_channels as usize]
        )
        );
    }
    let (worker_in_tx, worker_in_rx) = channel();
    let (worker_out_tx, worker_out_rx) = channel();
    let buffer_pool_wrkr = buffer_pool.clone();

    let preprocess_sink_path = opt.capture.preprocess_sink_path.clone();
    let worker_thr = thread::spawn(move || {
        worker_thread(worker_in_rx, worker_out_tx, &preprocess_sink_path, buffer_pool_wrkr);
    });

    let audio_callback = {
        // Allocate buffers outside the performance-sensitive audio loop.
        let mut input_deinterleaved =
            vec![vec![0f32; processor.num_samples_per_frame()]; opt.capture.num_channels as usize];

        let mut output_deinterleaved =
            vec![vec![0f32; processor.num_samples_per_frame()]; opt.render.num_channels as usize];

        let running = running.clone();
        let mute = opt.render.mute;
        let processor = Arc::clone(&processor);
        move |portaudio::DuplexStreamCallbackArgs { in_buffer, out_buffer, frames, .. }| {
            assert_eq!(frames, processor.num_samples_per_frame());

            let mut should_continue = true;
            let mut in_buffer_arc = buffer_pool.pop().unwrap();
            let in_buf = Arc::get_mut(&mut in_buffer_arc).unwrap();
            if let Some(source) = &mut capture_source {
                if !copy_stream(source, in_buf) {
                    should_continue = false;
                }
            } else {
                in_buf.copy_from_slice(in_buffer);
            }

            deinterleave(in_buf, &mut input_deinterleaved);
            processor.process_capture_frame(&mut input_deinterleaved).unwrap();
            interleave(&input_deinterleaved, in_buf);
            
            worker_in_tx.send(in_buffer_arc).unwrap();

            let mut out_buffer_arc = match worker_out_rx.try_recv() {
                Ok(buf) => buf,
                _ => {
                    let mut buf = buffer_pool.pop().unwrap();
                    Arc::get_mut(&mut buf).unwrap().iter_mut().for_each(|s| *s = 0.0);
                    buf
                }
            };

            if let Some(source) = &mut render_source {
                let out_buf = Arc::get_mut(&mut out_buffer_arc).unwrap();
                if !copy_stream(source, out_buf) {
                    should_continue = false;
                }
            }

            deinterleave(Arc::get_mut(&mut out_buffer_arc).unwrap(), &mut output_deinterleaved);
            processor.process_render_frame(&mut output_deinterleaved).unwrap();
            interleave(&output_deinterleaved, out_buffer);

            let _ = buffer_pool.push(out_buffer_arc);

            if mute {
                out_buffer.iter_mut().for_each(|m| *m = 0.0)
            }

            if should_continue {
                portaudio::Continue
            } else {
                running.store(false, Ordering::SeqCst);
                portaudio::Complete
            }
        }
    };

    let stream_settings = create_stream_settings(&pa, &processor, &opt)?;
    let mut stream = pa.open_non_blocking_stream(stream_settings, audio_callback)?;
    stream.start()?;

    ctrlc::set_handler({
        let running = running.clone();
        move || {
            running.store(false, Ordering::SeqCst);
        }
    })?;

    while running.load(Ordering::SeqCst) {
        thread::sleep(Duration::from_millis(10));
    }

    println!("{:#?}", processor.get_stats());

    Ok(())
}

fn worker_thread(in_buffers: Receiver<SampleBuffer>, out_buffers: Sender<SampleBuffer>,
    out_path: &Option<PathBuf>, buffer_pool: Arc<ArrayQueue<Arc<Vec<f32>>>>) {
    
    let mut capture_preprocess_sink = if let Some(path) = out_path {
        Some(open_wav_writer(path, 2).unwrap())
    } else {
        None
    };

    loop {
        match in_buffers.recv() {
            Ok(buf_in) => {
                if let Some(sink) = &mut capture_preprocess_sink {
                    for sample in buf_in.iter() {
                        sink.write_sample(*sample).unwrap();
                    }
                }
                let _ = buffer_pool.push(buf_in);
                let buffer_out_arc = buffer_pool.pop().unwrap();
                for sample in Arc::get_mut(&mut buffer_out_arc.clone()).unwrap().iter_mut() {
                    *sample = 0.0;
                }
                out_buffers.send(buffer_out_arc).unwrap();
            }
            Err(_) => return,
        }
    }
}

