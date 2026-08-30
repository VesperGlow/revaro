use std::{
    env,
    path::{Path, PathBuf},
};

use axum::{Json, extract::State};
use ffmpeg::{
    ChannelLayout, Dictionary, Packet, Rational, codec, encoder, filter, format, frame, media,
    packet,
};
use ffmpeg_next as ffmpeg;
use image::GenericImageView;
use serde::{Deserialize, Serialize};
use tokio::task;
use tokio_util::sync::CancellationToken;

use crate::{AppState, audio_fifo::AudioFrameAccumulator, error::ApiError};

struct TempOutput(std::path::PathBuf);

impl Drop for TempOutput {
    fn drop(&mut self) {
        let _ = std::fs::remove_file(&self.0);
    }
}

#[derive(Deserialize)]
pub struct MergeRequest {
    inputs: Vec<String>,
    input_names: Vec<String>,
    output: String,
    format: String,
    title: Option<String>,
}

#[derive(Serialize)]
pub struct MergeResponse {
    durations_ms: Vec<i64>,
    size: u64,
}

#[derive(Deserialize)]
pub struct DecorateRequest {
    input: String,
    cover: Option<String>,
    subtitle: Option<String>,
}

pub async fn merge(
    State(state): State<AppState>,
    Json(q): Json<MergeRequest>,
) -> Result<Json<MergeResponse>, ApiError> {
    if q.inputs.len() < 2 || q.inputs.len() > 256 || q.input_names.len() != q.inputs.len() {
        return Err(ApiError::bad_request(
            "audio merge requires 2 to 256 inputs",
        ));
    }
    let root = tokio::fs::canonicalize(env::var("APP_WORK_DIR").map_err(ApiError::internal)?)
        .await
        .map_err(ApiError::internal)?;
    let mut inputs = Vec::with_capacity(q.inputs.len());
    for raw in q.inputs {
        let path = tokio::fs::canonicalize(raw)
            .await
            .map_err(|e| ApiError::bad_request(e.to_string()))?;
        if !path.starts_with(&root) {
            return Err(ApiError::bad_request("audio input is outside work root"));
        }
        inputs.push(path);
    }
    let output = PathBuf::from(q.output);
    let parent = output
        .parent()
        .ok_or_else(|| ApiError::bad_request("invalid audio output path"))?;
    let parent = tokio::fs::canonicalize(parent)
        .await
        .map_err(|e| ApiError::bad_request(e.to_string()))?;
    if !parent.starts_with(&root) {
        return Err(ApiError::bad_request("audio output is outside work root"));
    }
    let output = parent.join(
        output
            .file_name()
            .ok_or_else(|| ApiError::bad_request("invalid audio output filename"))?,
    );
    let _permit = state
        .media_heavy_slots
        .clone()
        .acquire_owned()
        .await
        .map_err(ApiError::internal)?;
    let cancel = state.shutdown.child_token();
    let mut guard = CancelOnDrop(cancel.clone());
    let result = task::spawn_blocking(move || {
        merge_blocking(
            &inputs,
            &q.input_names,
            &output,
            &q.format,
            q.title.as_deref(),
            cancel,
        )
    })
    .await
    .map_err(ApiError::internal)?
    .map_err(ApiError::bad_request)?;
    guard.disarm();
    Ok(Json(result))
}

