use df::*;
use ndarray::prelude::*;
use std::sync::Arc;
use std::path::PathBuf;
use df::tract::{*};


pub type SampleBuffer = Arc<Vec<f32>>;

pub struct Processor {
    num_channels: usize,
    channel_buf: Vec<f32>,
    denoised_buf: Vec<f32>,
    frame_sz: usize,
    m: tract::DfTract,
}

impl Processor {
    pub fn new(num_channels: usize, frame_sz: usize, model_path: PathBuf, atten_lim: f32) -> Self {
        let channel_buf = vec![0.0f32; frame_sz];
        let denoised_buf = vec![0.0f32; frame_sz];

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
        let m =
            DfTract::new(df_params, &r_params).expect("Could not initialize DeepFilter runtime.");
        println!("num_channels: {num_channels}, frame_sz: {frame_sz}, hop_size: {0}", m.hop_size);
        Self { num_channels, channel_buf, denoised_buf, frame_sz, m }
    }

    pub fn process_frame(&mut self, in_buffer: SampleBuffer, out_buffer: &mut SampleBuffer) -> f32 {
        assert_eq!(in_buffer.len(), self.frame_sz);
        assert!(out_buffer.capacity() >= in_buffer.len());
        
        let input = ArrayView2::from_shape((1, self.m.hop_size), in_buffer.as_slice()).unwrap();
        let mut output = ArrayViewMut2::from_shape((1, self.m.hop_size), Arc::get_mut(out_buffer).unwrap().as_mut_slice()).unwrap();

        self.m.process(input, output).expect("Failed to process DF frame")
        // Arc::get_mut(out_buffer).unwrap().clear();
        // Arc::get_mut(out_buffer).unwrap().extend_from_slice(in_buffer.as_slice());
    }
}
