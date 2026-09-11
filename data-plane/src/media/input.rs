fn millis(value: i64, base: Rational) -> i64 {
    if value < 0 { return 0; }
    let numerator = i128::from(value) * i128::from(base.numerator()) * 1000;
    let denominator = i128::from(base.denominator()).max(1);
    (numerator / denominator).clamp(0, i128::from(i64::MAX)) as i64
}

fn open_input(reader: std::fs::File, cancel: CancellationToken) -> Result<ffmpeg::format::context::Input, String> {
    let io = StreamIo::from_read_seek_with_capacity(reader, 256 << 10).map_err(|e| e.to_string())?;
    ffmpeg::format::input_from_stream_with_interrupt(io, None, None, move || cancel.is_cancelled())
        .map_err(|e| format!("open media: {e}"))
}
