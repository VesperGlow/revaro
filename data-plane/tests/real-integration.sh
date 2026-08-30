#!/usr/bin/env bash
set -euo pipefail

# Opt-in acceptance suite. It intentionally uses ffmpeg only to create and
# inspect fixtures; the Revaro process under test never invokes the CLI.
: "${DATA_PLANE_BIN:?set DATA_PLANE_BIN to revaro-data-plane}"
: "${S3_ENDPOINT:?set S3_ENDPOINT}"
: "${S3_ACCESS_KEY:?set S3_ACCESS_KEY}"
: "${S3_SECRET_KEY:?set S3_SECRET_KEY}"
: "${S3_BUCKET:?set S3_BUCKET}"

for tool in curl ffmpeg ffprobe 7z mc jq; do
  command -v "$tool" >/dev/null || { echo "missing integration dependency: $tool" >&2; exit 2; }
done

fixture_root=$(mktemp -d "${TMPDIR:-/tmp}/revaro-real-integration.XXXXXX")
work_root="$fixture_root/work"
mkdir -p "$work_root/hls-copy" "$work_root/hls-transcode" "$work_root/hls-long"
token=0123456789abcdef0123456789abcdef
addr=127.0.0.1:17081
server_pid=
cleanup() {
  if [[ -n "$server_pid" ]]; then kill -TERM "$server_pid" 2>/dev/null || true; wait "$server_pid" 2>/dev/null || true; fi
}
trap cleanup EXIT

start_server() {
  APP_WORK_DIR="$work_root" REVARO_DATA_PLANE_ADDR="$addr" REVARO_DATA_PLANE_TOKEN="$token" \
    S3_REGION="${S3_REGION:-us-east-1}" S3_ENDPOINT="$S3_ENDPOINT" S3_PUBLIC_ENDPOINT="$S3_ENDPOINT" \
    S3_ACCESS_KEY="$S3_ACCESS_KEY" S3_SECRET_KEY="$S3_SECRET_KEY" S3_BUCKET="$S3_BUCKET" \
    S3_PATH_STYLE="${S3_PATH_STYLE:-true}" S3_MULTIPART_STALE_SECONDS="${S3_MULTIPART_STALE_SECONDS:-0}" \
    "$DATA_PLANE_BIN" >>"$fixture_root/data-plane.log" 2>&1 &
  server_pid=$!
  for _ in $(seq 1 600); do
    curl -fsS -H "Authorization: Bearer $token" "http://$addr/v1/health" >/dev/null && return
    sleep .1
  done
  echo "data plane failed to become ready" >&2
  exit 1
}

request() { curl -fsS -H "Authorization: Bearer $token" -H 'Content-Type: application/json' "$@"; }
assert_probe() {
  ffprobe -v error -of json -show_streams -show_format -show_chapters "$1" >"$1.probe.json"
  jq -e "$2" "$1.probe.json" >/dev/null
}

ffmpeg -hide_banner -loglevel error -f lavfi -i testsrc2=size=640x360:rate=30000/1001 \
  -f lavfi -i sine=frequency=440:sample_rate=48000 -t 18 -c:v libx264 -threads 1 -preset ultrafast \
  -pix_fmt yuv420p -c:a aac "$fixture_root/h264-aac.mp4"
ffmpeg -hide_banner -loglevel error -f lavfi -i testsrc2=size=320x240:rate=25 \
  -f lavfi -i sine=frequency=330:sample_rate=44100 -t 20 -c:v libvpx-vp9 -threads 1 -b:v 500k \
  -c:a libopus "$fixture_root/vp9-opus.webm"
ffmpeg -hide_banner -loglevel error -f lavfi -i testsrc2=size=320x240:rate=24 \
  -f lavfi -i sine=frequency=550:sample_rate=48000 -t 12 -c:v libx265 \
  -x265-params pools=1:frame-threads=1 -pix_fmt yuv420p -c:a aac "$fixture_root/hevc-aac.mp4"
ffmpeg -hide_banner -loglevel error -f lavfi -i testsrc2=size=320x240:rate=24 \
  -f lavfi -i sine=frequency=660:sample_rate=44100 -t 24 -c:v libx265 \
  -x265-params pools=1:frame-threads=1 -pix_fmt yuv420p -c:a flac "$fixture_root/hevc-flac.mkv"
# Ten minutes at low resolution catches accumulated audio PTS drift cheaply.
ffmpeg -hide_banner -loglevel error -f lavfi -i testsrc2=size=160x90:rate=10 \
  -f lavfi -i sine=frequency=220:sample_rate=48000 -t 600 -c:v libvpx-vp9 -threads 1 -b:v 120k \
  -c:a libopus "$fixture_root/long-vp9-opus.webm"
