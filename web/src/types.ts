export interface UploadTask {
  id:string
  file:File
  parentId:string
  relativePath?:string
  progress:number
  status:'queued'|'uploading'|'done'|'failed'|'cancelled'
  error:string
  cancelled:boolean
  uploadId?:string
  requests:XMLHttpRequest[]
}
export type TaskStatus='queued'|'running'|'waiting_input'|'retrying'|'completed'|'failed'|'cancelled'
export interface BackgroundTask { id:string;type:string;status:TaskStatus;phase:string;progress:number;speed:number;eta_seconds?:number;retry_count:number;max_retries:number;error?:string;source_type?:string;source_id?:string;cancel_requested:boolean;name:string;created_at:string;started_at?:string;finished_at?:string;updated_at:string }

export interface ShareResponse { active:boolean; url?:string; created_at?:string }
export interface ProfileResponse { username:string; has_avatar:boolean }
export interface StorageStats { total_bytes:number; file_count:number }
export interface SystemStatus {
  status:'ok'|'degraded'
  database:{status:string;bytes:number}
  storage:{status:string}
  cache:{status:string;memory_bytes:number;disk_bytes:number;memory_entries:number;disk_entries:number}
  tasks:{status:string;running:number;queued:number;waiting:number;failed:number}
  object_cleanup:{status:string;pending:number}
  media_sessions:{status:string;audio_hls:number;video_hls:number;fmp4:number}
  bt:{status:string;enabled:boolean;available:boolean}
  backup:{status:string;enabled:boolean}
}
export interface TOTPStatusResponse { enabled:boolean; recovery_codes:number }
export interface TOTPSetupResponse { secret:string; uri:string; qr_data_url:string }
export interface TOTPRecoveryResponse { enabled:boolean; recovery_codes:string[] }
export type AudioMergeFormat = 'flac'|'alac'|'aac'
export interface AudioChapter { id:number; title:string; start:number; end:number }
export interface AudioSubtitle { id:number; start:number; end:number; text:string }
export interface AudioMediaResponse { duration:number; chapters:AudioChapter[]; subtitles:AudioSubtitle[]; stream_url:string; cover_url:string; has_cover:boolean; stream_size:number }
export interface AudioHLSResponse { session_id:string; playlist_url:string; start:number }
export interface AudioMergeResponse {
  id:string
  status:'uploading'|'queued'|'preparing'|'merging'|'saving'|'cancelling'|'done'|'failed'|'cancelled'
  progress:number
  message:string
  error?:string
  output_name:string
  output_format:AudioMergeFormat
  output_file_id?:string
  parent_id:string
  input_count:number
  source?:'revaro'|'local'
  created_at:string
  updated_at:string
}

export interface LocalMergeFileInfo { name:string; size:number; kind:'audio'|'subtitle'|'cover'; chunk_count:number }
export interface LocalMergeCreateResponse extends AudioMergeResponse {
  chunk_size:number
  files:LocalMergeFileInfo[]
}
export interface LocalMergePick { file:File; name:string; size:number; kind:'audio'|'subtitle'|'cover'; preview?:string }

export type DownloadStatus = 'metadata'|'waiting'|'queued'|'downloading'|'paused'|'importing'|'done'|'failed'|'cancelled'
export type MediaIngestStatus = 'queued'|'probing'|'processing'|'uploading'|'completed'|'unsupported'|'failed'|'cancelled'
export interface DownloadFile {
  index:number
  path:string
  size:number
  selected:boolean
}
export interface DownloadJob {
  id:string
  parent_id:string
  source_type:'magnet'|'torrent'|'url'
  info_hash?:string
  name:string
  status:DownloadStatus
  ingest_state?:MediaIngestStatus
  selected_size:number
  completed_size:number
  download_speed:number
  imported_size:number
  import_speed:number
  current_file?:string
  peers:number
  error?:string
  created_at:string
  updated_at:string
  files?:DownloadFile[]
}

export interface VideoSubtitleTrack { id:string; name:string; label:string; language:string; url:string; default?:boolean; forced?:boolean }
export interface VideoMediaResponse { optimized?:boolean; playback_url?:string; playback_size?:number; playback_etag?:string; subtitles:VideoSubtitleTrack[] }
export interface VideoFMP4Metadata {
  duration:number
  mime_type:string
  aac_mime_type:string
  video_mime_type:string
  audio_mime_type?:string
  aac_audio_mime_type?:string
  video_codec:string
  audio_codec?:string
  width:number
  height:number
  bitrate:number
  frame_rate:number
}
export interface VideoFMP4Response extends VideoFMP4Metadata {
  session_id:string
  stream_url:string
  start:number
  requested_start:number
  output_audio_codec?:string
  audio_transcoding:boolean
  selected_mode:'mse-copy'|'mse-copy-video-aac-audio'
}
export interface VideoHLSResponse { session_id:string; playlist_url:string; start:number; duration:number; video_codec:string; audio_codec:string; transcoding:boolean }
export interface ArchiveJob {
  id:string
  file_id:string
  parent_id:string
  name:string
  status:'queued'|'downloading'|'checking'|'extracting'|'importing'|'waiting_password'|'done'|'failed'
  progress:number
  message:string
  output_id?:string
  output_name?:string
  error?:string
  created_at:string
  updated_at:string
}
