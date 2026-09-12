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
export interface SystemStatusCacheClass {
  // External providers without hit/miss semantics leave these counters at 0.
  hits:number; misses:number; loads:number; load_errors:number; evictions:number
  memory_bytes?:number; memory_entries?:number; disk_bytes?:number; disk_entries?:number
}
export interface SystemStatus {
  status:'ok'|'degraded'
  database:{status:string;bytes:number}
  storage:{status:string;bytes:number;trash_bytes:number;file_count:number}
  cache:{status:string;memory_bytes:number;disk_bytes:number;memory_entries:number;disk_entries:number;classes?:Record<string,SystemStatusCacheClass>}
}
export interface TOTPStatusResponse { enabled:boolean; recovery_codes:number }
export interface TOTPSetupResponse { secret:string; uri:string; qr_data_url:string }
export interface TOTPRecoveryResponse { enabled:boolean; recovery_codes:string[] }
export interface AudioChapter { id:number; title:string; start:number; end:number }
export interface AudioMediaResponse { duration:number; chapters:AudioChapter[]; cover_url:string; has_cover:boolean }
export interface VideoSubtitleTrack { id:string; name:string; label:string; language:string; url:string; default?:boolean; forced?:boolean }
export interface VideoMediaResponse { subtitles:VideoSubtitleTrack[] }
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