ffmpeg -hide_banner -loglevel error -f lavfi -i sine=duration=3:sample_rate=48000 -ac 1 "$work_root/mono.wav"
ffmpeg -hide_banner -loglevel error -f lavfi -i sine=duration=4:sample_rate=44100 -ac 2 -c:a libmp3lame "$work_root/stereo.mp3"
ffmpeg -hide_banner -loglevel error -f lavfi -i anullsrc=channel_layout=5.1:sample_rate=48000 -t 3 "$work_root/surround.wav"
ffmpeg -hide_banner -loglevel error -f lavfi -i sine=duration=180:sample_rate=44100 -ac 1 "$work_root/long-mono.wav"

mc alias set revaro-it "$S3_ENDPOINT" "$S3_ACCESS_KEY" "$S3_SECRET_KEY" >/dev/null
for name in h264-aac.mp4 vp9-opus.webm hevc-aac.mp4 hevc-flac.mkv long-vp9-opus.webm; do
  mc cp --quiet "$fixture_root/$name" "revaro-it/$S3_BUCKET/integration/$name"
done
start_server

request -d '{"key":"integration/h264-aac.mp4","start_seconds":5,"include_audio":true,"transcode_video":false,"transcode_audio":false}' \
  "http://$addr/v1/media/fmp4" >"$fixture_root/copy.mp4"
assert_probe "$fixture_root/copy.mp4" '[.streams[].codec_name] == ["h264","aac"] and (.format.start_time|tonumber) < 0.1'
request -d '{"key":"integration/vp9-opus.webm","start_seconds":7,"include_audio":true,"transcode_video":true,"transcode_audio":true}' \
  "http://$addr/v1/media/fmp4" >"$fixture_root/transcoded.mp4"
assert_probe "$fixture_root/transcoded.mp4" '[.streams[].codec_name] == ["h264","aac"] and ((.streams[0].duration|tonumber)-(.streams[1].duration|tonumber)|fabs) < 0.1'
request -d '{"key":"integration/hevc-aac.mp4","start_seconds":3,"include_audio":true,"transcode_video":true,"transcode_audio":false}' \
  "http://$addr/v1/media/fmp4" >"$fixture_root/hevc-transcoded.mp4"
assert_probe "$fixture_root/hevc-transcoded.mp4" '[.streams[].codec_name] == ["h264","aac"] and (.format.start_time|tonumber) < 0.1'
request -d '{"key":"integration/hevc-flac.mkv","start_seconds":7,"include_audio":true,"transcode_video":false,"transcode_audio":true}' \
  "http://$addr/v1/media/fmp4" >"$fixture_root/hevc-flac-fmp4.mp4"
assert_probe "$fixture_root/hevc-flac-fmp4.mp4" '[.streams[].codec_name] == ["hevc","aac"] and ((.streams[0].duration|tonumber)-(.streams[1].duration|tonumber)|fabs) < 0.1'
request -d '{"key":"integration/long-vp9-opus.webm","start_seconds":300,"include_audio":true,"transcode_video":true,"transcode_audio":true}' \
  "http://$addr/v1/media/fmp4" >"$fixture_root/long.mp4"
assert_probe "$fixture_root/long.mp4" '((.streams[0].duration|tonumber)-(.streams[1].duration|tonumber)|fabs) < 0.1'
request -d "{\"key\":\"integration/vp9-opus.webm\",\"output_dir\":\"$work_root/hls-transcode\",\"start_seconds\":7}" \
  "http://$addr/v1/media/hls" >/dev/null
assert_probe "$work_root/hls-transcode/index.m3u8" '[.streams[].codec_name] == ["h264","aac"] and (.format.start_time|tonumber) < 0.1'
mkdir "$work_root/hls-hevc-flac"
hevc_hls=$(request -d "{\"key\":\"integration/hevc-flac.mkv\",\"output_dir\":\"$work_root/hls-hevc-flac\",\"start_seconds\":7}" \
  "http://$addr/v1/media/hls")
hevc_hls_job=$(jq -er .job_id <<<"$hevc_hls")
for _ in $(seq 1 400); do
  status=$(request "http://$addr/v1/media/hls/$hevc_hls_job")
  jq -e '.done == true' <<<"$status" >/dev/null && break
  sleep .1
done
jq -e '.done == true and (.error == null)' <<<"$status" >/dev/null
assert_probe "$work_root/hls-hevc-flac/index.m3u8" '[.streams[].codec_name] == ["h264","aac"] and (.format.duration|tonumber) > 12'

# Production scheduling regression: the long transcode keeps the single heavy
# permit, but startup returns after a playable prefix and probe uses a separate
# light pool. The HLS job remains alive without an HTTP generation deadline.
long_hls=$(request -d "{\"key\":\"integration/long-vp9-opus.webm\",\"output_dir\":\"$work_root/hls-long\",\"start_seconds\":0}" \
  "http://$addr/v1/media/hls")
