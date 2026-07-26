use df::*;
use ndarray::prelude::*;
use std::sync::Arc;
use std::path::PathBuf;
use df::tract::{*};


pub type SampleBuffer = Arc<Vec<f32>>;

/// Decay coefficient for the peak-hold smoothing shared by the SNR and level meters.
const SMOOTHING_ALPHA: f32 = 0.97;

/// Floor applied to the linear RMS before converting to dBFS (avoids log10(0)).
const RMS_FLOOR: f32 = 1e-7;

pub struct Processor {
    num_channels: usize,
    channel_buf: Vec<f32>,
    denoised_buf: Vec<f32>,
    frame_sz: usize,
    m: Option<tract::DfTract>,
    snr_status: f32,
    level_rms: f32,
    enable: bool,
}

impl Processor {
    pub fn new(num_channels: usize, frame_sz: usize, model_path: Option<PathBuf>, atten_lim: f32) -> Self {
        let channel_buf = vec![0.0f32; frame_sz];
        let denoised_buf = vec![0.0f32; frame_sz];

        let m = if model_path.is_some() {
            let model_path = model_path.unwrap();
            let mut r_params = RuntimeParams::default_with_ch(num_channels);
            r_params = r_params.with_atten_lim(atten_lim).with_thresholds(
                -15.0f32,  //min_db_thresh
                35.0f32,   //max_db_erb_thresh
                35.0f32,   //max_db_df_thresh
            );
            r_params = r_params.with_post_filter(0.0f32);  //post_filter_beta
            r_params = r_params.with_mask_reduce(ReduceMask::MAX);  //reduce_mask
            let df_params =
                DfParams::new(model_path).expect(format!("Could not load model file").as_ref());

            Some(DfTract::new(df_params, &r_params).expect("Could not initialize DeepFilter runtime."))
        } else { 
            None 
        };
        if let Some(ref some_m) = m {
            println!("num_channels: {num_channels}, frame_sz: {frame_sz}, hop_size: {0}", some_m.hop_size);
        } else {
            println!("No model provided, DeepFilterNet disabled");
        }
        Self { num_channels, channel_buf, denoised_buf, frame_sz, m, snr_status: 0f32, level_rms: 0f32, enable: true}
    }

    pub fn process_frame(&mut self, in_buffer: SampleBuffer, out_buffer: &mut SampleBuffer) -> f32 {
        assert_eq!(in_buffer.len(), self.frame_sz);
        assert!(out_buffer.capacity() >= in_buffer.len());

        // Input level meter: smooth the linear RMS with the same peak-hold decay
        // as the SNR, before any early return so it tracks the raw capture level
        // even when denoising is disabled.
        let sum_sq: f32 = in_buffer.iter().map(|&s| s * s).sum();
        let rms = (sum_sq / self.frame_sz as f32).sqrt();
        self.level_rms = f32::max(self.level_rms * SMOOTHING_ALPHA, rms);

        if let Some(ref mut m) = self.m && self.enable {
            let input = ArrayView2::from_shape((1, m.hop_size), in_buffer.as_slice()).unwrap();
            let output = ArrayViewMut2::from_shape((1, m.hop_size), Arc::get_mut(out_buffer).unwrap().as_mut_slice()).unwrap();

            let new_snr = m.process(input, output).expect("Failed to process DF frame");
            self.snr_status = f32::max(self.snr_status * SMOOTHING_ALPHA, new_snr);
            self.snr_status
        } else {
            let y = Arc::get_mut(out_buffer).unwrap().as_mut_slice();
            for (i, x) in in_buffer.iter().enumerate() {
                y[i] = *x;
            }
            assert!(in_buffer.len() == out_buffer.len());
            -15f32
        }
    }

    /// Smoothed input level in dBFS (full scale = 1.0).
    pub fn level_dbfs(&self) -> f32 {
        20.0 * self.level_rms.max(RMS_FLOOR).log10()
    }

    pub fn enable_denoise(&mut self, enable: bool) {
        self.enable = enable;
    }
}
