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
      'ReaderView',
      'SelectionToolbar',
      'ShareDialog',
    ])
  })
})