long_hls_job=$(jq -er .job_id <<<"$long_hls")
probe_started=$(date +%s)
request -d '{"key":"integration/hevc-aac.mp4"}' "http://$addr/v1/media/probe" >"$fixture_root/concurrent-probe.json"
probe_elapsed=$(($(date +%s)-probe_started))
test "$probe_elapsed" -lt 5
jq -e '.video_codec == "hevc" and .duration_ms > 0' "$fixture_root/concurrent-probe.json" >/dev/null
request "http://$addr/v1/media/hls/$long_hls_job" | jq -e '.error == null' >/dev/null
request -X DELETE "http://$addr/v1/media/hls/$long_hls_job" >/dev/null

request -d "{\"inputs\":[\"$work_root/mono.wav\",\"$work_root/mono.wav\"],\"input_names\":[\"mono original.wav\",\"mono second.wav\"],\"output\":\"$work_root/mono.m4a\",\"format\":\"aac\"}" \
  "http://$addr/v1/media/audio/merge" >/dev/null
assert_probe "$work_root/mono.m4a" '.streams[0].channels == 1 and .chapters[0].tags.title == "mono original.wav"'
ffmpeg -hide_banner -loglevel error -f lavfi -i color=c=blue:s=320x240 -frames:v 1 "$work_root/cover.jpg"
printf 'WEBVTT\n\n00:00:00.020 --> 00:00:00.300\nembedded fixture\n' >"$work_root/subtitles.vtt"
request -d "{\"input\":\"$work_root/mono.m4a\",\"cover\":\"$work_root/cover.jpg\",\"subtitle\":\"$work_root/subtitles.vtt\"}" \
  "http://$addr/v1/media/audio/decorate" >/dev/null
assert_probe "$work_root/mono.m4a" 'any(.streams[]; .codec_name == "mov_text") and any(.streams[]; .codec_name == "mjpeg" and .disposition.attached_pic == 1) and .chapters[0].tags.title == "mono original.wav"'
request -d "{\"inputs\":[\"$work_root/surround.wav\",\"$work_root/surround.wav\"],\"input_names\":[\"surround A.wav\",\"surround B.wav\"],\"output\":\"$work_root/surround.m4a\",\"format\":\"aac\"}" \
  "http://$addr/v1/media/audio/merge" >/dev/null
assert_probe "$work_root/surround.m4a" '.streams[0].channels == 6 and .chapters[1].tags.title == "surround B.wav"'
request -d "{\"inputs\":[\"$work_root/stereo.mp3\",\"$work_root/mono.wav\"],\"input_names\":[\"stereo 44.1k MP3\",\"mono 48k WAV\"],\"output\":\"$work_root/mixed-aac.m4a\",\"format\":\"aac\"}" \
  "http://$addr/v1/media/audio/merge" >/dev/null
assert_probe "$work_root/mixed-aac.m4a" '.streams[0].codec_name == "aac" and .streams[0].sample_rate == "48000" and .streams[0].channels == 2'
request -d "{\"inputs\":[\"$work_root/mono.wav\",\"$work_root/mono.wav\"],\"input_names\":[\"mono A\",\"mono B\"],\"output\":\"$work_root/merged-alac.m4a\",\"format\":\"alac\"}" \
  "http://$addr/v1/media/audio/merge" >/dev/null
assert_probe "$work_root/merged-alac.m4a" '.streams[0].codec_name == "alac" and .streams[0].channels == 1'
request -d "{\"inputs\":[\"$work_root/surround.wav\",\"$work_root/surround.wav\"],\"input_names\":[\"5.1 A\",\"5.1 B\"],\"output\":\"$work_root/merged-flac.flac\",\"format\":\"flac\"}" \
  "http://$addr/v1/media/audio/merge" >/dev/null
assert_probe "$work_root/merged-flac.flac" '.streams[0].codec_name == "flac" and .streams[0].channels == 6'
request -d "{\"inputs\":[\"$work_root/long-mono.wav\",\"$work_root/long-mono.wav\"],\"input_names\":[\"long 44.1k WAV A\",\"long 44.1k WAV B\"],\"output\":\"$work_root/long-aac.m4a\",\"format\":\"aac\"}" \
  "http://$addr/v1/media/audio/merge" >/dev/null
assert_probe "$work_root/long-aac.m4a" '.streams[0].codec_name == "aac" and (.format.duration|tonumber) > 359.9'

# Exercise streaming upload and exact S3 byte ranges independently of libav.
head -c $((6 * 1024 * 1024)) /dev/urandom >"$fixture_root/s3-stream.bin"
request -X PUT --data-binary @"$fixture_root/s3-stream.bin" \
  "http://$addr/v1/s3/object?key=integration%2Fs3-stream.bin&size=$((6 * 1024 * 1024))" >/dev/null
