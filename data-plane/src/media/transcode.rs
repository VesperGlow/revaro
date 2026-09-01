fn millis(value: i64, base: Rational) -> i64 {
    if value < 0 {
        return 0;
    }
    let numerator = i128::from(value) * i128::from(base.numerator()) * 1000;
    let denominator = i128::from(base.denominator()).max(1);
    (numerator / denominator).clamp(0, i128::from(i64::MAX)) as i64
}

fn open_input(
    reader: crate::s3::S3RangeReader,
    cancel: CancellationToken,
) -> Result<ffmpeg::format::context::Input, String> {
    let io =
        StreamIo::from_read_seek_with_capacity(reader, 256 << 10).map_err(|e| e.to_string())?;
    ffmpeg::format::input_from_stream_with_interrupt(io, None, None, move || cancel.is_cancelled())
        .map_err(|e| format!("open media: {e}"))
}

// ---- transcoding: decode + re-encode to browser-safe codecs ----

const H264_DEFAULT_BITRATE: i64 = 8_000_000;
const AAC_DEFAULT_BITRATE: usize = 192_000;

fn is_h264_codec(id: codec::Id) -> bool {
    id == codec::Id::H264
}

fn is_aac_codec(id: codec::Id) -> bool {
    id == codec::Id::AAC
}

struct VideoEncode {
    decoder: codec::decoder::Video,
    encoder: codec::encoder::Video,
    scaler: Scaler,
    scaled: Video,
    output_index: usize,
    output_time_base: Rational,
    start_pts: i64,
}

impl VideoEncode {
    fn drain(&mut self, output: &mut format::context::Output) -> Result<(), String> {
        let mut encoded = Packet::empty();
        while self.encoder.receive_packet(&mut encoded).is_ok() {
            encoded.set_stream(self.output_index);
            if encoded.duration() == 0 {
                encoded.set_duration(1);
            }
            encoded.rescale_ts(self.encoder.time_base(), self.output_time_base);
            encoded
                .write_interleaved(output)
                .map_err(|e| e.to_string())?;
        }
        Ok(())
    }

    fn send(
        &mut self,
        packet: &Packet,
        output: &mut format::context::Output,
    ) -> Result<(), String> {
        self.decoder
            .send_packet(packet)
            .map_err(|e| e.to_string())?;
        let mut decoded = Video::empty();
        while self.decoder.receive_frame(&mut decoded).is_ok() {
            let Some(pts) = decoded.pts() else { continue };
            if pts < self.start_pts {
                continue;
            }
            self.scaler
                .run(&decoded, &mut self.scaled)
                .map_err(|e| e.to_string())?;
            self.scaled.set_pts(Some(pts - self.start_pts));
            self.encoder
                .send_frame(&self.scaled)
                .map_err(|e| e.to_string())?;
            self.drain(output)?;
        }
        Ok(())
    }

    fn flush(&mut self, output: &mut format::context::Output) -> Result<(), String> {
        self.decoder.send_eof().map_err(|e| e.to_string())?;
        let mut decoded = Video::empty();
        while self.decoder.receive_frame(&mut decoded).is_ok() {
            let Some(pts) = decoded.pts() else { continue };
            if pts < self.start_pts {
                continue;
            }
            self.scaler
                .run(&decoded, &mut self.scaled)
                .map_err(|e| e.to_string())?;
            self.scaled.set_pts(Some(pts - self.start_pts));
            self.encoder
                .send_frame(&self.scaled)
                .map_err(|e| e.to_string())?;
            self.drain(output)?;
        }
        self.encoder.send_eof().map_err(|e| e.to_string())?;
        self.drain(output)
    }
}

struct AudioEncode {
    decoder: codec::decoder::Audio,
    encoder: codec::encoder::Audio,
    resampler: Resampler,
    output_index: usize,
    output_time_base: Rational,
    next_pts: i64,
    input_time_base: Rational,
    start_pts: i64,
    initialized: bool,
    accumulator: AudioFrameAccumulator,
}

impl AudioEncode {
    fn drain(&mut self, output: &mut format::context::Output) -> Result<(), String> {
        let mut encoded = Packet::empty();
        while self.encoder.receive_packet(&mut encoded).is_ok() {
            encoded.set_stream(self.output_index);
            encoded.rescale_ts(self.encoder.time_base(), self.output_time_base);
            encoded
                .write_interleaved(output)
                .map_err(|e| e.to_string())?;
        }
        Ok(())
    }

