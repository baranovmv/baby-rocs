use anyhow::{Error, anyhow};
use clap::Parser;
use crossbeam_queue::ArrayQueue;
use hound::{WavIntoSamples, WavReader, WavWriter};
use regex::Regex;
use serde::{Deserialize, Serialize};
use std::{
    fs::{self, File},
    io::{BufReader, BufWriter},
    net::IpAddr,
    path::{Path, PathBuf},
    sync::LazyLock,
    sync::{
        Arc, Barrier,
        atomic::{AtomicBool, Ordering},
        mpsc::*,
    },
    thread, time,
};
use webrtc_audio_processing as wap;

use roc::ffi as rcs;

mod common;
use common::{deinterleave, interleave};
mod audio_processing;
use audio_processing::{Processor as ap, SampleBuffer};
mod ui;
use ui::Shared;

const AUDIO_SAMPLE_RATE: u32 = 48_000;
const AUDIO_INTERLEAVED: bool = true;

#[derive(Debug, Parser)]
struct Args {
    /// Configuration file that stores JSON serialization of [`Option`] struct.
    #[arg(long)]
    pub config_file: Option<PathBuf>,

    /// List available audio devices and exit.
    #[arg(long)]
    pub list_devices: bool,

    #[arg(long)]
    pub model: Option<PathBuf>,
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
    config: wap::Config,
    /// Config for roc-send node
    roc_send: Option<RocSendOptions>,
    /// Various sound processing options
    processing: ProcessingOptions,
}

#[derive(Deserialize, Serialize, Default, Clone, Debug)]
struct RocSendOptions {
    destination: String,
    source: u16,
    repair: Option<u16>,
    control: Option<u16>,
}

#[derive(Deserialize, Serialize, Default, Clone, Debug)]
struct ProcessingOptions {
    snr_threshold: f32,
    snr_timeout: f32,
    deepfilternet_enabled: bool,
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
    Err(anyhow!(
        "Audio device matching \"{}\" not found.",
        device_name
    ))
}

