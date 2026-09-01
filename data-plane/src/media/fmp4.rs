struct ChannelWriter {
    tx: mpsc::Sender<Result<bytes::Bytes, io::Error>>,
    cancel: CancellationToken,
}

impl Write for ChannelWriter {
    fn write(&mut self, data: &[u8]) -> io::Result<usize> {
        if self.cancel.is_cancelled() {
            return Err(io::Error::new(io::ErrorKind::Interrupted, "fMP4 cancelled"));
        }
        self.tx
            .blocking_send(Ok(bytes::Bytes::copy_from_slice(data)))
            .map_err(|_| io::Error::new(io::ErrorKind::BrokenPipe, "fMP4 consumer closed"))?;
        Ok(data.len())
    }
    fn flush(&mut self) -> io::Result<()> {
        Ok(())
    }
}

fn remux_fmp4(
    reader: crate::s3::S3RangeReader,
    writer: ChannelWriter,
    start: f64,
    include_audio: bool,
    transcode_video: bool,
    transcode_audio: bool,
    cancel: CancellationToken,
) -> Result<(), String> {
    ffmpeg::init().map_err(|e| e.to_string())?;
    let mut input = open_input(reader, cancel.clone())?;
    if start > 0.0 {
        input
            .seek((start * 1_000_000.0) as i64, ..)
            .map_err(|e| e.to_string())?;
    }
    let output_io =
        StreamIo::from_write_with_capacity(writer, 256 << 10).map_err(|e| e.to_string())?;
    let mut output = ffmpeg::format::output_to_stream(output_io, Some("stream.mp4"), Some("mp4"))
        .map_err(|e| e.to_string())?;
    let global_header = output
        .format()
        .flags()
        .contains(format::Flags::GLOBAL_HEADER);
    let mut mapping = vec![None; input.nb_streams() as usize];
    let mut transforms: Vec<Transform> = Vec::new();
    let video_bitrate = if input.bit_rate() > 0 {
        input.bit_rate()
    } else {
        H264_DEFAULT_BITRATE
    };
    let mut have_video = false;
    let mut have_audio = false;
    for stream in input.streams() {
        let medium = stream.parameters().medium();
        let transform = if medium == Type::Video && !have_video {
            have_video = true;
            if transcode_video {
                Transform::Video(setup_video_encode(
                    &stream,
                    &mut output,
                    video_bitrate,
                    global_header,
                    start,
                )?)
            } else {
                copy_stream(&stream, &mut output, start)?
            }
        } else if medium == Type::Audio && include_audio && !have_audio {
            have_audio = true;
            if transcode_audio {
                Transform::Audio(setup_audio_encode(
                    &stream,
                    &mut output,
                    global_header,
                    start,
                )?)
            } else {
                copy_stream(&stream, &mut output, start)?
            }
        } else {
            continue;
        };
        mapping[stream.index()] = Some(transforms.len());
        transforms.push(transform);
    }
    if transforms.is_empty() {
        return Err("no MP4-compatible media stream".into());
    }
    let mut options = Dictionary::new();
    options.set("movflags", "frag_keyframe+empty_moov+default_base_moof");
    options.set("frag_duration", "2000000");
    options.set("min_frag_duration", "500000");
    output
        .write_header_with(options)
        .map_err(|e| e.to_string())?;
    assign_output_time_bases(&mut transforms, &output);
    for (stream, mut packet) in input.packets() {
        if cancel.is_cancelled() {
            return Err("fMP4 cancelled".into());
        }
        let Some(index) = mapping[stream.index()] else {
            continue;
        };
        process_transform(&mut transforms[index], &mut packet, &mut output)?;
    }
    for transform in &mut transforms {
        flush_transform(transform, &mut output)?;
    }
    output.write_trailer().map_err(|e| e.to_string())?;
    Ok(())
}

pub(crate) struct PreparedWebMedia {
    pub duration_ms: i64,
    pub video_codec: String,
    pub audio_codec: String,
    pub subtitles: Vec<(Subtitle, Vec<u8>)>,
}

fn web_video_supported(id: codec::Id) -> bool {
    matches!(id, codec::Id::H264 | codec::Id::HEVC)
}