    fn encode_accumulated(
        &mut self,
        output: &mut format::context::Output,
        finishing: bool,
    ) -> Result<(), String> {
        while let Some(mut frame) = self.accumulator.next_frame(finishing)? {
            frame.set_pts(Some(self.next_pts));
            self.next_pts += frame.samples() as i64;
            self.encoder.send_frame(&frame).map_err(|e| e.to_string())?;
            self.drain(output)?;
        }
        Ok(())
    }

    fn resample_and_queue(
        &mut self,
        decoded: &frame::Audio,
        output: &mut format::context::Output,
    ) -> Result<(), String> {
        let mut resampled = frame::Audio::empty();
        self.resampler
            .run(decoded, &mut resampled)
            .map_err(|e| e.to_string())?;
        self.accumulator.push(&resampled)?;
        self.encode_accumulated(output, false)
    }

    fn send(
        &mut self,
        packet: &Packet,
        output: &mut format::context::Output,
    ) -> Result<(), String> {
        self.decoder
            .send_packet(packet)
            .map_err(|e| e.to_string())?;
        let mut decoded = frame::Audio::empty();
        while self.decoder.receive_frame(&mut decoded).is_ok() {
            if !self.initialized {
                let source_pts = decoded.pts().unwrap_or(self.start_pts);
                if source_pts < self.start_pts {
                    continue;
                }
                self.next_pts = rescale_value(
                    source_pts - self.start_pts,
                    self.input_time_base,
                    self.encoder.time_base(),
                );
                self.initialized = true;
            }
            self.resample_and_queue(&decoded, output)?;
        }
        Ok(())
    }

    fn flush(&mut self, output: &mut format::context::Output) -> Result<(), String> {
        self.decoder.send_eof().map_err(|e| e.to_string())?;
        let mut decoded = frame::Audio::empty();
        while self.decoder.receive_frame(&mut decoded).is_ok() {
            if !self.initialized {
                let source_pts = decoded.pts().unwrap_or(self.start_pts);
                if source_pts < self.start_pts {
                    continue;
                }
                self.next_pts = rescale_value(
                    source_pts - self.start_pts,
                    self.input_time_base,
                    self.encoder.time_base(),
                );
                self.initialized = true;
            }
            self.resample_and_queue(&decoded, output)?;
        }
        let mut resampler_drained = false;
        for _ in 0..32 {
            let samples = self
                .resampler
                .delay()
                .map(|delay| delay.output.max(0) as usize)
                .unwrap_or(0)
                .saturating_add(self.accumulator.frame_size().max(32));
            let mut delayed = frame::Audio::new(
                self.encoder.format(),
                samples,
                self.encoder.channel_layout(),
            );
            let remaining = self
                .resampler
                .flush(&mut delayed)
                .map_err(|e| e.to_string())?;
            let produced = delayed.samples();
            if produced > 0 {
                self.accumulator.push(&delayed)?;
                self.encode_accumulated(output, false)?;
            }
            if remaining.is_none() || produced == 0 {
                resampler_drained = true;
                break;
            }
        }
        if !resampler_drained {
            return Err("audio resampler did not drain within bounded iterations".into());
        }
        self.encode_accumulated(output, true)?;
        self.encoder.send_eof().map_err(|e| e.to_string())?;
        self.drain(output)
    }
}

enum Transform {
    Copy {
        source: usize,
        dest: usize,
        input_time_base: Rational,
        output_time_base: Rational,
        start_pts: i64,
        next_dts: Option<i64>,
        timestamp_offset: i64,
        last_mux_dts: Option<i64>,
    },
    Video(VideoEncode),
    Audio(AudioEncode),
}

fn process_transform(
    transform: &mut Transform,
    packet: &mut Packet,
    output: &mut format::context::Output,
) -> Result<(), String> {
    match transform {
        Transform::Copy {
            source,
            dest,
            input_time_base,
            output_time_base,
            start_pts,
            next_dts,
            timestamp_offset,
            last_mux_dts,
        } => {
            let original_pts = packet.pts();
            let original_dts = packet.dts();
            if original_pts
                .or(original_dts)
                .is_some_and(|value| value < *start_pts)
            {
                return Ok(());
            }
            // Matroska commonly omits DTS for presentation-ordered streams.
            // MP4 requires both timestamps. Preserve every supplied timestamp;
            // when one is absent, derive it from the other (or the end of the
            // previous packet) before rescaling the complete packet, including
            // its duration, into the muxer's time base.
            let pts = original_pts
                .or(original_dts)
                .map(|value| value - *start_pts);
            let dts = original_dts
                .map(|value| value - *start_pts)
                .or(*next_dts)
                .or(pts);
            packet.set_pts(pts.or(dts));
            packet.set_dts(dts);
            if let Some(value) = dts {
                *next_dts = Some(value.saturating_add(packet.duration().max(1)));
            }
            packet.rescale_ts(*input_time_base, *output_time_base);
            normalize_copy_timestamps(packet, timestamp_offset, last_mux_dts);
            packet.set_position(-1);
            packet.set_stream(*dest);
            packet
                .write_interleaved(output)
                .map_err(|e| format!("mux copied input stream {source} as MP4 stream {dest}: {e}"))
        }
        Transform::Video(video) => video.send(packet, output),
        Transform::Audio(audio) => audio.send(packet, output),
    }
}

