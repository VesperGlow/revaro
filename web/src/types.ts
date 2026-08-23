export interface UploadTask {
  id:string
  file:File
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
export interface AudioMediaResponse { duration:number; chapters:AudioChapter[]; stream_url:string; cover_url:string; has_cover:boolean; stream_size:number }
export interface AudioMergeResponse {
  id:string
  status:'queued'|'preparing'|'merging'|'saving'|'cancelling'|'done'|'failed'|'cancelled'
  progress:number
  message:string
  error?:string
  output_name:string
  output_format:AudioMergeFormat
  output_file_id?:string
  input_count:number
  created_at:string
  updated_at:string
}