// Some(false) means packet copy; Some(true) means AAC encode.
fn web_audio_mode(id: codec::Id) -> Option<bool> {
    match id {
        codec::Id::AAC => Some(false),
        codec::Id::FLAC | codec::Id::AC3 | codec::Id::EAC3 => Some(true),
        _ => None,
    }
}

// Narrow BT ingest remux: one video, one audio, text subtitles out-of-band.
// The seekable MP4 output permits faststart instead of the live MSE fragments.
pub(crate) fn prepare_web_media(
    input_path: &Path,
    output_path: &Path,
    cancel: CancellationToken,
) -> Result<PreparedWebMedia, String> {
    ffmpeg::init().map_err(|e| e.to_string())?;
    let mut input = format::input(input_path).map_err(|e| format!("open media: {e}"))?;
    let duration_ms = (input.duration() / 1000).max(0);
    let mut output =
        format::output_as(output_path, "mp4").map_err(|e| format!("create MP4: {e}"))?;
    let global_header = output
        .format()
        .flags()
        .contains(format::Flags::GLOBAL_HEADER);
    let mut mapping = vec![None; input.nb_streams() as usize];
    let mut transforms = Vec::new();
    let mut video_codec = String::new();
    let mut audio_codec = String::new();
    let mut subtitle_meta = Vec::new();
    for stream in input.streams() {
        let id = stream.parameters().id();
        let codec_name = id.name().to_ascii_lowercase();
        match stream.parameters().medium() {
            Type::Video if video_codec.is_empty() => {
                if !web_video_supported(id) {
                    return Err(format!("unsupported video codec: {codec_name}"));
                }
                video_codec = codec_name;
                let transform = copy_stream(&stream, &mut output, 0.0)?;
                if id == codec::Id::HEVC {
                    let dest = match transform {
                        Transform::Copy { dest, .. } => dest,
                        _ => unreachable!(),
                    };
                    if let Some(target) = output.stream_mut(dest) {
                        unsafe {
                            (*target.parameters().as_mut_ptr()).codec_tag =
                                u32::from_le_bytes(*b"hvc1");
                        }
                    }
                }
                mapping[stream.index()] = Some(transforms.len());
                transforms.push(transform);
            }
            Type::Audio if audio_codec.is_empty() => {
                let Some(transcode) = web_audio_mode(id) else {
                    return Err(format!("unsupported audio codec: {codec_name}"));
                };
                audio_codec = codec_name;
                let transform = if !transcode {
                    copy_stream(&stream, &mut output, 0.0)?
                } else {
                    setup_audio_encode(&stream, &mut output, global_header, 0.0)
                        .map(Transform::Audio)?
                };
                mapping[stream.index()] = Some(transforms.len());
                transforms.push(transform);
            }
            Type::Subtitle
                if matches!(
                    id,
                    codec::Id::ASS | codec::Id::SSA | codec::Id::SUBRIP | codec::Id::WEBVTT
                ) =>
            {
                let metadata = stream.metadata();
                subtitle_meta.push(Subtitle {
                    index: stream.index(),
                    codec: codec_name,
                    language: metadata.get("language").unwrap_or("").to_owned(),
                    title: metadata.get("title").unwrap_or("").to_owned(),
                    default: stream
                        .disposition()
                        .contains(ffmpeg::format::stream::Disposition::DEFAULT),
                    forced: stream
                        .disposition()
                        .contains(ffmpeg::format::stream::Disposition::FORCED),
                });
            }
            _ => {}
        }
    }
    if video_codec.is_empty() {
        return Err("unsupported video codec: none".into());
    }
    let mut options = Dictionary::new();
    options.set("movflags", "+faststart");
    output
        .write_header_with(options)
        .map_err(|e| format!("write MP4 header: {e}"))?;
    assign_output_time_bases(&mut transforms, &output);
    for (stream, mut packet) in input.packets() {
        if cancel.is_cancelled() {
            return Err("web media preparation cancelled".into());
        }
        if let Some(index) = mapping[stream.index()] {
            process_transform(&mut transforms[index], &mut packet, &mut output)?;
        }
    }
    for transform in &mut transforms {
        flush_transform(transform, &mut output)?;
    }
    output
        .write_trailer()
        .map_err(|e| format!("finish MP4: {e}"))?;
    let mut subtitles = Vec::new();
    for meta in subtitle_meta {
        let index = meta.index;
        let data = embedded_subtitle_path(input_path, index, cancel.clone())?;
        subtitles.push((meta, data));
    }
    Ok(PreparedWebMedia {
        duration_ms,
        video_codec,
        audio_codec,
        subtitles,
    })
}