fn flush_transform(
    transform: &mut Transform,
    output: &mut format::context::Output,
) -> Result<(), String> {
    match transform {
        Transform::Copy { .. } => Ok(()),
        Transform::Video(video) => video.flush(output),
        Transform::Audio(audio) => audio.flush(output),
    }
}

fn copy_stream(
    stream: &ffmpeg::format::stream::Stream,
    output: &mut format::context::Output,
    start_seconds: f64,
) -> Result<Transform, String> {
    let mut target = output
        .add_stream(encoder::find(codec::Id::None))
        .map_err(|e| e.to_string())?;
    target.set_parameters(stream.parameters());
    unsafe {
        (*target.parameters().as_mut_ptr()).codec_tag = 0;
    }
    Ok(Transform::Copy {
        source: stream.index(),
        dest: target.index(),
        input_time_base: stream.time_base(),
        output_time_base: Rational(0, 1),
        start_pts: seconds_to_pts(start_seconds, stream.time_base()),
        next_dts: None,
        timestamp_offset: 0,
        last_mux_dts: None,
    })
}

// Some Matroska remuxes contain a timestamp discontinuity at the beginning of
// a track (observed as video DTS 3690 followed by 0). The MP4 muxer rejects the
// second packet with EINVAL. Keep the supplied PTS/DTS relationship, but carry
// a stable offset across the discontinuity so decoded timestamps remain
// strictly increasing instead of adjusting just one packet and failing again.
fn normalize_copy_timestamps(
    packet: &mut Packet,
    timestamp_offset: &mut i64,
    last_mux_dts: &mut Option<i64>,
) {
    let Some(raw_dts) = packet.dts() else {
        return;
    };
    let mut dts = raw_dts.saturating_add(*timestamp_offset);
    if let Some(last) = *last_mux_dts
        && dts <= last
    {
        let step = packet.duration().max(1);
        let increase = last.saturating_add(step).saturating_sub(dts);
        *timestamp_offset = timestamp_offset.saturating_add(increase);
        dts = raw_dts.saturating_add(*timestamp_offset);
    }
    packet.set_dts(Some(dts));
    if let Some(pts) = packet.pts() {
        packet.set_pts(Some(pts.saturating_add(*timestamp_offset)));
    }
    *last_mux_dts = Some(dts);
}

fn setup_video_encode(
    stream: &ffmpeg::format::stream::Stream,
    output: &mut format::context::Output,
    source_bitrate: i64,
    global_header: bool,
    start_seconds: f64,
) -> Result<VideoEncode, String> {
    let input_time_base = stream.time_base();
    let decoder = codec::context::Context::from_parameters(stream.parameters())
        .map_err(|e| e.to_string())?
        .decoder()
        .video()
        .map_err(|e| e.to_string())?;
    let width = decoder.width();
    let height = decoder.height();
    if width == 0 || height == 0 {
        return Err("video stream has unknown dimensions".into());
    }
    let codec = encoder::find_by_name("libx264")
        .ok_or("libx264 H.264 encoder is unavailable")?
        .video()
        .map_err(|e| e.to_string())?;
    let mut context = codec::context::Context::new_with_codec(*codec)
        .encoder()
        .video()
        .map_err(|e| e.to_string())?;
    context.set_width(width);
    context.set_height(height);
    context.set_format(ffmpeg::format::Pixel::YUV420P);
    context.set_time_base(input_time_base);
    context.set_frame_rate(stream.avg_frame_rate().into());
    context.set_aspect_ratio(decoder.aspect_ratio());
    context.set_bit_rate(source_bitrate.clamp(1_000_000, H264_DEFAULT_BITRATE) as usize);
    context.set_gop(60);
    context.set_max_b_frames(0);
    unsafe {
        (*context.as_mut_ptr()).profile = ffmpeg::ffi::FF_PROFILE_H264_HIGH;
    }
    // FFmpeg 5 bindings still expose `safe` while FFmpeg 6+ removed it.
    // Default fills version-specific fields without referring to either ABI;
    // newer bindings make the update syntactically redundant, hence the
    // narrowly scoped lint allowance.
    #[allow(clippy::needless_update)]
    let threading = codec::threading::Config {
        kind: codec::threading::Type::Frame,
        count: 1,
        ..Default::default()
    };
    context.set_threading(threading);
    if global_header {
        context.set_flags(codec::Flags::GLOBAL_HEADER);
    }
    let mut options = Dictionary::new();
    options.set("preset", "veryfast");
    options.set("tune", "zerolatency");
    let opened = context
        .open_with(options)
        .map_err(|e| format!("open H.264 encoder: {e}"))?;
    let mut target = output.add_stream(codec).map_err(|e| e.to_string())?;
    target.set_parameters(&opened);
    target.set_time_base(input_time_base);
    let output_index = target.index();
    let scaler = Scaler::get(
        decoder.format(),
        width,
        height,
        ffmpeg::format::Pixel::YUV420P,
        width,
        height,
        Flags::BILINEAR,
    )
    .map_err(|e| e.to_string())?;
    Ok(VideoEncode {
        decoder,
        encoder: opened,
        scaler,
        scaled: Video::empty(),
        output_index,
        output_time_base: Rational(0, 1),
        start_pts: seconds_to_pts(start_seconds, input_time_base),
    })
}