request "http://$addr/v1/s3/object?key=integration%2Fs3-stream.bin&start=1048576&end=2097151" >"$fixture_root/s3-range.bin"
cmp "$fixture_root/s3-range.bin" <(dd if="$fixture_root/s3-stream.bin" bs=1 skip=1048576 count=1048576 status=none)

multipart=$(request -d '{"key":"integration/completed-multipart.bin"}' "http://$addr/v1/s3/multipart" | jq -r .upload_id)
part=$(request -X PUT --data-binary @"$fixture_root/s3-stream.bin" \
  "http://$addr/v1/s3/multipart/upload?key=integration%2Fcompleted-multipart.bin&upload_id=$(printf %s "$multipart" | jq -sRr @uri)&part_number=1&size=$((6 * 1024 * 1024))")
etag=$(jq -r .etag <<<"$part")
request -X PUT -d "{\"key\":\"integration/completed-multipart.bin\",\"upload_id\":\"$multipart\",\"parts\":[{\"part_number\":1,\"etag\":\"$etag\"}]}" "http://$addr/v1/s3/multipart" >/dev/null
request "http://$addr/v1/s3/object?key=integration%2Fcompleted-multipart.bin" >"$fixture_root/multipart-result.bin"
cmp "$fixture_root/s3-stream.bin" "$fixture_root/multipart-result.bin"

mkdir "$fixture_root/archive-input"
printf 'archive fixture\n' >"$fixture_root/archive-input/file.txt"
7z a -bd -bso0 -tzip -mem=ZipCrypto -psecret "$fixture_root/password.zip" "$fixture_root/archive-input/file.txt"
mc cp --quiet "$fixture_root/password.zip" "revaro-it/$S3_BUCKET/integration/password.zip"
archive_size=$(wc -c <"$fixture_root/password.zip")
if request -d "{\"job_id\":\"password-retry\",\"key\":\"integration/password.zip\",\"archive_size\":$archive_size}" "http://$addr/v1/archive/extract"; then
  echo 'password archive unexpectedly extracted without password' >&2; exit 1
fi
kill -TERM "$server_pid"; wait "$server_pid"; server_pid=
start_server
request -d "{\"job_id\":\"password-retry\",\"key\":\"integration/password.zip\",\"archive_size\":$archive_size,\"password\":\"secret\"}" "http://$addr/v1/archive/extract" >/dev/null
test -f "$work_root/revaro-extract-password-retry/output/file.txt"

dd if=/dev/zero of="$fixture_root/cancel.bin" bs=1M count=64 status=none
7z a -bd -bso0 -mx=0 "$fixture_root/cancel.7z" "$fixture_root/cancel.bin"
mc cp --quiet "$fixture_root/cancel.7z" "revaro-it/$S3_BUCKET/integration/cancel.7z"
cancel_size=$(wc -c <"$fixture_root/cancel.7z")
request -d "{\"job_id\":\"cancel-download\",\"key\":\"integration/cancel.7z\",\"archive_size\":$cancel_size}" "http://$addr/v1/archive/extract" >"$fixture_root/cancel-response" 2>&1 &
archive_pid=$!
sleep .05
request -X POST -d '{}' "http://$addr/v1/archive/cancel-download/cancel" >/dev/null
if wait "$archive_pid"; then echo 'cancelled archive unexpectedly completed' >&2; exit 1; fi
test ! -e "$work_root/revaro-extract-cancel-download/source.partial"

# A client-created multipart abandoned by a crash is removed on restart.
orphan=$(request -d '{"key":"integration/orphan.bin"}' "http://$addr/v1/s3/multipart" | jq -r .upload_id)
test -n "$orphan"
cancelled=$(request -d '{"key":"integration/cancelled-multipart.bin"}' "http://$addr/v1/s3/multipart" | jq -r .upload_id)
request -X DELETE -d "{\"key\":\"integration/cancelled-multipart.bin\",\"upload_id\":\"$cancelled\"}" "http://$addr/v1/s3/multipart" >/dev/null
kill -TERM "$server_pid"; wait "$server_pid"; server_pid=
start_server
sleep .2
if mc ls --incomplete "revaro-it/$S3_BUCKET/integration/" | grep -q orphan.bin; then
  echo 'stale multipart upload survived restart cleanup' >&2; exit 1
fi

if grep -Eqi 'more samples than frame size|Qavg: nan|unexpected EOF' "$fixture_root/data-plane.log"; then
  echo 'audio encoder regression signature found in data-plane log' >&2
  grep -Ei 'more samples than frame size|Qavg: nan|unexpected EOF' "$fixture_root/data-plane.log" >&2
  exit 1
fi

echo "real data-plane integration fixtures passed ($fixture_root)"
