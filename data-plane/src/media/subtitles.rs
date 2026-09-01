fn subtitle_blocking(
    reader: crate::s3::S3RangeReader,
    format: Option<&str>,
    stream_index: Option<usize>,
    cancel: CancellationToken,
) -> Result<Vec<u8>, String> {
    const MAX: usize = 16 << 20;
    if let Some(index) = stream_index {
        return embedded_subtitle(reader, index, cancel);
    }
    let mut raw = Vec::new();
    reader
        .take((MAX + 1) as u64)
        .read_to_end(&mut raw)
        .map_err(|e| e.to_string())?;
    if raw.len() > MAX {
        return Err("subtitle is too large".into());
    }
    let text = String::from_utf8(raw).map_err(|_| "subtitle is not valid UTF-8")?;
    let format = format
        .unwrap_or("vtt")
        .trim_start_matches('.')
        .to_ascii_lowercase();
    let output = match format.as_str() {
        "vtt" => {
            if !text.trim_start().starts_with("WEBVTT") {
                return Err("invalid WebVTT header".into());
            }
            text
        }
        "srt" => srt_to_vtt(&text),
        "ass" | "ssa" => ass_to_vtt(&text),
        _ => return Err("unsupported subtitle format".into()),
    };
    if output.len() > MAX {
        return Err("converted subtitle is too large".into());
    }
    Ok(output.into_bytes())
}

fn embedded_subtitle(
    reader: crate::s3::S3RangeReader,
    index: usize,
    cancel: CancellationToken,
) -> Result<Vec<u8>, String> {
    ffmpeg::init().map_err(|e| e.to_string())?;
    let mut input = open_input(reader, cancel.clone())?;
    let stream = input
        .stream(index)
        .ok_or("subtitle stream index is out of range")?;
    if stream.parameters().medium() != Type::Subtitle {
        return Err("selected stream is not a subtitle".into());
    }
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
        let display_duration = subtitle.end().saturating_sub(subtitle.start()) as f64 / 1000.0;
        // libavcodec leaves start/end_display_time at zero for Matroska ASS.
        // In that case the Matroska block duration carried by the packet is
        // authoritative. Treating the missing display duration as 1 ms made
        // otherwise valid embedded ASS cues effectively impossible to see.
        let end = start + subtitle_cue_duration(display_duration, packet_duration);
        let mut lines = Vec::new();
        let mut settings = "";
        for rect in subtitle.rects() {
            match rect {
                ffmpeg::subtitle::Rect::Text(value) => lines.push(value.get().to_string()),
                ffmpeg::subtitle::Rect::Ass(value) => {
                    if settings.is_empty() {
                        settings = decoded_ass_vtt_settings(value.get());
                    }
                    lines.push(strip_decoded_ass(value.get()));
                }
                _ => {}
            }
        }
        if !lines.is_empty() {
            output.push_str(&vtt_time(start));
            output.push_str(" --> ");
            output.push_str(&vtt_time(end));
            output.push_str(settings);
            output.push('\n');
            output.push_str(&lines.join("\n"));
            output.push_str("\n\n");
        }
        if output.len() > 16 << 20 {
            return Err("converted subtitle is too large".into());
        }
    }
    Ok(output.into_bytes())
}

fn srt_to_vtt(text: &str) -> String {
    let normalized = text.trim_start_matches('\u{feff}').replace("\r\n", "\n");
    let mut out = String::from("WEBVTT\n\n");
    for line in normalized.lines() {
        if line.contains(" --> ") {
            out.push_str(&line.replace(',', "."));
        } else {
            out.push_str(line);
        }
        out.push('\n');
    }
    out
}

