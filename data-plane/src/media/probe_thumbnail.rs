fn probe_blocking(
    reader: crate::s3::S3RangeReader,
    cancel: CancellationToken,
) -> Result<ProbeResponse, String> {
    ffmpeg::init().map_err(|e| e.to_string())?;
    let input = open_input(reader, cancel)?;
    let mut result = ProbeResponse {
        duration_ms: (input.duration() / 1000).max(0),
        container: input.format().name().to_lowercase(),
        bitrate: input.bit_rate().max(0),
        ..Default::default()
    };
    for stream in input.streams() {
        let parameters = stream.parameters();
        let codec = parameters.id().name().to_lowercase();
        match parameters.medium() {
            Type::Video if result.video_codec.is_empty() => {
                result.video_codec = codec;
                result.frame_rate = stream.avg_frame_rate().to_string();
                unsafe {
                    let raw = parameters.as_ptr();
                    result.video_level = (*raw).level;
                    result.video_profile = match (result.video_codec.as_str(), (*raw).profile) {
                        ("h264", 66) => "Baseline",
                        ("h264", 77) => "Main",
                        ("h264", 100) => "High",
                        ("hevc", 2) => "Main 10",
                        ("hevc", 1) => "Main",
                        _ => "",
                    }
                    .to_string();
                }
                if let Ok(context) = ffmpeg::codec::context::Context::from_parameters(parameters)
                    && let Ok(decoder) = context.decoder().video()
                {
                    result.width = decoder.width();
                    result.height = decoder.height();
                }
            }
            Type::Audio if result.audio_codec.is_empty() => result.audio_codec = codec,
            Type::Subtitle => {
                let metadata = stream.metadata();
                let disposition = stream.disposition();
                result.subtitles.push(Subtitle {
                    index: stream.index(),
                    codec,
                    language: metadata.get("language").unwrap_or_default().to_string(),
                    title: metadata.get("title").unwrap_or_default().to_string(),
                    default: disposition.contains(ffmpeg::format::stream::Disposition::DEFAULT),
                    forced: disposition.contains(ffmpeg::format::stream::Disposition::FORCED),
                });
            }
            _ => {}
        }
    }
    for (index, chapter) in input.chapters().enumerate() {
        let title = chapter
            .metadata()
            .get("title")
            .map(str::trim)
            .filter(|s| !s.is_empty())
            .map(str::to_string)
            .unwrap_or_else(|| format!("Chapter {}", index + 1));
        result.chapters.push(Chapter {
            title,
            start_ms: millis(chapter.start(), chapter.time_base()),
            end_ms: millis(chapter.end(), chapter.time_base()),
        });
    }
    Ok(result)
}

fn thumbnail_blocking(
    reader: crate::s3::S3RangeReader,
    max_dimension: u32,
    attached_picture_only: bool,
    cancel: CancellationToken,
) -> Result<Vec<u8>, String> {
    ffmpeg::init().map_err(|e| e.to_string())?;
    let mut input = open_input(reader, cancel.clone())?;
    let stream = if attached_picture_only {
        input.streams().find(|stream| {
            stream.parameters().medium() == Type::Video
                && stream
                    .disposition()
                    .contains(ffmpeg::format::stream::Disposition::ATTACHED_PIC)
        })
    } else {
        input.streams().best(Type::Video)
    }
    .ok_or(if attached_picture_only {
        "media has no attached picture"
    } else {
        "media has no video stream"
    })?;
    let stream_index = stream.index();
    let context = ffmpeg::codec::context::Context::from_parameters(stream.parameters())
        .map_err(|e| e.to_string())?;
    let mut decoder = context.decoder().video().map_err(|e| e.to_string())?;
    let (width, height) = fit(decoder.width(), decoder.height(), max_dimension);
    let mut scaler = Scaler::get(
        decoder.format(),
        decoder.width(),
        decoder.height(),
        ffmpeg::format::Pixel::RGB24,
        width,
        height,
        Flags::BILINEAR,
    )
    .map_err(|e| e.to_string())?;
    let mut decoded = Video::empty();
    let mut rgb = Video::empty();
    if attached_picture_only {
        let mut found = false;
        for (packet_stream, packet) in input.packets() {
            if cancel.is_cancelled() {
                return Err("thumbnail cancelled".into());
            }
            if packet_stream.index() != stream_index {
                continue;
            }
            decoder.send_packet(&packet).map_err(|e| e.to_string())?;
            if decoder.receive_frame(&mut decoded).is_ok() {
                scaler.run(&decoded, &mut rgb).map_err(|e| e.to_string())?;
                found = true;
                break;
            }
        }
        if !found {
            return Err("media has no attached picture".into());
        }
        return encode_thumbnail_rgb(&rgb, width, height);
    }
    // libav exposes the probed container duration in AV_TIME_BASE units. Seek
    // before decoding so only the GOP around the requested frame is read.
    // Later positions avoid black leaders and title cards; stop at the first
    // frame that is not overwhelmingly near-black.
    let duration = input.duration();
    let positions = if duration > 0 {
        [
            duration * 20 / 100,
            duration * 35 / 100,
            duration * 50 / 100,
        ]
    } else {
        [1_000_000, 2_000_000, 3_000_000]
    };
    let mut found = false;
    for seek_timestamp in positions {
        if cancel.is_cancelled() {
            return Err("thumbnail cancelled".into());
        }
        if input.seek(seek_timestamp, ..seek_timestamp).is_err() {
            continue;
        }
        decoder.flush();
        for (packet_stream, packet) in input.packets() {
            if cancel.is_cancelled() {
                return Err("thumbnail cancelled".into());
            }
            if packet_stream.index() != stream_index {
                continue;
            }
            if decoder.send_packet(&packet).is_err() {
                continue;
            }
            if decoder.receive_frame(&mut decoded).is_ok() {
                scaler.run(&decoded, &mut rgb).map_err(|e| e.to_string())?;
                if !frame_is_near_black(&rgb, width, height) {
                    found = true;
                }
                break;
            }
        }
        if found {
            break;
        }
    }
    if !found {
        return Err("video produced no usable thumbnail frame".into());
    }
    encode_thumbnail_rgb(&rgb, width, height)
}