fn setup_audio_encode(
    stream: &ffmpeg::format::stream::Stream,
    output: &mut format::context::Output,
    global_header: bool,
    start_seconds: f64,
) -> Result<AudioEncode, String> {
    let decoder = codec::context::Context::from_parameters(stream.parameters())
        .map_err(|e| e.to_string())?
        .decoder()
        .audio()
        .map_err(|e| e.to_string())?;
    let codec = encoder::find(codec::Id::AAC)
        .ok_or("AAC encoder is unavailable")?
        .audio()
        .map_err(|e| e.to_string())?;
    let mut context = codec::context::Context::new_with_codec(*codec)
        .encoder()
        .audio()
        .map_err(|e| e.to_string())?;
    let source_layout = if decoder.channel_layout().is_empty() {
        ChannelLayout::default(decoder.channels().into())
    } else {
        decoder.channel_layout()
    };
    let layout = if decoder.channels() > 6 {
        ChannelLayout::STEREO
    } else {
        source_layout
    };
    context.set_rate(48_000);
    context.set_channel_layout(layout);
    context.set_format(
        codec
            .formats()
            .ok_or("AAC encoder exposes no sample formats")?
            .next()
            .ok_or("AAC encoder exposes no sample formats")?,
    );
    context.set_bit_rate((layout.channels() as usize * 96_000).clamp(AAC_DEFAULT_BITRATE, 384_000));
    context.set_time_base((1, 48_000));
    if global_header {
        context.set_flags(codec::Flags::GLOBAL_HEADER);
    }
    let opened = context
        .open_as(codec)
        .map_err(|e| format!("open AAC encoder: {e}"))?;
    let mut target = output.add_stream(codec).map_err(|e| e.to_string())?;
    target.set_parameters(&opened);
    target.set_time_base((1, 48_000));
    let output_index = target.index();
    let resampler = Resampler::get(
        decoder.format(),
        if decoder.channel_layout().is_empty() {
            ChannelLayout::default(decoder.channels().into())
        } else {
            decoder.channel_layout()
        },
        decoder.rate(),
        opened.format(),
        opened.channel_layout(),
        opened.rate(),
    )
    .map_err(|e| e.to_string())?;
    let accumulator = AudioFrameAccumulator::new(&opened)?;
    Ok(AudioEncode {
        decoder,
        encoder: opened,
        resampler,
        output_index,
        output_time_base: Rational(0, 1),
        next_pts: 0,
        input_time_base: stream.time_base(),
        start_pts: seconds_to_pts(start_seconds, stream.time_base()),
        initialized: false,
        accumulator,
    })
}

fn seconds_to_pts(seconds: f64, base: Rational) -> i64 {
    if seconds <= 0.0 || base.numerator() == 0 {
        return 0;
    }
    (seconds / f64::from(base)).round() as i64
}

fn rescale_value(value: i64, from: Rational, to: Rational) -> i64 {
    if to.numerator() == 0 || from.denominator() == 0 {
        return 0;
    }
    let numerator = i128::from(value) * i128::from(from.numerator()) * i128::from(to.denominator());
    let denominator = i128::from(from.denominator()) * i128::from(to.numerator());
    (numerator / denominator).clamp(i128::from(i64::MIN), i128::from(i64::MAX)) as i64
}

