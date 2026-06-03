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
        Barrier,
        mpsc::{*}
    },
    thread,
    time::Duration,
};
use structopt::StructOpt;
use crossbeam_queue::ArrayQueue;
use webrtc_audio_processing::Processor;
use webrtc_audio_processing_config::Config;

mod common;
use common::{deinterleave, interleave};
mod audio_processing;
use audio_processing::{Processor as ap, SampleBuffer};

const AUDIO_SAMPLE_RATE: u32 = 48_000;
const AUDIO_INTERLEAVED: bool = true;

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
    let buffer_pool_wrkr = buffer_pool.clone();

    let preprocess_sink_path = opt.capture.preprocess_sink_path.clone();
    let postprocess_sink_path = opt.capture.postprocess_sink_path.clone();
    let num_channels = opt.capture.num_channels;
    let barrier = Arc::new(Barrier::new(2));
    let barrier_worker = barrier.clone();
    let frame_size = processor.num_samples_per_frame();
    let worker_thr = thread::spawn(move || {
        worker_thread(worker_in_rx, &preprocess_sink_path, &postprocess_sink_path, buffer_pool_wrkr, num_channels, frame_size, barrier_worker);
    });

    let audio_callback = {
        // Allocate buffers outside the performance-sensitive audio loop.
        let mut input_deinterleaved =
            vec![vec![0f32; processor.num_samples_per_frame()]; opt.capture.num_channels as usize];

        let mut output_deinterleaved =
            vec![vec![0f32; processor.num_samples_per_frame()]; opt.render.num_channels as usize];

        // Dedicated render buffer — not taken from the shared pool since it never leaves the callback.
        let mut render_buffer =
            vec![0f32; processor.num_samples_per_frame() * opt.render.num_channels as usize];

        let running = running.clone();
        let mute = opt.render.mute;
        let processor = Arc::clone(&processor);
        move |portaudio::DuplexStreamCallbackArgs { in_buffer, out_buffer, frames, .. }| {
            assert_eq!(frames, processor.num_samples_per_frame());

            let mut should_continue = true;
            let Some(mut in_buffer_arc) = buffer_pool.pop() else {
                eprintln!("buffer pool exhausted, dropping audio frame");
                out_buffer.iter_mut().for_each(|s| *s = 0.0);
                return portaudio::Continue;
            };
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

            render_buffer.iter_mut().for_each(|s| *s = 0.0);

            if let Some(source) = &mut render_source {
                if !copy_stream(source, &mut render_buffer) {
                    should_continue = false;
                }
            }

            deinterleave(&render_buffer, &mut output_deinterleaved);
            processor.process_render_frame(&mut output_deinterleaved).unwrap();
            interleave(&output_deinterleaved, out_buffer);

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
    barrier.wait();
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

fn worker_thread(
    in_buffers: Receiver<SampleBuffer>,
    in_path: &Option<PathBuf>,
    out_path: &Option<PathBuf>,
    buffer_pool: Arc<ArrayQueue<Arc<Vec<f32>>>>,
    num_channels: u16,
    frame_size: usize,
    barrier: Arc::<Barrier>,
) {
    let num_ch = num_channels as usize;
    let mut proc = ap::new(num_ch, frame_size, 
        "/home/misha/coding/baby_rocs/3rdparty/DeepFilterNer/models/DeepFilterNet3_onnx.tar.gz",
        80f32);

    // One DenoiseState per channel (rnnoise operates on mono 480-sample frames at 48kHz)
    // let mut denoise_states: Vec<Box<DenoiseState>> =
    //     (0..num_ch).map(|_| DenoiseState::new()).collect();

    // Dedicated output buffer for processing — not taken from the shared pool.
    let mut out_buffer = Arc::new(vec![0.0f32; frame_size * num_ch]);

    let mut capture_preprocess_sink = if let Some(path) = in_path {
        Some(open_wav_writer(path, num_channels).unwrap())
    } else {
        None
    };
    let mut output_postprocess_sink = if let Some(path) = out_path {
        Some(open_wav_writer(path, num_channels).unwrap())
    } else {
        None
    };
    
    barrier.wait();

    loop {
        match in_buffers.recv() {
            Ok(mut buf_in) => {
                // Drain stale frames — if the worker fell behind (e.g. during model init),
                // skip to the most recent frame and return skipped buffers to the pool.
                while let Ok(newer) = in_buffers.try_recv() {
                    let _ = buffer_pool.push(buf_in);
                    buf_in = newer;
                }

                let snr = proc.process_frame(buf_in.clone(), &mut out_buffer);
                println!("SNR: {snr}");

                // Write denoised interleaved audio to sink
                if let Some(sink) = &mut capture_preprocess_sink {
                    for i in 0..frame_size {
                        for ch in 0..num_ch {
                            sink.write_sample(buf_in[i]).unwrap();
                        }
                    }
                }
                // Write denoised interleaved audio to sink
                if let Some(sink) = &mut output_postprocess_sink {
                    for i in 0..frame_size {
                        for ch in 0..num_ch {
                            sink.write_sample(out_buffer[i]).unwrap();
                        }
                    }
                }

                let _ = buffer_pool.push(buf_in);
            }
            Err(_) => return,
        }
    }
}