fn encode_thumbnail_rgb(rgb: &Video, width: u32, height: u32) -> Result<Vec<u8>, String> {
    let row_bytes = width as usize * 3;
    let stride = rgb.stride(0);
    let plane = rgb.data(0);
    let packed;
    let pixels = if stride == row_bytes {
        &plane[..row_bytes * height as usize]
    } else {
        packed = (0..height as usize)
            .flat_map(|row| {
                plane[row * stride..row * stride + row_bytes]
                    .iter()
                    .copied()
            })
            .collect::<Vec<_>>();
        packed.as_slice()
    };
    let mut jpeg = Vec::with_capacity((width * height) as usize / 2);
    JpegEncoder::new_with_quality(&mut jpeg, 82)
        .encode(pixels, width, height, ExtendedColorType::Rgb8)
        .map_err(|e| e.to_string())?;
    if jpeg.len() > 8 << 20 {
        return Err("thumbnail exceeds output limit".into());
    }
    Ok(jpeg)
}

// Sample the RGB frame on a coarse grid. Treat it as near-black only when at
// least 98% of samples have very low luma; this deliberately simple check is
// cheap and avoids rejecting ordinary dark scenes with visible highlights.
fn frame_is_near_black(frame: &Video, width: u32, height: u32) -> bool {
    let stride = frame.stride(0);
    let plane = frame.data(0);
    rgb_is_near_black(plane, stride, width, height)
}

fn rgb_is_near_black(plane: &[u8], stride: usize, width: u32, height: u32) -> bool {
    let step_x = (width / 64).max(1) as usize;
    let step_y = (height / 36).max(1) as usize;
    let mut samples = 0usize;
    let mut black = 0usize;
    for y in (0..height as usize).step_by(step_y) {
        for x in (0..width as usize).step_by(step_x) {
            let offset = y * stride + x * 3;
            if offset + 2 >= plane.len() {
                continue;
            }
            samples += 1;
            let luma = (u16::from(plane[offset]) * 54
                + u16::from(plane[offset + 1]) * 183
                + u16::from(plane[offset + 2]) * 19)
                / 256;
            if luma < 16 {
                black += 1;
            }
        }
    }
    samples > 0 && black * 100 >= samples * 98
}

fn fit(width: u32, height: u32, max_dimension: u32) -> (u32, u32) {
    if width == 0 || height == 0 {
        return (max_dimension, max_dimension);
    }
    if width <= max_dimension && height <= max_dimension {
        return (width, height);
    }
    if width >= height {
        (
            max_dimension,
            ((u64::from(height) * u64::from(max_dimension) / u64::from(width)).max(1) as u32),
        )
    } else {
        (
            ((u64::from(width) * u64::from(max_dimension) / u64::from(height)).max(1) as u32),
            max_dimension,
        )
    }
}


