import { describe, expect, it } from 'vitest'
import App from './app-controller'

describe('app controller', () => {
  it('registers every component referenced by the external App template', () => {
    expect(Object.keys(App.components ?? {}).sort()).toEqual([
      'AppDialog',
      'AppTopbar',
      'DocumentEditor',
      'FileBrowserHeader',
      'FileGrid',
      'LoginPage',
      'MediaPreview',
      'MoveCopyDialog',
      'Reader',
      'SelectionToolbar',
      'ShareDialog',
    ])
  })

  it('starts batch downloads through prepare and a native anchor without buffering the ZIP', () => {
    const source = App.setup?.toString() ?? ''

    expect(source).toContain('/api/files/batch-download/prepare')
    expect(source).toContain('/api/files/batch-download/${encodeURIComponent(prepared.token)}')
    expect(source).not.toContain('response.blob')
    expect(source).not.toContain('ArrayBuffer')
    expect(source).not.toContain("createElement('iframe')")
  })
})
