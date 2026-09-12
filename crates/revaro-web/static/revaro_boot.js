// The strict server CSP permits same-origin modules and rejects inline script.
// Keep this loader tiny: all application code remains in the Rust wasm module.
import init from './revaro_web.js'

init()