fn create_stream_settings(
    pa: &portaudio::PortAudio,
    wap_processor: &wap::Processor,
    render_options: &RenderOptions,
    capture_options: &CaptureOptions,
) -> Result<portaudio::DuplexStreamSettings<f32, f32>, Error> {
    let input_device = match_device(pa, Regex::new(&capture_options.device_name)?)?;
    let input_device_info = &pa.device_info(input_device)?;
    let input_params = portaudio::StreamParameters::<f32>::new(
        input_device,
        capture_options.num_channels as i32,
        AUDIO_INTERLEAVED,
        input_device_info.default_low_input_latency,
    );

    let output_device = match_device(pa, Regex::new(&render_options.device_name)?)?;
    let output_device_info = &pa.device_info(output_device)?;
    let output_params = portaudio::StreamParameters::<f32>::new(
        output_device,
        render_options.num_channels as i32,
        AUDIO_INTERLEAVED,
        output_device_info.default_low_output_latency,
    );

    pa.is_duplex_format_supported(input_params, output_params, f64::from(AUDIO_SAMPLE_RATE))?;

    Ok(portaudio::DuplexStreamSettings::new(
        input_params,
        output_params,
        f64::from(AUDIO_SAMPLE_RATE),
        wap_processor.num_samples_per_frame() as u32,
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

static RUNNING: LazyLock<Arc<AtomicBool>> = LazyLock::new(|| Arc::new(true.into()));

fn main() -> Result<(), Error> {
    let args = Args::parse();

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

    let config_file = args
        .config_file
        .ok_or_else(|| anyhow!("--config-file is required"))?;
    let opt: Options = json5::from_str(&fs::read_to_string(&config_file)?)?;

    let wap_processor = Arc::new(wap::Processor::new(AUDIO_SAMPLE_RATE)?);

    wap_processor.set_config(opt.config);
    let frame_size = wap_processor.num_samples_per_frame();

    let mut capture_source = if let Some(path) = &opt.capture.source_path {
        Some(open_wav_reader(path)?)
    } else {
        None
    };
    let mut render_source = if let Some(path) = &opt.render.source_path {
        Some(open_wav_reader(path)?)
    } else {
        None
    };

    let buffer_pool = Arc::new(ArrayQueue::<Arc<Vec<f32>>>::new(64));
    for _ in 0..64 {
        buffer_pool.push(Arc::new(vec![
            0f32;
            frame_size * opt.capture.num_channels as usize
        ]));
    }
    let (worker_in_tx, worker_in_rx) = channel();
    let buffer_pool_wrkr = buffer_pool.clone();

    let preprocess_sink_path = opt.capture.preprocess_sink_path.clone();
    let postprocess_sink_path = opt.capture.postprocess_sink_path.clone();
    let num_channels = opt.capture.num_channels;
    let barrier = Arc::new(Barrier::new(2));
    let barrier_worker = barrier.clone();
    let roc_opts = opt.roc_send;

    // Runtime state shared between the audio worker and the TUI.
    let shared = Arc::new(Shared::new(
        opt.processing.snr_threshold,
        opt.processing.snr_timeout,
        opt.processing.deepfilternet_enabled,
    ));

    // Route roc-toolkit log messages into the UI's log tab.
    roc::log::set_level(roc::log::Level::Debug);
    {
        let shared = shared.clone();
        roc::log::set_handler_async(move |msg| {
            let text = match (&msg.module, &msg.text) {
                (Some(module), Some(text)) => format!("[{module}] {text}"),
                (None, Some(text)) => text.clone(),
                _ => String::new(),
            };
            shared.push_log(msg.level, text);
        });
    }

    let shared_worker = shared.clone();
    let worker_thr = thread::spawn(move || {
        worker_thread(
            worker_in_rx,
            &preprocess_sink_path,
            &postprocess_sink_path,
            buffer_pool_wrkr,
            num_channels,
            frame_size,
            barrier_worker,
            args.model,
            roc_opts,
            shared_worker,
        );
    });

    let audio_callback = {
        // Allocate buffers outside the performance-sensitive audio loop.
        let mut input_deinterleaved =
            vec![vec![0f32; frame_size]; opt.capture.num_channels as usize];

        let mut output_deinterleaved =
            vec![vec![0f32; frame_size]; opt.render.num_channels as usize];

        // Dedicated render buffer — not taken from the shared pool since it never leaves the callback.
        let mut render_buffer = vec![0f32; frame_size * opt.render.num_channels as usize];

        let mute = opt.render.mute;
        let wap_processor = Arc::clone(&wap_processor);
        move |portaudio::DuplexStreamCallbackArgs {
                  in_buffer,
                  out_buffer,
                  frames,
                  ..
              }| {
            assert_eq!(frames, frame_size);

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
            wap_processor
                .process_capture_frame(&mut input_deinterleaved)
                .unwrap();
            interleave(&input_deinterleaved, in_buf);

            worker_in_tx.send(in_buffer_arc).unwrap();

            render_buffer.iter_mut().for_each(|s| *s = 0.0);

            if let Some(source) = &mut render_source {
                if !copy_stream(source, &mut render_buffer) {
                    should_continue = false;
                }
            }

            deinterleave(&render_buffer, &mut output_deinterleaved);
            wap_processor
                .process_render_frame(&mut output_deinterleaved)
                .unwrap();
            interleave(&output_deinterleaved, out_buffer);

            if mute {
                out_buffer.iter_mut().for_each(|m| *m = 0.0)
            }

            if should_continue {
                portaudio::Continue
            } else {
                RUNNING.store(false, Ordering::SeqCst);
                portaudio::Complete
            }
        }
    };

    let stream_settings = create_stream_settings(&pa, &wap_processor, &opt.render, &opt.capture)?;
    let mut stream = pa.open_non_blocking_stream(stream_settings, audio_callback)?;
    barrier.wait();
    stream.start()?;

    ctrlc::set_handler({
        move || {
            RUNNING.store(false, Ordering::SeqCst);
        }
    })?;

    // Run the TUI until the user quits or the audio pipeline stops.
    ui::crossterm::run(shared.clone(), RUNNING.clone())?;

    // Stop the stream and drop it so the worker's channel sender is released,
    // letting the worker thread finish and finalize its WAV sinks.
    let _ = stream.stop();
    drop(stream);
    let _ = worker_thr.join();

    // Persist runtime-edited processing values back to the config file.
    if let Err(e) = persist_config(&config_file, &shared) {
        eprintln!("Failed to write config: {e}");
    }

    println!("{:#?}", wap_processor.get_stats());

    Ok(())
}

/// Re-read the config file, update the processing options with the values
/// edited at runtime, and write it back out.
fn persist_config(path: &Path, shared: &Shared) -> Result<(), Error> {
    let mut opt: Options = json5::from_str(&fs::read_to_string(path)?)?;
    opt.processing.snr_threshold = shared.snr_threshold();
    opt.processing.snr_timeout = shared.snr_timeout();
    opt.processing.deepfilternet_enabled = shared.deepfilternet_enabled();
    fs::write(path, json5::to_string(&opt)?)?;
    Ok(())
}

fn worker_thread(
    in_buffers: Receiver<SampleBuffer>,
    in_path: &Option<PathBuf>,
    out_path: &Option<PathBuf>,
    buffer_pool: Arc<ArrayQueue<Arc<Vec<f32>>>>,
    num_channels: u16,
    frame_size: usize,
    barrier: Arc<Barrier>,
    model: Option<PathBuf>,
    roc_opts: Option<RocSendOptions>,
    shared: Arc<Shared>,
) {
    let num_ch = num_channels as usize;
    let mut proc = ap::new(num_ch, frame_size, model, 80f32);

    let mut roc_sender = if roc_opts.is_some() {
        let roc_sender = RocTimedSender::new(roc_opts.unwrap(), shared.clone());
        if roc_sender.is_err() {
            shared.push_log(roc::log::Level::Error, "Can't create roc-sender");
            RUNNING.store(false, Ordering::SeqCst);
            return;
        }
        Some(roc_sender.unwrap())
    } else {
        None
    };

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

    // Frames per periodic debug log (~1s at 10 ms/frame).
    const LOG_EVERY_FRAMES: u64 = 100;

    // Real-time budget for one frame: frame_size samples at the capture rate.
    // process_frame must finish within this wall-clock time to keep up with the
    // live audio stream; if it doesn't, the drain loop below discards frames and
    // the roc receiver starves.
    let frame_dur_secs = frame_size as f32 / AUDIO_SAMPLE_RATE as f32;

    // Rolling 5 s statistics for the DeepFilterNet real-time factor.
    const RTF_REPORT_INTERVAL: time::Duration = time::Duration::from_secs(5);
    let mut rtf_window_start = time::Instant::now();
    let mut rtf_sum = 0.0f32;
    let mut rtf_max = 0.0f32;
    let mut rtf_count: u64 = 0;
    let mut dropped_frames: u64 = 0;

    loop {
        match in_buffers.recv() {
            Ok(mut buf_in) => {
                // Drain stale frames — if the worker fell behind (e.g. during model init),
                // skip to the most recent frame and return skipped buffers to the pool.
                while let Ok(newer) = in_buffers.try_recv() {
                    let _ = buffer_pool.push(buf_in);
                    buf_in = newer;
                    dropped_frames += 1;
                }

                proc.enable_denoise(shared.deepfilternet_enabled());
                let denoise_on = shared.deepfilternet_enabled();
                let t0 = time::Instant::now();
                let snr = proc.process_frame(buf_in.clone(), &mut out_buffer);
                let elapsed = t0.elapsed();
                shared.set_current_snr(snr);

                // Only accumulate the real-time factor while DeepFilterNet is
                // actually running; the disabled path is a trivial copy and would
                // otherwise dilute the statistics.
                if denoise_on {
                    let rtf = elapsed.as_secs_f32() / frame_dur_secs;
                    rtf_sum += rtf;
                    rtf_max = rtf_max.max(rtf);
                    rtf_count += 1;
                }

                if rtf_window_start.elapsed() >= RTF_REPORT_INTERVAL {
                    if rtf_count > 0 {
                        let avg = rtf_sum / rtf_count as f32;
                        shared.push_log(
                            roc::log::Level::Info,
                            format!(
                                "DFN RTF avg={avg:.2}x max={rtf_max:.2}x (n={rtf_count}, dropped={dropped_frames}) / 5s"
                            ),
                        );
                    }
                    rtf_window_start = time::Instant::now();
                    rtf_sum = 0.0;
                    rtf_max = 0.0;
                    rtf_count = 0;
                    dropped_frames = 0;
                }

                // Publish the input level (dBFS) for the level meter.
                let level_dbfs = proc.level_dbfs();
                shared.set_current_level(level_dbfs);

                // Write denoised interleaved audio to sink
                if let Some(sink) = &mut capture_preprocess_sink {
                    for i in 0..frame_size {
                        for _ch in 0..num_ch {
                            sink.write_sample(buf_in[i]).unwrap();
                        }
                    }
                }
                // Write denoised interleaved audio to sink
                if let Some(sink) = &mut output_postprocess_sink {
                    for i in 0..frame_size {
                        for _ch in 0..num_ch {
                            sink.write_sample(out_buffer[i]).unwrap();
                        }
                    }
                }

                if let Some(roc_sender) = &mut roc_sender {
                    if let Ok(sending) = roc_sender.send(&out_buffer, snr) {
                        shared.set_sending(sending);
                    } else {
                        shared.push_log(roc::log::Level::Error, "Can't send frame");
                        RUNNING.store(false, Ordering::SeqCst);
                        return;
                    }
                }
                let _ = buffer_pool.push(buf_in);
            }
            Err(_) => {
                shared.push_log(roc::log::Level::Error, "Error while getting a buffer");
                RUNNING.store(false, Ordering::SeqCst);
                return;
            }
        }
    }
}

struct RocTimedSender {
    config: RocSendOptions,
    context: roc::context::Context,
    sender: Option<roc::sender::Sender>,
    first_silent_ts: std::time::Instant,
    shared: Arc<Shared>,
}

impl RocTimedSender {
    fn new(config: RocSendOptions, shared: Arc<Shared>) -> Result<Self, Error> {
        let context_config = roc::config::Config::default();
        let context = roc::context::Context::open(&context_config).result()?;
        Ok(Self {
            config,
            context,
            sender: None,
            first_silent_ts: time::Instant::now(),
            shared,
        })
    }

    fn send(&mut self, frame: &[f32], snr: f32) -> Result<bool, Error> {
        // Read live control values edited via the UI.
        let bypass = self.shared.bypass();
        let threshold = self.shared.snr_threshold();
        let timeout = time::Duration::from_secs_f32(self.shared.snr_timeout());
        let above = bypass || snr >= threshold;

        if !above && self.sender.is_none() {
            return Ok(self.sender.is_some());
        } else if above && self.sender.is_none() {
            let roc_sender = self
                .make_sender(
                    self.config.destination.clone(),
                    self.config.source,
                    self.config.repair,
                    self.config.control,
                )
                .expect("failed to create roc sender");
            self.sender = Some(roc_sender);
            self.shared
                .push_log(roc::log::Level::Info, format!("Start sending, SNR {snr}"));
        }

        self.sender.as_mut().unwrap().write_slice(frame).result()?;

        if above {
            self.first_silent_ts = time::Instant::now();
        } else if self.first_silent_ts.elapsed() >= timeout {
            self.shared
                .push_log(roc::log::Level::Info, "Timeout, send off");
            self.sender = None;
        }
        Ok(self.sender.is_some())
    }

    fn make_sender(
        &mut self,
        roc_send_ip: String,
        source_port: u16,
        repair_port: Option<u16>,
        control_port: Option<u16>,
    ) -> Result<roc::sender::Sender, Error> {
        let sender_config = roc::config::SenderConfig::builder()
            .frame_encoding(roc::config::MediaEncodingFactory::mono(
                AUDIO_SAMPLE_RATE,
                roc::config::Format::PCM_FLOAT32,
            ))
            .packet_encoding(roc::config::PacketEncoding::ACP_L16_STEREO)
            .clock_source(rcs::roc_clock_source::ROC_CLOCK_SOURCE_EXTERNAL)
            .build();

        let mut sender = roc::sender::Sender::open(&mut self.context, &sender_config).result()?;

        let source_endp = roc::endpoint::EndpointBuilder::new()
            .host(roc_send_ip.clone())
            .port(source_port)
            .protocol(roc::network::config::Protocol::RTP_RS8M_SOURCE)
            .build()
            .result()?;

        sender
            .default_slot()
            .connect(roc::config::Interface::AudioSource, &source_endp)
            .result()?;

        source_endp.deallocate();

        if let Some(control_port) = control_port {
            let control_endp = roc::endpoint::EndpointBuilder::new()
                .host(roc_send_ip.clone())
                .port(control_port)
                .protocol(roc::network::config::Protocol::RTCP)
                .build()
                .result()?;

            sender
                .default_slot()
                .connect(roc::config::Interface::AudioControl, &control_endp)
                .result()?;

            control_endp.deallocate();
        }

        if let Some(repair_port) = repair_port {
            let repair_endp = roc::endpoint::EndpointBuilder::new()
                .host(roc_send_ip.clone())
                .port(repair_port)
                .protocol(roc::network::config::Protocol::RS8M_REPAIR)
                .build()
                .result()?;

            sender
                .default_slot()
                .connect(roc::config::Interface::AudioRepair, &repair_endp)
                .result()?;

            repair_endp.deallocate();
        }
        Ok(sender)
    }
}