pub async fn decorate(
    State(state): State<AppState>,
    Json(q): Json<DecorateRequest>,
) -> Result<Json<MergeResponse>, ApiError> {
    let root = tokio::fs::canonicalize(env::var("APP_WORK_DIR").map_err(ApiError::internal)?)
        .await
        .map_err(ApiError::internal)?;
    let input = checked_path(&root, &q.input).await?;
    let cover = match q.cover {
        Some(path) => Some(checked_path(&root, &path).await?),
        None => None,
    };
    let subtitle = match q.subtitle {
        Some(path) => Some(checked_path(&root, &path).await?),
        None => None,
    };
    if cover.is_none() && subtitle.is_none() {
        return Err(ApiError::bad_request("audio decoration has no assets"));
    }
    let _permit = state
        .media_heavy_slots
        .clone()
        .acquire_owned()
        .await
        .map_err(ApiError::internal)?;
    let cancel = state.shutdown.child_token();
    let mut guard = CancelOnDrop(cancel.clone());
    let result = task::spawn_blocking(move || {
        decorate_blocking(&input, cover.as_deref(), subtitle.as_deref(), cancel)
    })
    .await
    .map_err(ApiError::internal)?
    .map_err(ApiError::bad_request)?;
    guard.disarm();
    Ok(Json(result))
}

async fn checked_path(root: &Path, raw: &str) -> Result<PathBuf, ApiError> {
    let path = tokio::fs::canonicalize(raw)
        .await
        .map_err(|e| ApiError::bad_request(e.to_string()))?;
    if !path.starts_with(root) {
        return Err(ApiError::bad_request("audio asset is outside work root"));
    }
    Ok(path)
}

struct CancelOnDrop(CancellationToken);
impl CancelOnDrop {
    fn disarm(&mut self) {
        self.0 = CancellationToken::new();
    }
}
impl Drop for CancelOnDrop {
    fn drop(&mut self) {
        self.0.cancel();
    }
}

struct AudioOutput {
    encoder: codec::encoder::Audio,
    accumulator: AudioFrameAccumulator,
    time_base: Rational,
    next_pts: i64,
}

