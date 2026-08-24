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

export interface FolderOption { id:string; name:string; depth:number }
export interface ShareResponse { active:boolean; url?:string; created_at?:string }
export interface ProfileResponse { username:string; has_avatar:boolean }
export interface StorageStats { total_bytes:number; file_count:number }
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
  status:'queued'|'preparing'|'merging'|'saving'|'cancelling'|'done'|'failed'|'cancelled'
  progress:number
  message:string
  error?:string
  output_name:string
  output_format:AudioMergeFormat
  output_file_id?:string
  parent_id:string
  input_count:number
  created_at:string
  updated_at:string
}

export type DownloadStatus = 'metadata'|'waiting'|'queued'|'downloading'|'paused'|'importing'|'done'|'failed'|'cancelled'
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

export interface VideoSubtitleTrack { id:string; name:string; label:string; language:string; url:string }
export interface VideoMediaResponse { subtitles:VideoSubtitleTrack[] }
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
  init_url:string
  index_url:string
  start:number
  requested_start:number
  output_audio_codec?:string
  audio_transcoding:boolean
  selected_mode:'mse-copy'|'mse-copy-video-aac-audio'
}
export interface VideoFMP4Fragment { number:number;start:number;duration:number;url:string }
export interface VideoFMP4Index { fragments:VideoFMP4Fragment[];available_until:number;done:boolean;error?:string }
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
