import { nextTick, onBeforeUnmount, onMounted, type Ref } from 'vue'

// Keep keyboard navigation inside the viewer, and return to the same file on exit.
export function usePreviewDialog(root: Ref<HTMLElement | null>, close: () => void) {
  let previous: HTMLElement | null = null
  let previousOverflow = ''
  function onKey(event: KeyboardEvent) {
    const el = root.value
    if (!el || event.defaultPrevented) return
    if (event.key === 'Escape') {
      // Native fullscreen consumes its own Escape before the viewer can close.
      if (document.fullscreenElement) return
      const menu = el.querySelector<HTMLDetailsElement>('details[open]')
      if (menu) {
        menu.open = false
        menu.querySelector('summary')?.focus()
      } else close()
      event.preventDefault()
      event.stopPropagation()
    }
    if (event.key !== 'Tab') return
    const sheet = el.querySelector<HTMLElement>('[data-preview-sheet]')
    const scope = sheet && getComputedStyle(sheet).position !== 'static' ? sheet : el
    const focusable = Array.from(scope.querySelectorAll<HTMLElement>('button:not(:disabled), summary, input:not(:disabled), select:not(:disabled), [tabindex="0"]'))
      .filter(node => node.getClientRects().length && !node.closest('[inert], [aria-hidden="true"]') && getComputedStyle(node).visibility !== 'hidden')
    const first = focusable[0], last = focusable.at(-1)
    if (!first) { event.preventDefault(); el.focus(); return }
    if (event.shiftKey && (document.activeElement === first || document.activeElement === el || !scope.contains(document.activeElement))) {
      event.preventDefault(); last?.focus()
    } else if (!event.shiftKey && (document.activeElement === last || document.activeElement === el || !scope.contains(document.activeElement))) {
      event.preventDefault(); first.focus()
    }
  }
  onMounted(() => {
    previous = document.activeElement instanceof HTMLElement ? document.activeElement : null
    previousOverflow = document.body.style.overflow
    document.body.style.overflow = 'hidden'
    void nextTick(() => root.value?.focus({ preventScroll: true }))
    root.value?.addEventListener('keydown', onKey)
  })
  onBeforeUnmount(() => {
    root.value?.removeEventListener('keydown', onKey)
    document.body.style.overflow = previousOverflow
    if (previous?.isConnected) previous.focus({ preventScroll: true })
  })
}