impl AudioOutput {
    fn drain(&mut self, output: &mut format::context::Output) -> Result<(), String> {
        let mut packet = Packet::empty();
        while self.encoder.receive_packet(&mut packet).is_ok() {
            packet.set_stream(0);
            packet.rescale_ts(self.encoder.time_base(), self.time_base);
            packet
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
}

fn merge_blocking(
    inputs: &[PathBuf],
    input_names: &[String],
    output_path: &Path,
    output_format: &str,
    title: Option<&str>,
    cancel: CancellationToken,
) -> Result<MergeResponse, String> {
    ffmpeg::init().map_err(|e| e.to_string())?;
    let mut durations = Vec::with_capacity(inputs.len());
    let mut source_layout: Option<ChannelLayout> = None;
    let mut source_rate: Option<u32> = None;
    let mut uniform_layout = true;
    let mut uniform_rate = true;
    for path in inputs {
        let input = format::input(path).map_err(|e| format!("open audio input: {e}"))?;
        let duration = (input.duration() / 1000).max(0);
        let stream = input.streams().best(media::Type::Audio);
        if duration == 0 || stream.is_none() {
            return Err("audio input has no usable duration or audio stream".into());
        }
        let decoder = codec::context::Context::from_parameters(stream.unwrap().parameters())
            .map_err(|e| e.to_string())?
            .decoder()
            .audio()
            .map_err(|e| e.to_string())?;
        let layout = if decoder.channel_layout().is_empty() {
            ChannelLayout::default(decoder.channels().into())
        } else {
            decoder.channel_layout()
        };
        let rate = decoder.rate();
        if source_layout.is_some_and(|value| value != layout) {
            uniform_layout = false;
        }
        if source_rate.is_some_and(|value| value != rate) {
            uniform_rate = false;
        }
        source_layout.get_or_insert(layout);
        source_rate.get_or_insert(rate);
        durations.push(duration);
    }
    let codec_id = match output_format {
        "flac" => codec::Id::FLAC,
        "alac" => codec::Id::ALAC,
        "aac" => codec::Id::AAC,
        _ => return Err("unsupported audio output format".into()),
    };
    let codec = encoder::find(codec_id)
        .ok_or_else(|| format!("{output_format} encoder is unavailable"))?
        .audio()
        .map_err(|e| e.to_string())?;
    let mut output =
        format::output(output_path).map_err(|e| format!("create audio output: {e}"))?;
    let global = output
        .format()
        .flags()
        .contains(format::Flags::GLOBAL_HEADER);
    let mut stream = output.add_stream(codec).map_err(|e| e.to_string())?;
    let mut context = codec::context::Context::new_with_codec(*codec)
        .encoder()
        .audio()
        .map_err(|e| e.to_string())?;
    let layout = if uniform_layout {
        source_layout.unwrap_or(ChannelLayout::STEREO)
    } else {
        ChannelLayout::STEREO
    };
    let rate = if uniform_rate {
        source_rate.unwrap_or(48_000)
    } else {
        48_000
    };
    context.set_rate(rate as i32);
    context.set_channel_layout(layout);
    context.set_format(
        codec
            .formats()
            .ok_or("encoder exposes no sample formats")?
            .next()
            .ok_or("encoder exposes no sample formats")?,
    );
    context.set_bit_rate(if output_format == "aac" { 256_000 } else { 0 });
    context.set_time_base((1, rate as i32));
    if global {
        context.set_flags(codec::Flags::GLOBAL_HEADER);
    }
    let opened = context
        .open_as(codec)
        .map_err(|e| format!("open audio encoder: {e}"))?;
    stream.set_parameters(&opened);
    stream.set_time_base((1, rate as i32));
    let output_time_base = stream.time_base();
    if let Some(value) = title {
        let mut metadata = Dictionary::new();
        metadata.set("title", value);
        output.set_metadata(metadata);
    }
    let mut chapter_start = 0i64;
    for (index, (name, duration)) in input_names.iter().zip(&durations).enumerate() {
        let chapter_end = chapter_start
            .checked_add(*duration)
            .ok_or("audio chapter duration overflow")?;
        output
            .add_chapter(index as i64, (1, 1000), chapter_start, chapter_end, name)
            .map_err(|e| format!("add audio chapter: {e}"))?;
        chapter_start = chapter_end;
    }
    output
        .write_header()
        .map_err(|e| format!("write audio header: {e}"))?;
    let accumulator = AudioFrameAccumulator::new(&opened)?;
    let mut target = AudioOutput {
        encoder: opened,
        accumulator,
        time_base: output_time_base,
        next_pts: 0,
    };
    for path in inputs {
        if cancel.is_cancelled() {
            return Err("audio merge cancelled".into());
        }
        let mut input = format::input(path).map_err(|e| format!("open audio input: {e}"))?;
        let stream = input
            .streams()
            .best(media::Type::Audio)
            .ok_or("audio input has no audio stream")?;
        let stream_index = stream.index();
        let mut decoder = codec::context::Context::from_parameters(stream.parameters())
            .map_err(|e| e.to_string())?
            .decoder()
            .audio()
            .map_err(|e| e.to_string())?;
        let mut graph = audio_filter(&decoder, &target.encoder)?;
        for (packet_stream, packet) in input.packets() {
            if cancel.is_cancelled() {
                return Err("audio merge cancelled".into());
            }
            if packet_stream.index() != stream_index {
                continue;
            }
            decoder.send_packet(&packet).map_err(|e| e.to_string())?;
            process_audio_frames(&mut decoder, &mut graph, &mut target, &mut output)?;
        }
        decoder.send_eof().map_err(|e| e.to_string())?;
        process_audio_frames(&mut decoder, &mut graph, &mut target, &mut output)?;
        graph
            .get("in")
            .ok_or("audio filter input vanished")?
            .source()
            .flush()
            .map_err(|e| e.to_string())?;
        process_filtered(&mut graph, &mut target, &mut output)?;
    }
    target.encode_accumulated(&mut output, true)?;
    target.encoder.send_eof().map_err(|e| e.to_string())?;
    target.drain(&mut output)?;
    output
        .write_trailer()
        .map_err(|e| format!("finish audio output: {e}"))?;
    let size = std::fs::metadata(output_path)
        .map_err(|e| e.to_string())?
        .len();
    if size == 0 {
        return Err("audio encoder produced an empty file".into());
    }
    Ok(MergeResponse {
        durations_ms: durations,
        size,
    })
}

fn decorate_blocking(
    input_path: &Path,
    cover_path: Option<&Path>,
    subtitle_path: Option<&Path>,
    cancel: CancellationToken,
) -> Result<MergeResponse, String> {
    ffmpeg::init().map_err(|e| e.to_string())?;
    let mut input = format::input(input_path).map_err(|e| format!("open audio master: {e}"))?;
    let audio_stream = input
        .streams()
        .best(media::Type::Audio)
        .ok_or("audio master has no audio stream")?;
    let audio_index = audio_stream.index();
    let audio_base = audio_stream.time_base();
    let chapters: Vec<_> = input
        .chapters()
        .map(|chapter| {
            (
                chapter.id(),
                chapter.time_base(),
                chapter.start(),
                chapter.end(),
                chapter
                    .metadata()
                    .get("title")
                    .unwrap_or_default()
                    .to_owned(),
            )
        })
        .collect();
    let extension = input_path.extension().unwrap_or_default().to_string_lossy();
    let temp = input_path.with_file_name(format!(
        ".{}.decorated.{extension}",
        input_path.file_name().unwrap_or_default().to_string_lossy()
    ));
    let _temp_output = TempOutput(temp.clone());
    let mut output = format::output(&temp).map_err(|e| format!("create decorated audio: {e}"))?;
    let mut audio_out = output
        .add_stream(encoder::find(codec::Id::None))
        .map_err(|e| e.to_string())?;
    audio_out.set_parameters(audio_stream.parameters());
    unsafe {
        (*audio_out.parameters().as_mut_ptr()).codec_tag = 0;
    }
    audio_out.set_time_base(audio_base);
    let audio_output_index = audio_out.index();

    let mut cover_packet = if let Some(path) = cover_path {
        let bytes = std::fs::read(path).map_err(|e| format!("read audio cover: {e}"))?;
        let image =
            image::load_from_memory(&bytes).map_err(|e| format!("decode audio cover: {e}"))?;
        let (width, height) = image.dimensions();
        let mut parameters = codec::Parameters::new();
        parameters.set_medium(media::Type::Video);
        parameters.set_id(codec::Id::MJPEG);
        unsafe {
            (*parameters.as_mut_ptr()).width = width as i32;
            (*parameters.as_mut_ptr()).height = height as i32;
        }
        let mut stream = output
            .add_stream(encoder::find(codec::Id::None))
            .map_err(|e| e.to_string())?;
        stream.set_parameters(parameters);
        stream.set_time_base((1, 1000));
        unsafe {
            (*stream.as_mut_ptr()).disposition |= ffmpeg::ffi::AV_DISPOSITION_ATTACHED_PIC;
        }
        let mut packet = Packet::copy(&bytes);
        packet.set_stream(stream.index());
        packet.set_pts(Some(0));
        packet.set_dts(Some(0));
        packet.set_flags(packet::Flags::KEY);
        Some(packet)
    } else {
        None
    };

    let subtitle_cues = match subtitle_path {
        Some(path) => parse_webvtt(
            &std::fs::read_to_string(path).map_err(|e| format!("read merged subtitle: {e}"))?,
        )?,
        None => Vec::new(),
    };
    let subtitle_output_index = if subtitle_cues.is_empty() {
        None
    } else {
        let mut parameters = codec::Parameters::new();
        parameters.set_medium(media::Type::Subtitle);
        parameters.set_id(codec::Id::MOV_TEXT);
        let mut target = output
            .add_stream(encoder::find(codec::Id::None))
            .map_err(|e| e.to_string())?;
        target.set_parameters(parameters);
        target.set_time_base((1, 1000));
        Some(target.index())
    };
    for (id, base, start, end, title) in chapters {
        output
            .add_chapter(id, base, start, end, title)
            .map_err(|e| format!("copy audio chapter: {e}"))?;
    }
    output
        .write_header()
        .map_err(|e| format!("write decorated audio header: {e}"))?;
    if let Some(packet) = cover_packet.as_mut() {
        packet
            .write_interleaved(&mut output)
            .map_err(|e| e.to_string())?;
    }
    if let Some(index) = subtitle_output_index {
        for (start, end, text) in subtitle_cues {
            let bytes = text.as_bytes();
            if bytes.len() > u16::MAX as usize {
                return Err("audio subtitle cue is too large".into());
            }
            let mut payload = Vec::with_capacity(bytes.len() + 2);
            payload.extend_from_slice(&(bytes.len() as u16).to_be_bytes());
            payload.extend_from_slice(bytes);
            let mut packet = Packet::copy(&payload);
            packet.set_stream(index);
            packet.set_pts(Some(start));
            packet.set_dts(Some(start));
            packet.set_duration(end.saturating_sub(start));
            packet.set_flags(packet::Flags::KEY);
            packet
                .write_interleaved(&mut output)
                .map_err(|e| e.to_string())?;
        }
    }
    for (stream, mut packet) in input.packets() {
        if cancel.is_cancelled() {
            return Err("audio decoration cancelled".into());
        }
        if stream.index() != audio_index {
            continue;
        }
        let target_base = output
            .stream(audio_output_index)
            .ok_or("decorated audio stream vanished")?
            .time_base();
        packet.rescale_ts(audio_base, target_base);
        packet.set_stream(audio_output_index);
        packet.set_position(-1);
        packet
            .write_interleaved(&mut output)
            .map_err(|e| e.to_string())?;
    }
    output
        .write_trailer()
        .map_err(|e| format!("finish decorated audio: {e}"))?;
    std::fs::rename(&temp, input_path).map_err(|e| format!("replace decorated audio: {e}"))?;
    let size = std::fs::metadata(input_path)
        .map_err(|e| e.to_string())?
        .len();
    Ok(MergeResponse {
        durations_ms: Vec::new(),
        size,
    })
}

fn parse_webvtt(input: &str) -> Result<Vec<(i64, i64, String)>, String> {
    let normalized = input.trim_start_matches('\u{feff}').replace("\r\n", "\n");
    let mut cues = Vec::new();
    for block in normalized.split("\n\n") {
        let lines: Vec<_> = block.lines().collect();
        let Some(position) = lines.iter().position(|line| line.contains("-->")) else {
            continue;
        };
        let (start, end) = lines[position]
            .split_once("-->")
            .ok_or("invalid WebVTT timing")?;
        let end = end
            .split_whitespace()
            .next()
            .ok_or("invalid WebVTT end time")?;
        let text = lines[position + 1..].join("\n");
        if text.is_empty() {
            continue;
        }
        let start = parse_vtt_millis(start.trim())?;
        let end = parse_vtt_millis(end.trim())?;
        if end <= start {
            return Err("invalid WebVTT cue duration".into());
        }
        cues.push((start, end, text));
    }
    Ok(cues)
}

fn parse_vtt_millis(value: &str) -> Result<i64, String> {
    let pieces: Vec<_> = value.split(':').collect();
    let (hours, minutes, seconds) = match pieces.as_slice() {
        [minutes, seconds] => (0, *minutes, *seconds),
        [hours, minutes, seconds] => (
            hours.parse::<i64>().map_err(|_| "invalid WebVTT hour")?,
            *minutes,
            *seconds,
        ),
        _ => return Err("invalid WebVTT timestamp".into()),
    };
    let minutes = minutes
        .parse::<i64>()
        .map_err(|_| "invalid WebVTT minute")?;
    let (seconds, millis) = seconds.split_once('.').ok_or("invalid WebVTT second")?;
    let seconds = seconds
        .parse::<i64>()
        .map_err(|_| "invalid WebVTT second")?;
    if millis.is_empty() || millis.len() > 3 || !millis.bytes().all(|byte| byte.is_ascii_digit()) {
        return Err("invalid WebVTT milliseconds".into());
    }
    let millis = format!("{millis:0<3}")
        .parse::<i64>()
        .map_err(|_| "invalid WebVTT milliseconds")?;
    Ok(((hours * 60 + minutes) * 60 + seconds) * 1000 + millis)
}

fn audio_filter(
    decoder: &codec::decoder::Audio,
    encoder: &codec::encoder::Audio,
) -> Result<filter::Graph, String> {
    let mut graph = filter::Graph::new();
    let input_layout = if decoder.channel_layout().is_empty() {
        ChannelLayout::default(decoder.channels().into())
    } else {
        decoder.channel_layout()
    };
    let args = format!(
        "time_base={}:sample_rate={}:sample_fmt={}:channel_layout=0x{:x}",
        decoder.time_base(),
        decoder.rate(),
        decoder.format().name(),
        input_layout.bits()
    );
    graph
        .add(
            &filter::find("abuffer").ok_or("abuffer filter unavailable")?,
            "in",
            &args,
        )
        .map_err(|e| e.to_string())?;
    graph
        .add(
            &filter::find("abuffersink").ok_or("abuffersink filter unavailable")?,
            "out",
            "",
        )
        .map_err(|e| e.to_string())?;
    {
        let mut sink = graph.get("out").ok_or("audio filter output unavailable")?;
        sink.set_sample_format(encoder.format());
        sink.set_channel_layout(encoder.channel_layout());
        sink.set_sample_rate(encoder.rate());
    }
    graph
        .output("in", 0)
        .map_err(|e| e.to_string())?
        .input("out", 0)
        .map_err(|e| e.to_string())?
        .parse("anull")
        .map_err(|e| e.to_string())?;
    graph.validate().map_err(|e| e.to_string())?;
    if let Some(codec) = encoder.codec()
        && !codec
            .capabilities()
            .contains(codec::capabilities::Capabilities::VARIABLE_FRAME_SIZE)
    {
        graph
            .get("out")
            .ok_or("audio filter output unavailable")?
            .sink()
            .set_frame_size(encoder.frame_size());
    }
    Ok(graph)
}

fn process_audio_frames(
    decoder: &mut codec::decoder::Audio,
    graph: &mut filter::Graph,
    target: &mut AudioOutput,
    output: &mut format::context::Output,
) -> Result<(), String> {
    let mut decoded = frame::Audio::empty();
    while decoder.receive_frame(&mut decoded).is_ok() {
        graph
            .get("in")
            .ok_or("audio filter input unavailable")?
            .source()
            .add(&decoded)
            .map_err(|e| e.to_string())?;
        process_filtered(graph, target, output)?;
    }
    Ok(())
}

fn process_filtered(
    graph: &mut filter::Graph,
    target: &mut AudioOutput,
    output: &mut format::context::Output,
) -> Result<(), String> {
    let mut filtered = frame::Audio::empty();
    while graph
        .get("out")
        .ok_or("audio filter output unavailable")?
        .sink()
        .frame(&mut filtered)
        .is_ok()
    {
        target.accumulator.push(&filtered)?;
        target.encode_accumulated(output, false)?;
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parses_merged_webvtt_for_mov_text() {
        let cues = parse_webvtt(
            "WEBVTT\n\nfirst\n00:00:00.020 --> 00:00:00.300\nhello\n\n00:01.500 --> 00:02.000\nworld",
        )
        .unwrap();
        assert_eq!(
            cues,
            vec![(20, 300, "hello".into()), (1500, 2000, "world".into())]
        );
    }
}