fn embedded_subtitle_path(
    path: &Path,
    index: usize,
    cancel: CancellationToken,
) -> Result<Vec<u8>, String> {
    let mut input = format::input(path).map_err(|e| e.to_string())?;
    let stream = input
        .stream(index)
        .ok_or("subtitle stream index is out of range")?;
    let base = stream.time_base();
    let mut decoder = codec::context::Context::from_parameters(stream.parameters())
        .map_err(|e| e.to_string())?
        .decoder()
        .subtitle()
        .map_err(|e| e.to_string())?;
    let mut output = String::from("WEBVTT\n\n");
    for (packet_stream, packet) in input.packets() {
        if cancel.is_cancelled() {
            return Err("subtitle conversion cancelled".into());
        }
        if packet_stream.index() != index {
            continue;
        }
        let packet_start = packet
            .pts()
            .map(|pts| pts as f64 * f64::from(base))
            .unwrap_or_default();
        let packet_duration = packet.duration() as f64 * f64::from(base);
        let mut subtitle = ffmpeg::Subtitle::new();
        if !decoder
            .decode(&packet, &mut subtitle)
            .map_err(|e| e.to_string())?
        {
            continue;
        }
        let start = subtitle
            .pts()
            .map(|pts| pts as f64 / 1_000_000.0)
            .unwrap_or(packet_start)
            + subtitle.start() as f64 / 1000.0;
        let end = start
            + subtitle_cue_duration(
                subtitle.end().saturating_sub(subtitle.start()) as f64 / 1000.0,
                packet_duration,
            );
        let mut lines = Vec::new();
        let mut settings = "";
        for rect in subtitle.rects() {
            match rect {
                ffmpeg::subtitle::Rect::Text(v) => lines.push(v.get().to_owned()),
                ffmpeg::subtitle::Rect::Ass(v) => {
                    if settings.is_empty() {
                        settings = decoded_ass_vtt_settings(v.get());
                    }
                    lines.push(strip_decoded_ass(v.get()));
                }
                _ => {}
            }
        }
        if !lines.is_empty() {
            output.push_str(&format!(
                "{} --> {}{}\n{}\n\n",
                vtt_time(start),
                vtt_time(end),
                settings,
                lines.join("\n")
            ));
        }
        if output.len() > 16 << 20 {
            return Err("converted subtitle is too large".into());
        }
    }
    Ok(output.into_bytes())
}

