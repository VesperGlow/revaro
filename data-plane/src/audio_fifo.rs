use std::{ffi::c_void, ptr::NonNull};

use ffmpeg::{ChannelLayout, codec, frame, util::format::Sample};
use ffmpeg_next as ffmpeg;

const MAX_BUFFERED_AUDIO_SAMPLES: usize = 1 << 20;

/// A small encoder-side accumulator. libswresample and filters may return any
/// number of samples; fixed-frame encoders must only see frame_size chunks.
pub(crate) struct AudioFrameAccumulator {
    fifo: NonNull<ffmpeg::ffi::AVAudioFifo>,
    format: Sample,
    layout: ChannelLayout,
    rate: u32,
    frame_size: usize,
    variable: bool,
    small_last_frame: bool,
}

impl AudioFrameAccumulator {
    pub(crate) fn new(encoder: &codec::encoder::Audio) -> Result<Self, String> {
        let capabilities = encoder
            .codec()
            .map(|codec| codec.capabilities())
            .unwrap_or_else(codec::capabilities::Capabilities::empty);
        let variable = capabilities
            .contains(codec::capabilities::Capabilities::VARIABLE_FRAME_SIZE)
            || encoder.frame_size() == 0;
        let frame_size = if variable {
            0
        } else {
            encoder.frame_size() as usize
        };
        let fifo = unsafe {
            ffmpeg::ffi::av_audio_fifo_alloc(
                encoder.format().into(),
                encoder.channel_layout().channels(),
                frame_size.max(1) as i32,
            )
        };
        Ok(Self {
            fifo: NonNull::new(fifo).ok_or("allocate audio FIFO")?,
            format: encoder.format(),
            layout: encoder.channel_layout(),
            rate: encoder.rate(),
            frame_size,
            variable,
            small_last_frame: capabilities
                .contains(codec::capabilities::Capabilities::SMALL_LAST_FRAME),
        })
    }

    pub(crate) fn push(&mut self, frame: &frame::Audio) -> Result<(), String> {
        let samples = frame.samples();
        if samples == 0 {
            return Ok(());
        }
        if self.len().saturating_add(samples) > MAX_BUFFERED_AUDIO_SAMPLES {
            return Err("audio FIFO exceeds bounded sample limit".into());
        }
        let written = unsafe {
            ffmpeg::ffi::av_audio_fifo_write(
                self.fifo.as_ptr(),
                (*frame.as_ptr()).extended_data.cast::<*mut c_void>(),
                samples as i32,
            )
        };
        if written != samples as i32 {
            return Err(format!("write audio FIFO: {written}"));
        }
        Ok(())
    }

    pub(crate) fn next_frame(&mut self, finishing: bool) -> Result<Option<frame::Audio>, String> {
        let available = self.len();
        if available == 0 {
            return Ok(None);
        }
        let (read_samples, output_samples) = if self.variable {
            (available, available)
        } else if available >= self.frame_size {
            (self.frame_size, self.frame_size)
        } else if !finishing {
            return Ok(None);
        } else if self.small_last_frame {
            (available, available)
        } else {
            (available, self.frame_size)
        };
        let mut output = frame::Audio::new(self.format, output_samples, self.layout);
        output.set_rate(self.rate);
        if output_samples > read_samples {
            let silence = unsafe {
                ffmpeg::ffi::av_samples_set_silence(
                    (*output.as_mut_ptr()).extended_data,
                    read_samples as i32,
                    (output_samples - read_samples) as i32,
                    self.layout.channels(),
                    self.format.into(),
                )
            };
            if silence < 0 {
                return Err(format!("pad final audio frame: {silence}"));
            }
        }
        let read = unsafe {
            ffmpeg::ffi::av_audio_fifo_read(
                self.fifo.as_ptr(),
                (*output.as_mut_ptr()).extended_data.cast::<*mut c_void>(),
                read_samples as i32,
            )
        };
        if read != read_samples as i32 {
            return Err(format!("read audio FIFO: {read}"));
        }
        Ok(Some(output))
    }

    pub(crate) fn frame_size(&self) -> usize {
        self.frame_size
    }

    fn len(&self) -> usize {
        unsafe { ffmpeg::ffi::av_audio_fifo_size(self.fifo.as_ptr()).max(0) as usize }
    }
}

impl Drop for AudioFrameAccumulator {
    fn drop(&mut self) {
        unsafe { ffmpeg::ffi::av_audio_fifo_free(self.fifo.as_ptr()) }
    }
}

unsafe impl Send for AudioFrameAccumulator {}