fn ass_to_vtt(text: &str) -> String {
    let mut out = String::from("WEBVTT\n\n");
    let mut style_alignments = HashMap::new();
    let mut style_format: Vec<String> = Vec::new();
    let mut in_styles = false;
    let mut legacy_ssa_styles = false;
    for line in text.lines() {
        let trimmed = line.trim();
        if trimmed.starts_with('[') {
            legacy_ssa_styles = trimmed.eq_ignore_ascii_case("[V4 Styles]");
            in_styles = trimmed.eq_ignore_ascii_case("[V4+ Styles]") || legacy_ssa_styles;
        } else if in_styles {
            if let Some(value) = trimmed.strip_prefix("Format:") {
                style_format = value
                    .split(',')
                    .map(|v| v.trim().to_ascii_lowercase())
                    .collect();
            } else if let Some(value) = trimmed.strip_prefix("Style:") {
                let fields: Vec<_> = value.split(',').map(str::trim).collect();
                let name = style_format
                    .iter()
                    .position(|v| v == "name")
                    .and_then(|i| fields.get(i));
                let alignment = style_format
                    .iter()
                    .position(|v| v == "alignment")
                    .and_then(|i| fields.get(i))
                    .and_then(|v| v.parse::<u8>().ok());
                if let (Some(name), Some(alignment)) = (name, alignment) {
                    // Legacy SSA uses 5/6/7 for top and 9/10/11 for middle;
                    // normalize it to ASS's numpad-style alignment values.
                    let alignment = if legacy_ssa_styles {
                        match alignment {
                            5..=7 => alignment + 2,
                            9..=11 => alignment - 5,
                            _ => alignment,
                        }
                    } else {
                        alignment
                    };
                    style_alignments.insert(name.to_ascii_lowercase(), alignment);
                }
            }
        }
    }
    for line in text.lines() {
        let Some(raw) = line.strip_prefix("Dialogue:") else {
            continue;
        };
        let fields: Vec<_> = raw.trim().splitn(10, ',').collect();
        if fields.len() != 10 {
            continue;
        }
        let (Some(start), Some(end)) = (ass_time(fields[1]), ass_time(fields[2])) else {
            continue;
        };
        let (body, override_alignment) = clean_ass_text(fields[9]);
        if body.is_empty() {
            continue;
        }
        let alignment = override_alignment.or_else(|| {
            style_alignments
                .get(&fields[3].trim().to_ascii_lowercase())
                .copied()
        });
        out.push_str(&vtt_time(start));
        out.push_str(" --> ");
        out.push_str(&vtt_time(end));
        out.push_str(ass_vtt_settings(alignment));
        out.push('\n');
        out.push_str(&body);
        out.push_str("\n\n");
    }
    out
}

fn ass_time(value: &str) -> Option<f64> {
    let mut fields = value.trim().split(':');
    let hours = fields.next()?.parse::<f64>().ok()?;
    let minutes = fields.next()?.parse::<f64>().ok()?;
    let seconds = fields.next()?.parse::<f64>().ok()?;
    Some(hours * 3600.0 + minutes * 60.0 + seconds)
}

fn strip_ass(value: &str) -> String {
    let body = if value.starts_with("Dialogue:") {
        value.splitn(10, ',').nth(9).unwrap_or(value)
    } else {
        value
    };
    clean_ass_text(body).0
}

fn clean_ass_text(body: &str) -> (String, Option<u8>) {
    let mut result = String::new();
    let mut tag = false;
    let mut tag_body = String::new();
    let mut alignment = None;
    let mut drawing = false;
    for ch in body.chars() {
        match ch {
            '{' if !tag => {
                tag = true;
                tag_body.clear();
            }
            '}' if tag => {
                for candidate in 1..=9 {
                    if tag_body.contains(&format!(r"\an{candidate}")) {
                        alignment = Some(candidate);
                    }
                }
                for command in tag_body.split('\\').map(str::trim) {
                    if let Some(level) = command
                        .strip_prefix('p')
                        .and_then(|v| v.split_whitespace().next())
                        .and_then(|v| v.parse::<u8>().ok())
                    {
                        drawing = level > 0;
                    }
                }
                tag = false;
            }
            _ if tag => tag_body.push(ch),
            _ if drawing => {}
            '&' => result.push_str("&amp;"),
            '<' => result.push_str("&lt;"),
            '>' => result.push_str("&gt;"),
            _ => result.push(ch),
        }
    }
    (
        result
            .replace("\\N", "\n")
            .replace("\\n", "\n")
            .replace("\\h", " "),
        alignment,
    )
}

fn ass_vtt_settings(alignment: Option<u8>) -> &'static str {
    match alignment {
        Some(7..=9) => " line:10%",
        Some(4..=6) => " line:50%",
        _ => "",
    }
}

fn strip_decoded_ass(value: &str) -> String {
    // AVSubtitleRect::ass omits "Dialogue:" and the Start/End columns. Its
    // nine fields are ReadOrder, Layer, Style, Name, three margins, Effect,
    // and Text. Commas in Text must remain intact.
    let fields: Vec<_> = value.splitn(9, ',').collect();
    strip_ass(if fields.len() == 9 { fields[8] } else { value })
}

fn decoded_ass_vtt_settings(value: &str) -> &'static str {
    let fields: Vec<_> = value.splitn(9, ',').collect();
    ass_vtt_settings(clean_ass_text(if fields.len() == 9 { fields[8] } else { value }).1)
}

fn subtitle_cue_duration(display_duration: f64, packet_duration: f64) -> f64 {
    if display_duration.is_finite() && display_duration > 0.0 {
        display_duration
    } else if packet_duration.is_finite() && packet_duration > 0.0 {
        packet_duration
    } else {
        0.001
    }
}

fn vtt_time(value: f64) -> String {
    let milliseconds = (value.max(0.0) * 1000.0).round() as u64;
    format!(
        "{:02}:{:02}:{:02}.{:03}",
        milliseconds / 3_600_000,
        milliseconds / 60_000 % 60,
        milliseconds / 1000 % 60,
        milliseconds % 1000
    )
}