pub(crate) fn external_subtitle_path(
    path: &Path,
    cancel: CancellationToken,
) -> Result<Vec<u8>, String> {
    ffmpeg::init().map_err(|e| e.to_string())?;
    let input = format::input(path).map_err(|e| format!("open external subtitle: {e}"))?;
    let index = input
        .streams()
        .best(Type::Subtitle)
        .ok_or("external subtitle contains no supported stream")?
        .index();
    drop(input);
    embedded_subtitle_path(path, index, cancel)
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::process::{Command, Stdio};

    #[test]
    fn near_black_detection_allows_visible_frames() {
        let black = vec![0_u8; 16 * 9 * 3];
        assert!(rgb_is_near_black(&black, 16 * 3, 16, 9));

        let mut visible = black;
        // More than 2% bright samples makes the deliberately coarse detector
        // accept the frame, while a tiny isolated compression speck does not.
        for index in 0..4 {
            visible[index * 3..index * 3 + 3].copy_from_slice(&[180, 180, 180]);
        }
        assert!(!rgb_is_near_black(&visible, 16 * 3, 16, 9));
    }
    #[test]
    fn rational_time_is_bounded() {
        assert_eq!(millis(90_000, Rational(1, 90_000)), 1000);
        assert_eq!(millis(-1, Rational(1, 1)), 0);
    }

    #[test]
    fn decoded_ass_uses_packet_duration_and_removes_packet_fields() {
        assert_eq!(subtitle_cue_duration(0.0, 2.84), 2.84);
        assert_eq!(subtitle_cue_duration(1.25, 2.84), 1.25);
        assert_eq!(
            strip_decoded_ass(r"2,0,Dial_CH,,0,0,0,,{\an8}你好,世界\N第二行"),
            "你好,世界\n第二行"
        );
        assert_eq!(
            decoded_ass_vtt_settings(r"2,0,Dial_CH,,0,0,0,,{\an8}顶部"),
            " line:10%"
        );
    }

    #[test]
    fn sidecar_language_metadata_uses_matroska_language_codes() {
        assert_eq!(external_subtitle_language("Movie.chs.ass"), "chs");
        assert_eq!(external_subtitle_language("Movie.zh-CN.srt"), "chs");
        assert_eq!(external_subtitle_language("Movie.zh_tw.vtt"), "cht");
        assert_eq!(external_subtitle_language("Movie.ja.ass"), "jpn");
        assert_eq!(external_subtitle_language("Movie.unknown.srt"), "und");
    }

    #[test]
    fn mkv_sidecar_remux_copies_av_and_appends_soft_subtitles() {
        if Command::new("ffmpeg")
            .arg("-version")
            .stdout(Stdio::null())
            .status()
            .is_err()
        {
            return;
        }
        let temp = tempfile::tempdir().unwrap();
        let original_ass = temp.path().join("original.ass");
        let japanese_ass = temp.path().join("Movie.ja.ass");
        let chinese_srt = temp.path().join("Movie.zh-CN.srt");
        let traditional_vtt = temp.path().join("Movie.zh-TW.vtt");
        let source = temp.path().join("Movie.mkv");
        let output = temp.path().join("remuxed.mkv");
        std::fs::write(&original_ass, "[Script Info]\nScriptType: v4.00+\n[Events]\nFormat: Layer, Start, End, Style, Name, MarginL, MarginR, MarginV, Effect, Text\nDialogue: 0,0:00:00.00,0:00:00.80,Default,,0,0,0,,original\n").unwrap();
        std::fs::write(&japanese_ass, "[Script Info]\nScriptType: v4.00+\n[Events]\nFormat: Layer, Start, End, Style, Name, MarginL, MarginR, MarginV, Effect, Text\nDialogue: 0,0:00:00.00,0:00:00.80,Default,,0,0,0,,日本語\n").unwrap();
        std::fs::write(&chinese_srt, "1\n00:00:00,000 --> 00:00:00,800\n中文\n").unwrap();
        std::fs::write(
            &traditional_vtt,
            "WEBVTT\n\n00:00:00.000 --> 00:00:00.800\n繁體\n",
        )
        .unwrap();
        let created = Command::new("ffmpeg")
            .args([
                "-hide_banner",
                "-loglevel",
                "error",
                "-f",
                "lavfi",
                "-i",
                "color=c=black:s=32x32:r=2:d=1",
                "-f",
                "lavfi",
                "-i",
                "sine=frequency=440:duration=1",
                "-i",
            ])
            .arg(&original_ass)
            .args([
                "-map", "0:v:0", "-map", "1:a:0", "-map", "2:s:0", "-c:v", "mpeg4", "-c:a", "aac",
                "-c:s", "ass", "-y",
            ])
            .arg(&source)
            .status()
            .unwrap();
        assert!(created.success());
        let source_probe = format::input(&source).unwrap();
        let source_av: Vec<_> = source_probe
            .streams()
            .filter(|stream| matches!(stream.parameters().medium(), Type::Video | Type::Audio))
            .map(|stream| stream.parameters().id())
            .collect();
        drop(source_probe);

        remux_mkv_sidecars(
            &source,
            &output,
            &[
                (4, "Movie.ja.ass".into(), japanese_ass),
                (5, "Movie.zh-CN.srt".into(), chinese_srt),
                (6, "Movie.zh-TW.vtt".into(), traditional_vtt),
            ],
        )
        .unwrap();
        let remuxed = format::input(&output).unwrap();
        let remuxed_av: Vec<_> = remuxed
            .streams()
            .filter(|stream| matches!(stream.parameters().medium(), Type::Video | Type::Audio))
            .map(|stream| stream.parameters().id())
            .collect();
        let subtitles: Vec<_> = remuxed
            .streams()
            .filter(|stream| stream.parameters().medium() == Type::Subtitle)
            .map(|stream| {
                (
                    stream.metadata().get("language").unwrap_or("").to_owned(),
                    stream.metadata().get("title").unwrap_or("").to_owned(),
                )
            })
            .collect();
        assert_eq!(remuxed_av, source_av);
        assert_eq!(subtitles.len(), 4);
        assert_eq!(subtitles[1], ("jpn".into(), "Movie.ja".into()));
        assert_eq!(subtitles[2], ("chs".into(), "Movie.zh-CN".into()));
        assert_eq!(subtitles[3], ("cht".into(), "Movie.zh-TW".into()));

        let before = std::fs::read(&source).unwrap();
        let invalid = temp.path().join("Movie.bad.ass");
        let failed_output = temp.path().join("failed.mkv");
        std::fs::write(&invalid, "not an ASS subtitle").unwrap();
        assert!(
            remux_mkv_sidecars(
                &source,
                &failed_output,
                &[(7, "Movie.bad.ass".into(), invalid)],
            )
            .is_err()
        );
        assert_eq!(std::fs::read(&source).unwrap(), before);
    }

    #[test]
    fn ass_conversion_discards_styles_and_keeps_basic_position() {
        let input = r#"[V4+ Styles]
Format: Name, Fontname, PrimaryColour, BackColour, BorderStyle, Outline, Shadow, Alignment
Style: Top,Arial,&H00000000,&H00FFFFFF,3,4,4,8
[Events]
Format: Layer, Start, End, Style, Name, MarginL, MarginR, MarginV, Effect, Text
Dialogue: 0,0:00:01.00,0:00:03.50,Top,,0,0,0,,{\c&H000000&\bord5\shad4}黑字{\p1}特效{\p0}\N第二行 <tag> & text
Dialogue: 0,0:00:04.00,0:00:05.00,Top,,0,0,0,,{\an2\3c&HFFFFFF&}底部
"#;
        let output = ass_to_vtt(input);
        assert!(
            output.contains(
                "00:00:01.000 --> 00:00:03.500 line:10%\n黑字\n第二行 &lt;tag&gt; &amp; text"
            ),
            "{output}"
        );
        assert!(
            output.contains("00:00:04.000 --> 00:00:05.000\n底部"),
            "{output}"
        );
        for discarded in [
            "PrimaryColour",
            "BackColour",
            "BorderStyle",
            "\\bord",
            "\\shad",
            "\\p1",
        ] {
            assert!(
                !output.contains(discarded),
                "style leaked into WebVTT: {output}"
            );
        }
    }

    #[test]
    fn web_ingest_codec_policy_is_narrow_and_copy_preserving() {
        assert!(web_video_supported(codec::Id::H264));
        assert!(web_video_supported(codec::Id::HEVC));
        assert!(!web_video_supported(codec::Id::VP9));
        assert_eq!(web_audio_mode(codec::Id::AAC), Some(false));
        assert_eq!(web_audio_mode(codec::Id::FLAC), Some(true));
        assert_eq!(web_audio_mode(codec::Id::AC3), Some(true));
        assert_eq!(web_audio_mode(codec::Id::EAC3), Some(true));
        assert_eq!(web_audio_mode(codec::Id::OPUS), None);
    }

    #[test]
    fn copied_packet_timestamp_reset_gets_a_stable_offset() {
        let mut offset = 0;
        let mut last = None;
        let mut first = Packet::empty();
        first.set_dts(Some(3690));
        first.set_pts(Some(3690));
        first.set_duration(40);
        normalize_copy_timestamps(&mut first, &mut offset, &mut last);
        assert_eq!(
            (first.dts(), first.pts(), offset),
            (Some(3690), Some(3690), 0)
        );

        let mut reset = Packet::empty();
        reset.set_dts(Some(0));
        reset.set_pts(Some(80));
        reset.set_duration(40);
        normalize_copy_timestamps(&mut reset, &mut offset, &mut last);
        assert_eq!(
            (reset.dts(), reset.pts(), offset),
            (Some(3730), Some(3810), 3730)
        );

        let mut following = Packet::empty();
        following.set_dts(Some(40));
        following.set_pts(Some(120));
        following.set_duration(40);
        normalize_copy_timestamps(&mut following, &mut offset, &mut last);
        assert_eq!(
            (following.dts(), following.pts(), offset),
            (Some(3770), Some(3850), 3730)
        );
    }

    #[test]
    fn web_ingest_handles_hevc_aac_eac3_and_two_ass_tracks() {
        if Command::new("ffmpeg")
            .arg("-version")
            .stdout(Stdio::null())
            .status()
            .is_err()
        {
            return;
        }
        let temp = tempfile::tempdir().unwrap();
        let ass = "[Script Info]\nScriptType: v4.00+\n[V4+ Styles]\nFormat: Name, Fontname, Fontsize, PrimaryColour, SecondaryColour, OutlineColour, BackColour, Bold, Italic, Underline, StrikeOut, ScaleX, ScaleY, Spacing, Angle, BorderStyle, Outline, Shadow, Alignment, MarginL, MarginR, MarginV, Encoding\nStyle: Default,Arial,20,&H00FFFFFF,&H000000FF,&H00000000,&H00000000,0,0,0,0,100,100,0,0,1,1,0,2,10,10,10,1\n[Events]\nFormat: Layer, Start, End, Style, Name, MarginL, MarginR, MarginV, Effect, Text\nDialogue: 0,0:00:00.00,0:00:00.80,Default,,0,0,0,,fixture\n";
        let ass_one = temp.path().join("one.ass");
        let ass_two = temp.path().join("two.ass");
        std::fs::write(&ass_one, ass).unwrap();
        std::fs::write(&ass_two, ass).unwrap();
        let input = temp.path().join("kaguya-layout.mkv");
        let status = Command::new("ffmpeg")
            .args([
                "-hide_banner",
                "-loglevel",
                "error",
                "-f",
                "lavfi",
                "-i",
                "testsrc2=size=128x72:rate=12",
                "-f",
                "lavfi",
                "-i",
                "sine=frequency=440:sample_rate=48000",
                "-f",
                "lavfi",
                "-i",
                "sine=frequency=660:sample_rate=48000",
            ])
            .arg("-i")
            .arg(&ass_one)
            .arg("-i")
            .arg(&ass_two)
            .args([
                "-t",
                "1",
                "-map",
                "0:v:0",
                "-map",
                "1:a:0",
                "-map",
                "2:a:0",
                "-map",
                "3:s:0",
                "-map",
                "4:s:0",
                "-c:v",
                "libx265",
                "-x265-params",
                "pools=1:frame-threads=1",
                "-pix_fmt",
                "yuv420p10le",
                "-c:a:0",
                "aac",
                "-c:a:1",
                "eac3",
                "-c:s",
                "ass",
                "-y",
            ])
            .arg(&input)
            .stdout(Stdio::null())
            .stderr(Stdio::null())
            .status()
            .unwrap();
        if !status.success() {
            return;
        }
        let output = temp.path().join("playback.mp4");
        let prepared = prepare_web_media(&input, &output, CancellationToken::new()).unwrap();
        assert_eq!(prepared.video_codec, "hevc");
        assert_eq!(prepared.audio_codec, "aac");
        assert_eq!(prepared.subtitles.len(), 2);
        let probe = format::input(&output).unwrap();
        let codecs: Vec<_> = probe.streams().map(|s| s.parameters().id()).collect();
        assert_eq!(codecs, [codec::Id::HEVC, codec::Id::AAC]);
    }
}

