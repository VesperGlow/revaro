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
  info_hash?:string
  name:string
  status:DownloadStatus
  selected_size:number
  completed_size:number
  download_speed:number
  peers:number
  error?:string
  created_at:string
  updated_at:string
  files?:DownloadFile[]
}