fn assign_output_time_bases(transforms: &mut [Transform], output: &format::context::Output) {
    let output_bases: Vec<_> = output.streams().map(|stream| stream.time_base()).collect();
    for transform in transforms {
        match transform {
            Transform::Copy {
                dest,
                output_time_base,
                ..
            } => *output_time_base = output_bases[*dest],
            Transform::Video(video) => video.output_time_base = output_bases[video.output_index],
            Transform::Audio(audio) => audio.output_time_base = output_bases[audio.output_index],
        }
    }
}

fn remux_hls(
    reader: crate::s3::S3RangeReader,
    output_dir: &Path,
    start_seconds: f64,
    audio_only: bool,
    cancel: CancellationToken,
) -> Result<HlsResponse, String> {
    ffmpeg::init().map_err(|e| e.to_string())?;
    let mut input = open_input(reader, cancel.clone())?;
    let playlist = output_dir.join("index.m3u8");
    let mut output = ffmpeg::format::output_as(&playlist, "hls")
        .map_err(|e| format!("create HLS output: {e}"))?;
    let global_header = output
        .format()
        .flags()
        .contains(format::Flags::GLOBAL_HEADER);
    let mut mapping = vec![None; input.nb_streams() as usize];
    let mut transforms: Vec<Transform> = Vec::new();
    let mut video_codec = String::new();
    let mut audio_codec = String::new();
    let mut transcoding = false;
    let video_bitrate = if input.bit_rate() > 0 {
        input.bit_rate()
    } else {
        H264_DEFAULT_BITRATE
    };
    for stream in input.streams() {
        let medium = stream.parameters().medium();
        if (audio_only || medium != Type::Video) && medium != Type::Audio {
            continue;
        }
        if medium == Type::Video && !video_codec.is_empty()
            || medium == Type::Audio && !audio_codec.is_empty()
        {
            continue;
        }
        let id = stream.parameters().id();
        let codec_name = id.name().to_lowercase();
        let transform = if medium == Type::Video {
            video_codec = codec_name;
            if is_h264_codec(id) {
                copy_stream(&stream, &mut output, start_seconds)?
            } else {
                transcoding = true;
                Transform::Video(setup_video_encode(
                    &stream,
                    &mut output,
                    video_bitrate,
                    global_header,
                    start_seconds,
                )?)
            }
        } else {
            audio_codec = codec_name;
            if is_aac_codec(id) {
                copy_stream(&stream, &mut output, start_seconds)?
            } else {
                transcoding = true;
                Transform::Audio(setup_audio_encode(
                    &stream,
                    &mut output,
                    global_header,
                    start_seconds,
                )?)
            }
        };
        mapping[stream.index()] = Some(transforms.len());
        transforms.push(transform);
    }
    if (audio_only && audio_codec.is_empty()) || (!audio_only && video_codec.is_empty()) {
        return Err("required media stream is unavailable".into());
    }
    if start_seconds > 0.0 {
        let position = (start_seconds * f64::from(ffmpeg::ffi::AV_TIME_BASE)) as i64;
        input
            .seek(position, ..position)
            .map_err(|e| format!("seek HLS input: {e}"))?;
    }
    let mut options = Dictionary::new();
    options.set("hls_time", if audio_only { "6" } else { "4" });
    options.set("hls_list_size", "0");
    options.set("hls_playlist_type", "event");
    options.set("hls_flags", "temp_file+independent_segments");
    let segment = output_dir
        .join("segment-%06d.ts")
        .to_string_lossy()
        .into_owned();
    options.set("hls_segment_filename", &segment);
    output
        .write_header_with(options)
        .map_err(|e| format!("write HLS header: {e}"))?;
    assign_output_time_bases(&mut transforms, &output);
    let stop_at = start_seconds + 180.0;
    for (stream, mut packet) in input.packets() {
        if cancel.is_cancelled() {
            return Err("HLS cancelled".into());
        }
        let Some(index) = mapping[stream.index()] else {
            continue;
        };
        if packet
            .pts()
            .is_some_and(|pts| f64::from(stream.time_base()) * pts as f64 > stop_at)
        {
            break;
        }
        process_transform(&mut transforms[index], &mut packet, &mut output)?;
    }
    for transform in &mut transforms {
        flush_transform(transform, &mut output)?;
    }
    output
        .write_trailer()
        .map_err(|e| format!("finish HLS: {e}"))?;
    Ok(HlsResponse {
        duration_ms: (input.duration() / 1000).max(0),
        video_codec,
        audio_codec,
        transcoding,
        job_id: String::new(),
    })
}

