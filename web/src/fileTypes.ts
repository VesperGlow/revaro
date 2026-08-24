import type { DriveFile } from './api'

export function isBook(item:DriveFile){return item.kind==='file'&&item.status==='ready'&&/\.(epub|txt)$/i.test(item.name)}
export function isEpub(item:DriveFile){return item.kind==='file'&&/\.epub$/i.test(item.name)}
export function isImage(item:DriveFile){return item.kind==='file'&&item.status==='ready'&&(['image/jpeg','image/png','image/webp','image/gif','image/avif'].includes(item.mime_type||'')||/\.(jpe?g|png|gif|webp|avif)$/i.test(item.name))}
export function isVideo(item:DriveFile){return item.kind==='file'&&item.status==='ready'&&((item.mime_type||'').startsWith('video/')||/\.(mp4|webm|ogv|mov|m4v|mkv|avi|flv|wmv|mpg|mpeg|ts|m2ts|mts)$/i.test(item.name))}
export function isAudio(item:DriveFile){return item.kind==='file'&&item.status==='ready'&&((item.mime_type||'').startsWith('audio/')||/\.(mp3|wav|ogg|oga|m4a|aac|flac|opus|wma|aiff?|ape)$/i.test(item.name))}
export function isMedia(item:DriveFile){return isImage(item)||isVideo(item)||isAudio(item)}
export function isArchive(item:DriveFile){return item.kind==='file'&&item.status==='ready'&&/\.(?:zip|7z|rar|tar|tar\.(?:gz|bz2|xz|zst)|tgz|tbz2?|txz|tzst|gz|bz2|xz|zst)$/i.test(item.name)}
export function isEditable(item:DriveFile){return item.kind==='file'&&item.status==='ready'&&item.size<=1024*1024&&/\.(md|markdown|txt|ya?ml|json|toml|ini|conf|log|csv)$/i.test(item.name)}
export function thumbSRC(item:DriveFile){return `/api/files/${item.id}/thumbnail?v=${encodeURIComponent(item.etag||'')}`}
export function previewURL(item:DriveFile){return `/api/files/${item.id}/preview`}
